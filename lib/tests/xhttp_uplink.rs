use bytes::{BufMut, Bytes, BytesMut};
use hyper::body::HttpBody;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use trusttunnel::settings::{
    Http1Settings, Http2Settings, ListenProtocolSettings, MetricsSettings, Settings, TlsHostInfo,
    TlsHostsSettings,
};

#[allow(dead_code)]
mod common;

/// The wire frame header: the type, the offset, the length.
const FRAME_HEADER_LEN: usize = 11;
/// The frames on the wire.
const FRAME_OPEN: u8 = 0x01;
const FRAME_DATA: u8 = 0x02;
const FRAME_ACK: u8 = 0x03;
const FRAME_RESUME: u8 = 0x07;
const FRAME_FIN: u8 = 0x04;

/// The upload checked by the task is three megabytes.
const UPLINK_TOTAL: usize = 3 * 1024 * 1024;
/// The client window: synchronised with the APK core (the default 1 MiB).
const CLIENT_WINDOW: usize = 1024 * 1024;
/// The maximal payload fitting into the short length field of the frame.
const DATA_CHUNK: usize = 16 * 1024;
/// How long the test waits for the corresponding confirmation.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

fn encode_frame(kind: u8, offset: u64, payload: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(FRAME_HEADER_LEN + payload.len());
    buf.put_u8(kind);
    buf.put_u64(offset);
    if payload.len() <= 0x7fff {
        buf.put_u16(payload.len() as u16);
    } else {
        buf.put_u16(0x8000 | ((payload.len() >> 16) as u16));
        buf.put_u32(payload.len() as u32);
    }
    buf.put_slice(payload);
    buf.freeze()
}

/// A receiver which reads everything and never answers: the downlink;
/// the standard configuration of the field tests: the upload alone.
async fn spawn_discarder(listener: TcpListener, received: Arc<AtomicUsize>) {
    while let Ok((mut sock, _)) = listener.accept().await {
        let received = received.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        received.fetch_add(n, Ordering::SeqCst);
                    }
                }
            }
        });
    }
}

async fn spawn_endpoint(endpoint_address: SocketAddr) {
    let settings = Settings::builder()
        .listen_address(endpoint_address)
        .unwrap()
        .listen_protocols(ListenProtocolSettings {
            http1: None,
            http2: Some(Http2Settings::builder().build()),
            quic: None,
        })
        .xhttp_enable(true)
        .xhttp_path("/xh/")
        .allow_private_network_connections(true)
        .build()
        .unwrap();

    spawn_endpoint_settings(settings).await;
}

async fn spawn_endpoint_settings(settings: Settings) {
    let cert_key_file = common::make_cert_key_file();
    let cert_key_path = cert_key_file.path.to_str().unwrap().to_string();
    let hosts_settings = TlsHostsSettings::builder()
        .main_hosts(vec![TlsHostInfo {
            hostname: common::MAIN_DOMAIN_NAME.to_string(),
            cert_chain_path: cert_key_path.clone(),
            private_key_path: cert_key_path,
            allowed_sni: vec![],
        }])
        .build()
        .unwrap();

    let mut cert_key_file = cert_key_file;
    let task = async move {
        let _keep_cert = &mut cert_key_file;
        common::run_endpoint_with_settings(settings, hosts_settings).await;
    };
    tokio::spawn(task);
}

/// Take the frames out of the body of the downlink; the body is frames one
/// after another, so the parser is stateful.
struct DownParser {
    buffer: BytesMut,
}

impl DownParser {
    fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// Return the ACK offset when the next frame turns out to be one.
    fn next_ack(&mut self) -> Option<u64> {
        loop {
            let Some(head) = self.buffer.get(..FRAME_HEADER_LEN) else {
                return None;
            };
            let kind = head[0];
            let offset = u64::from_be_bytes(head[1..9].try_into().unwrap());
            let short_len = u16::from_be_bytes(head[9..11].try_into().unwrap());
            let (frame_len, header_len) = if short_len & 0x8000 != 0 {
                let Some(long) = self.buffer.get(..FRAME_HEADER_LEN + 4) else {
                    return None;
                };
                let len = u32::from_be_bytes(long[11..15].try_into().unwrap());
                (len as usize, FRAME_HEADER_LEN + 4)
            } else {
                (short_len as usize, FRAME_HEADER_LEN)
            };
            if self.buffer.len() < header_len + frame_len {
                return None;
            }
            self.buffer = self.buffer.split_off(header_len + frame_len);
            if kind == FRAME_ACK {
                return Some(offset);
            }
        }
    }
}

/// The screen of the client: three megabytes which go out of a window of
/// one megabyte at a time; each window waits for the confirmation in the
/// downlink. Without the confirmation no further bytes go: that is exactly
/// how the APK core acts.
#[tokio::test]
async fn uplink_3mb_goes_out_of_the_client_window() {
    common::set_up_logger();

    // The destination: everything is read and is not answered.
    let dest = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let dest_port = dest.local_addr().unwrap().port();
    let received = Arc::new(AtomicUsize::new(0));
    tokio::spawn(spawn_discarder(dest, received.clone()));

    let endpoint_address = common::make_endpoint_address();
    spawn_endpoint(endpoint_address).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let tls =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &endpoint_address, Some(b"h2"))
            .await;
    let (mut sender, conn) = hyper::client::conn::Builder::new()
        .http2_only(true)
        .handshake(tls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let sid = "oc39windowtestoc39";
    let base = format!("https://{}/xh/{}", common::MAIN_DOMAIN_NAME, sid);

    // The downlink: the streaming body of the GET from which the ACKs are read.
    let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let get_future = sender.send_request(
        http::Request::builder()
            .method(http::Method::GET)
            .uri(&base)
            .header("x-xhttp-version", "1")
            .body(hyper::Body::empty())
            .unwrap(),
    );
    let down_task = tokio::spawn(async move {
        let response = get_future.await.expect("the GET has been answered");
        assert_eq!(response.status(), hyper::StatusCode::OK);
        let mut body = response.into_body();
        while let Some(chunk) = body.data().await {
            if ack_tx.send(chunk.unwrap()).is_err() {
                return;
            }
        }
    });

    // The uplink: the POST with the body written by hand.
    let (mut up_tx, up_body) = hyper::Body::channel();
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let post_future = sender.send_request(
        http::Request::builder()
            .method(http::Method::POST)
            .uri(&base)
            .header("x-xhttp-version", "1")
            .header("content-type", "application/octet-stream")
            .body(up_body)
            .unwrap(),
    );
    let up_status = tokio::spawn(async move {
        post_future
            .await
            .expect("the POST has been answered")
            .status()
    });

    // The first frame in the uplink is always OPEN with the destination.
    up_tx
        .send_data(Bytes::from(encode_frame(
            FRAME_OPEN,
            0,
            format!("127.0.0.1:{dest_port}").as_bytes(),
        )))
        .await
        .unwrap();

    // The pairing of the session: the POST receives the headers of the answer.
    let status = tokio::time::timeout(Duration::from_secs(30), up_status)
        .await
        .expect("the POST has not been answered")
        .unwrap();
    assert_eq!(status, hyper::StatusCode::OK);

    // The client acts as the APK core: a DATA frame is accepted only while the
    // unconfirmed uplink is below the window; otherwise it waits for an ACK.
    // The downlink carries nothing but the ACKs of the endpoint.
    let mut parser = DownParser::new();
    let mut acked: u64 = 0;
    let mut sent: usize = 0;
    while sent < UPLINK_TOTAL {
        if (sent as u64) - acked < CLIENT_WINDOW as u64 {
            let chunk_len = (UPLINK_TOTAL - sent).min(DATA_CHUNK);
            let payload = vec![0xABu8; chunk_len];
            up_tx
                .send_data(encode_frame(FRAME_DATA, sent as u64, &payload))
                .await
                .unwrap();
            sent += chunk_len;
            while let Ok(chunk) = ack_rx.try_recv() {
                parser.push(&chunk);
                while let Some(x) = parser.next_ack() {
                    acked = x.max(acked);
                }
            }
            continue;
        }

        // The window is full: the client stands still until the endpoint
        // confirms something. Without the fix nothing ever comes.
        let chunk = tokio::time::timeout(ACK_TIMEOUT, ack_rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "the uplink stalled: sent={sent} acked={acked}, no ACK within {ACK_TIMEOUT:?}"
                )
            })
            .expect("the downlink has ended");
        parser.push(&chunk);
        while let Some(x) = parser.next_ack() {
            acked = x.max(acked);
        }
    }

    // Everything has reached the destination.
    let start = Instant::now();
    while received.load(Ordering::SeqCst) < UPLINK_TOTAL {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the destination got only {} of {} bytes",
            received.load(Ordering::SeqCst),
            UPLINK_TOTAL
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The uplink is over.
    up_tx
        .send_data(encode_frame(FRAME_FIN, sent as u64, &[]))
        .await
        .unwrap();
    drop(up_tx);

    // The last ACK may still be on its way.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while acked + 64 * 1024 <= UPLINK_TOTAL as u64 {
        match tokio::time::timeout_at(deadline, ack_rx.recv()).await {
            Ok(Some(chunk)) => {
                parser.push(&chunk);
                while let Some(x) = parser.next_ack() {
                    acked = x.max(acked);
                }
            }
            _ => break,
        }
    }

    // A tail below the threshold may stay unconfirmed: nothing waits for it.
    assert!(
        acked + 64 * 1024 > UPLINK_TOTAL as u64,
        "the uplink is not confirmed: acked={acked}"
    );
    down_task.abort();
}

/// The byte of the test stream at the position `i`: any shift or hole in the
/// stream is visible in the content, not only in the length.
fn pattern(i: u64) -> u8 {
    (i.wrapping_mul(2654435761) >> 13) as u8
}

/// The frames of the downlink with the payload kept: the parser is stateful.
struct FrameCollector {
    buffer: BytesMut,
}

impl FrameCollector {
    fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    fn next_frame(&mut self) -> Option<(u8, u64, Bytes)> {
        let head = self.buffer.get(..FRAME_HEADER_LEN)?;
        let kind = head[0];
        let offset = u64::from_be_bytes(head[1..9].try_into().unwrap());
        let short_len = u16::from_be_bytes(head[9..11].try_into().unwrap());
        let (len, header_len) = if short_len & 0x8000 != 0 {
            let long = self.buffer.get(..FRAME_HEADER_LEN + 4)?;
            (
                u32::from_be_bytes(long[11..15].try_into().unwrap()) as usize,
                FRAME_HEADER_LEN + 4,
            )
        } else {
            (short_len as usize, FRAME_HEADER_LEN)
        };
        if self.buffer.len() < header_len + len {
            return None;
        }
        let mut frame = self.buffer.split_to(header_len + len);
        let payload = frame.split_off(header_len).freeze();
        Some((kind, offset, payload))
    }
}

/// One pair of streams of a session over its own connection: the receiver of
/// the downlink chunks, the writer of the uplink and the task with the status
/// of the uplink answer.
async fn open_halves(
    endpoint_address: &SocketAddr,
    sid: &str,
    first_frame: Bytes,
    resume_key: Option<&[u8]>,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<Bytes>,
    hyper::body::Sender,
    tokio::task::JoinHandle<hyper::StatusCode>,
) {
    let tls =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, endpoint_address, Some(b"h2"))
            .await;
    let (mut sender, conn) = hyper::client::conn::Builder::new()
        .http2_only(true)
        .handshake(tls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let base = format!("https://{}/xh/{}", common::MAIN_DOMAIN_NAME, sid);

    let (down_tx, down_rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let get_future = sender.send_request(
        http::Request::builder()
            .method(http::Method::GET)
            .uri(&base)
            .header("x-xhttp-version", "1")
            .body(hyper::Body::empty())
            .unwrap(),
    );
    let (challenge_tx, challenge_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok(response) = get_future.await else {
            return;
        };
        let challenge = response.headers().get("x-xhttp-challenge").cloned();
        let _ = challenge_tx.send(challenge);
        let mut body = response.into_body();
        while let Some(Ok(chunk)) = body.data().await {
            if down_tx.send(chunk).is_err() {
                return;
            }
        }
    });

    let (mut up_tx, up_body) = hyper::Body::channel();
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let post_future = sender.send_request(
        http::Request::builder()
            .method(http::Method::POST)
            .uri(&base)
            .header("x-xhttp-version", "1")
            .header("content-type", "application/octet-stream")
            .body(up_body)
            .unwrap(),
    );
    let status = tokio::spawn(async move { post_future.await.unwrap().status() });
    let first_frame = if let Some(key) = resume_key {
        use base64::Engine;
        let challenge = tokio::time::timeout(Duration::from_secs(5), challenge_rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let challenge = base64::engine::general_purpose::STANDARD
            .decode(challenge.as_bytes())
            .unwrap();
        let offsets = &first_frame[FRAME_HEADER_LEN..];
        let mut message = b"cattunnel-xhttp-resume-v1".to_vec();
        message.extend_from_slice(sid.as_bytes());
        message.extend_from_slice(offsets);
        message.extend_from_slice(&challenge);
        let proof = ring::hmac::sign(
            &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key),
            &message,
        );
        let mut payload = offsets.to_vec();
        payload.extend_from_slice(proof.as_ref());
        encode_frame(FRAME_RESUME, 0, &payload)
    } else {
        first_frame
    };
    up_tx.send_data(first_frame).await.unwrap();
    (down_rx, up_tx, status)
}

/// The client does not confirm the downlink, the endpoint stops at the limit
/// of the unconfirmed data with a chunk of the destination in its hands, the
/// old connection is cancelled by the resume. The chunk must not be lost: the
/// repeated stream is the same bytes one after another, without a hole.
#[tokio::test]
async fn downlink_cancelled_at_the_limit_repeats_all_bytes() {
    const TOTAL: u64 = 4 * 1024 * 1024;
    common::set_up_logger();

    // The destination: a known stream, which then stays open.
    let dest = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let dest_port = dest.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Ok((mut sock, _)) = dest.accept().await {
            tokio::spawn(async move {
                let mut position = 0u64;
                while position < TOTAL {
                    let piece: Vec<u8> = (position..position + 16 * 1024).map(pattern).collect();
                    if sock.write_all(&piece).await.is_err() {
                        return;
                    }
                    position += piece.len() as u64;
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }
    });

    let endpoint_address = common::make_endpoint_address();
    spawn_endpoint(endpoint_address).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let sid = "oc74canceltestoc74";
    let open = encode_frame(FRAME_OPEN, 0, format!("127.0.0.1:{dest_port}").as_bytes());
    let (mut old_down, _old_up, old_status) = open_halves(&endpoint_address, sid, open, None).await;
    let status = tokio::time::timeout(Duration::from_secs(30), old_status)
        .await
        .expect("the POST has not been answered")
        .unwrap();
    assert_eq!(status, hyper::StatusCode::OK);

    // The old downlink is read and never confirmed: the endpoint stops at the
    // limit. It is quiet for a second when it stands at the limit.
    let mut collector = FrameCollector::new();
    let mut old_received = 0u64;
    let mut resume_key = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(1), old_down.recv()).await {
            Ok(Some(chunk)) => {
                collector.push(&chunk);
                while let Some((kind, _, payload)) = collector.next_frame() {
                    if kind == 0x0c {
                        resume_key = payload.to_vec();
                    }
                    if kind == FRAME_DATA {
                        old_received += payload.len() as u64;
                    }
                }
            }
            Ok(None) => panic!("the old downlink has ended"),
            Err(_) => break,
        }
    }
    assert!(
        old_received >= 1024 * 1024 && old_received < TOTAL,
        "the endpoint has not stopped at the limit: {old_received}"
    );

    assert_eq!(resume_key.len(), 32);
    // The resume: the client says it has received nothing, so the whole buffer
    // is repeated. The first frame of the new uplink is RESUME.
    let mut resume_payload = [0u8; 16];
    resume_payload[..8].copy_from_slice(&0u64.to_be_bytes());
    resume_payload[8..].copy_from_slice(&0u64.to_be_bytes());
    let resume = encode_frame(FRAME_RESUME, 0, &resume_payload);
    let (mut new_down, mut new_up, new_status) =
        open_halves(&endpoint_address, sid, resume, Some(&resume_key)).await;
    let status = tokio::time::timeout(Duration::from_secs(30), new_status)
        .await
        .expect("the resume POST has not been answered")
        .unwrap();
    assert_eq!(status, hyper::StatusCode::OK);

    // The new downlink is checked byte by byte against the stream; the client
    // confirms what it has taken, so the endpoint goes on past the limit.
    let want = old_received + 512 * 1024;
    let mut collector = FrameCollector::new();
    let mut position = 0u64;
    let mut confirmed = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while position < want {
        let chunk = tokio::time::timeout_at(deadline, new_down.recv())
            .await
            .unwrap_or_else(|_| panic!("the repeat stalled at {position} of {want}"))
            .expect("the new downlink has ended");
        collector.push(&chunk);
        while let Some((kind, offset, payload)) = collector.next_frame() {
            if kind != FRAME_DATA {
                continue;
            }
            assert_eq!(offset, position, "the frame is not where the stream is");
            for (i, byte) in payload.iter().enumerate() {
                assert_eq!(
                    *byte,
                    pattern(position + i as u64),
                    "the stream has a hole or a shift at {}",
                    position + i as u64
                );
            }
            position += payload.len() as u64;
        }
        if position - confirmed >= 256 * 1024 {
            confirmed = position;
            new_up
                .send_data(encode_frame(FRAME_ACK, confirmed, &[]))
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn endpoint_rejects_missing_or_unknown_version_before_pairing() {
    let address = common::make_endpoint_address();
    spawn_endpoint(address).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tls =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &address, Some(b"h2")).await;
    let (mut sender, connection) = hyper::client::conn::Builder::new()
        .http2_only(true)
        .handshake(tls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    for versions in [
        vec![],
        vec!["0"],
        vec!["2"],
        vec!["3"],
        vec!["01"],
        vec!["1,2"],
        vec!["abc"],
        vec!["1", "1"],
        vec!["1", "2"],
        vec!["2", "1"],
    ] {
        let mut request = http::Request::builder().method("GET").uri(format!(
            "https://{}/xh/strict-test",
            common::MAIN_DOMAIN_NAME
        ));
        for version in versions {
            request = request.header("x-xhttp-version", version);
        }
        let request = request.body(hyper::Body::empty()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(5), sender.send_request(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn protocol1_resume_proof_preserves_active_stream_on_failure_and_replays_on_same_h2() {
    use base64::Engine;
    use tokio::io::AsyncWriteExt;
    let destination = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = destination.local_addr().unwrap().port();
    let more = Arc::new(tokio::sync::Notify::new());
    let send_more = more.clone();
    let peer_task = tokio::spawn(async move {
        let (mut socket, _) = destination.accept().await.unwrap();
        socket.write_all(b"abcdef").await.unwrap();
        send_more.notified().await;
        socket.write_all(b"ghij").await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let address = common::make_endpoint_address();
    spawn_endpoint(address).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tls =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &address, Some(b"h2")).await;
    let (mut sender, connection) = hyper::client::conn::Builder::new()
        .http2_only(true)
        .handshake(tls)
        .await
        .unwrap();
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let sid = "proof-integration";
    let uri = format!("https://{}/xh/{sid}", common::MAIN_DOMAIN_NAME);
    let get = http::Request::builder()
        .method("GET")
        .uri(&uri)
        .header("x-xhttp-version", "1")
        .body(hyper::Body::empty())
        .unwrap();
    let down = sender.send_request(get);
    let (mut old_up, body) = hyper::Body::channel();
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let post = sender.send_request(
        http::Request::builder()
            .method("POST")
            .uri(&uri)
            .header("x-xhttp-version", "1")
            .body(body)
            .unwrap(),
    );
    old_up
        .send_data(encode_frame(
            FRAME_OPEN,
            0,
            format!("127.0.0.1:{port}").as_bytes(),
        ))
        .await
        .unwrap();
    let response = tokio::time::timeout(ACK_TIMEOUT, down)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.headers()["x-xhttp-version"], "1");
    let mut old_down = response.into_body();
    assert_eq!(
        tokio::time::timeout(ACK_TIMEOUT, post)
            .await
            .unwrap()
            .unwrap()
            .status(),
        http::StatusCode::OK
    );
    let mut parser = FrameCollector::new();
    let mut token = None;
    let mut received = Vec::new();
    let deadline = tokio::time::Instant::now() + ACK_TIMEOUT;
    while token.is_none() || received.len() < 6 {
        let chunk = tokio::time::timeout_at(deadline, old_down.data())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        parser.push(&chunk);
        while let Some((kind, _, payload)) = parser.next_frame() {
            if kind == 0x0c {
                token = Some(payload);
            } else if kind == FRAME_DATA {
                received.extend_from_slice(&payload);
            }
        }
    }
    assert_eq!(received, b"abcdef");
    let token = token.unwrap();
    assert_eq!(token.len(), 32);

    // A forged proof must leave the old pumps and offsets intact.
    for valid in [false, true] {
        futures::future::poll_fn(|cx| sender.poll_ready(cx))
            .await
            .unwrap();
        let response = tokio::time::timeout(
            ACK_TIMEOUT,
            sender.send_request(
                http::Request::builder()
                    .method("GET")
                    .uri(&uri)
                    .header("x-xhttp-version", "1")
                    .body(hyper::Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let challenge = base64::engine::general_purpose::STANDARD
            .decode(response.headers()["x-xhttp-challenge"].as_bytes())
            .unwrap();
        let mut resumed_down = response.into_body();
        let mut payload = vec![0u8; 16];
        let mut message = b"cattunnel-xhttp-resume-v1".to_vec();
        message.extend_from_slice(sid.as_bytes());
        message.extend_from_slice(&payload);
        message.extend_from_slice(&challenge);
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &token);
        let tag = ring::hmac::sign(&key, &message);
        payload.extend_from_slice(tag.as_ref());
        if !valid {
            payload[16] ^= 1;
        }
        let (mut resumed_up, body) = hyper::Body::channel();
        futures::future::poll_fn(|cx| sender.poll_ready(cx))
            .await
            .unwrap();
        let status = sender.send_request(
            http::Request::builder()
                .method("POST")
                .uri(&uri)
                .header("x-xhttp-version", "1")
                .body(body)
                .unwrap(),
        );
        resumed_up
            .send_data(encode_frame(FRAME_RESUME, 0, &payload))
            .await
            .unwrap();
        let status = tokio::time::timeout(ACK_TIMEOUT, status)
            .await
            .unwrap()
            .unwrap()
            .status();
        if !valid {
            assert_eq!(status, http::StatusCode::FORBIDDEN);
            more.notify_one();
            let deadline = tokio::time::Instant::now() + ACK_TIMEOUT;
            while received.len() < 10 {
                let chunk = tokio::time::timeout_at(deadline, old_down.data())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                parser.push(&chunk);
                while let Some((kind, _, payload)) = parser.next_frame() {
                    if kind == FRAME_DATA {
                        received.extend_from_slice(&payload);
                    }
                }
            }
            assert_eq!(received, b"abcdefghij");
        } else {
            assert_eq!(status, http::StatusCode::OK);
            let mut replay = FrameCollector::new();
            let mut bytes = Vec::new();
            let deadline = tokio::time::Instant::now() + ACK_TIMEOUT;
            while bytes.len() < 10 {
                let chunk = tokio::time::timeout_at(deadline, resumed_down.data())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                replay.push(&chunk);
                while let Some((kind, offset, payload)) = replay.next_frame() {
                    if kind == FRAME_DATA {
                        assert_eq!(offset, bytes.len() as u64);
                        bytes.extend_from_slice(&payload);
                    }
                }
            }
            assert_eq!(bytes, b"abcdefghij");
        }
    }
    peer_task.abort();
    connection_task.abort();
}

async fn fin_before_downlink(early_post_eof: bool) {
    use tokio::io::AsyncWriteExt;
    let destination = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = destination.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = destination.accept().await.unwrap();
        let mut upload = Vec::new();
        socket.read_to_end(&mut upload).await.unwrap();
        assert_eq!(upload, b"request");
        socket.write_all(b"response-after-fin").await.unwrap();
        socket.shutdown().await.unwrap();
    });
    let address = common::make_endpoint_address();
    spawn_endpoint(address).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tls =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &address, Some(b"h2")).await;
    let (mut sender, connection) = hyper::client::conn::Builder::new()
        .http2_only(true)
        .handshake(tls)
        .await
        .unwrap();
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let uri = format!("https://{}/xh/fin-order", common::MAIN_DOMAIN_NAME);
    let down = sender.send_request(
        http::Request::builder()
            .method("GET")
            .uri(&uri)
            .header("x-xhttp-version", "1")
            .body(hyper::Body::empty())
            .unwrap(),
    );
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let (mut upload, body) = hyper::Body::channel();
    let post = sender.send_request(
        http::Request::builder()
            .method("POST")
            .uri(&uri)
            .header("x-xhttp-version", "1")
            .body(body)
            .unwrap(),
    );
    let mut start = encode_frame(FRAME_OPEN, 0, format!("127.0.0.1:{port}").as_bytes()).to_vec();
    start.extend_from_slice(&encode_frame(FRAME_DATA, 0, b"request"));
    start.extend_from_slice(&encode_frame(FRAME_FIN, 7, &[]));
    upload.send_data(Bytes::from(start)).await.unwrap();
    let mut upload = Some(upload);
    if early_post_eof {
        upload.take();
    }
    let response = tokio::time::timeout(ACK_TIMEOUT, down)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    let mut body = response.into_body();
    assert_eq!(
        tokio::time::timeout(ACK_TIMEOUT, post)
            .await
            .unwrap()
            .unwrap()
            .status(),
        http::StatusCode::OK
    );
    let mut parser = FrameCollector::new();
    let mut received = Vec::new();
    let deadline = tokio::time::Instant::now() + ACK_TIMEOUT;
    'frames: loop {
        let chunk = tokio::time::timeout_at(deadline, body.data())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        parser.push(&chunk);
        while let Some((kind, offset, payload)) = parser.next_frame() {
            if kind == FRAME_DATA {
                assert_eq!(offset, received.len() as u64);
                received.extend_from_slice(&payload);
            } else if kind == FRAME_FIN {
                assert!(payload.is_empty());
                assert_eq!(offset, received.len() as u64);
                break 'frames;
            }
        }
    }
    assert_eq!(received, b"response-after-fin");
    if let Some(mut upload) = upload {
        upload
            .send_data(encode_frame(FRAME_ACK, received.len() as u64, &[FRAME_FIN]))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(chunk) = body.data().await {
                chunk.unwrap();
            }
        })
        .await
        .expect("final ACK must finish the response without the resume timeout");
    } else {
        tokio::time::timeout(Duration::from_secs(12), async {
            while let Some(chunk) = body.data().await {
                chunk.unwrap();
            }
        })
        .await
        .expect("missing final ACK must have a bounded wait");
    }
    tokio::time::timeout(ACK_TIMEOUT, peer)
        .await
        .unwrap()
        .unwrap();
    connection_task.abort();
}

#[tokio::test]
async fn uplink_fin_before_downlink_fin_allows_response_and_final_ack() {
    fin_before_downlink(false).await;
}

#[tokio::test]
async fn early_post_eof_after_fin_does_not_discard_downlink_response() {
    fin_before_downlink(true).await;
}

async fn reserved_bytes(address: SocketAddr) -> usize {
    let client = hyper::Client::new();
    let response = tokio::time::timeout(
        ACK_TIMEOUT,
        client.get(format!("http://{address}/metrics").parse().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    let bytes = tokio::time::timeout(ACK_TIMEOUT, hyper::body::to_bytes(response.into_body()))
        .await
        .unwrap()
        .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    text.lines()
        .find_map(|line| line.strip_prefix("xhttp_reserved_bytes "))
        .expect("reservation gauge must be exported")
        .parse()
        .unwrap()
}

#[tokio::test]
async fn budget_admits_200_live_sessions_rejects_201_and_returns_to_zero() {
    const SESSIONS: usize = 200;
    const BASE_RESERVE: usize = 256 * 1024;
    let destination = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = destination.local_addr().unwrap().port();
    let close = Arc::new(tokio::sync::Notify::new());
    let peer_close = close.clone();
    let peer = tokio::spawn(async move {
        let mut sockets = Vec::new();
        for _ in 0..SESSIONS {
            sockets.push(destination.accept().await.unwrap().0);
        }
        peer_close.notified().await;
        drop(sockets);
    });
    let address = common::make_endpoint_address();
    let metrics_address = common::make_endpoint_address();
    let settings = Settings::builder()
        .listen_address(address)
        .unwrap()
        .listen_protocols(ListenProtocolSettings {
            http1: Some(Http1Settings::builder().build()),
            http2: Some(Http2Settings::builder().build()),
            quic: None,
        })
        .xhttp_enable(true)
        .xhttp_path("/xh/")
        .xhttp_memory_limit(SESSIONS * BASE_RESERVE)
        .metrics(
            MetricsSettings::builder()
                .listen_address(metrics_address)
                .unwrap()
                .build()
                .unwrap(),
        )
        .allow_private_network_connections(true)
        .build()
        .unwrap();
    spawn_endpoint_settings(settings).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tls =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &address, Some(b"h2")).await;
    let (mut sender, connection) = hyper::client::conn::Builder::new()
        .http2_only(true)
        .handshake(tls)
        .await
        .unwrap();
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut sessions = Vec::new();
    for index in 0..SESSIONS {
        futures::future::poll_fn(|cx| sender.poll_ready(cx))
            .await
            .unwrap();
        let uri = format!("https://{}/xh/budget-{index}", common::MAIN_DOMAIN_NAME);
        let down = sender.send_request(
            http::Request::builder()
                .method("GET")
                .uri(&uri)
                .header("x-xhttp-version", "1")
                .body(hyper::Body::empty())
                .unwrap(),
        );
        futures::future::poll_fn(|cx| sender.poll_ready(cx))
            .await
            .unwrap();
        let (mut upload, body) = hyper::Body::channel();
        let post = sender.send_request(
            http::Request::builder()
                .method("POST")
                .uri(&uri)
                .header("x-xhttp-version", "1")
                .body(body)
                .unwrap(),
        );
        upload
            .send_data(encode_frame(
                FRAME_OPEN,
                0,
                format!("127.0.0.1:{port}").as_bytes(),
            ))
            .await
            .unwrap();
        let response = tokio::time::timeout(ACK_TIMEOUT, down)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK, "session {index}");
        assert_eq!(
            tokio::time::timeout(ACK_TIMEOUT, post)
                .await
                .unwrap()
                .unwrap()
                .status(),
            http::StatusCode::OK
        );
        sessions.push((upload, response.into_body()));
    }
    assert_eq!(
        reserved_bytes(metrics_address).await,
        SESSIONS * BASE_RESERVE
    );
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let rejected = sender.send_request(
        http::Request::builder()
            .method("GET")
            .uri(format!(
                "https://{}/xh/budget-overflow",
                common::MAIN_DOMAIN_NAME
            ))
            .header("x-xhttp-version", "1")
            .body(hyper::Body::empty())
            .unwrap(),
    );
    assert_eq!(
        tokio::time::timeout(ACK_TIMEOUT, rejected)
            .await
            .unwrap()
            .unwrap()
            .status(),
        http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        reserved_bytes(metrics_address).await,
        SESSIONS * BASE_RESERVE
    );
    close.notify_one();
    for (mut upload, mut down) in sessions {
        let mut parser = FrameCollector::new();
        let deadline = tokio::time::Instant::now() + ACK_TIMEOUT;
        'fin: loop {
            let bytes = tokio::time::timeout_at(deadline, down.data())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            parser.push(&bytes);
            while let Some((kind, offset, _)) = parser.next_frame() {
                if kind == FRAME_FIN {
                    assert_eq!(offset, 0);
                    break 'fin;
                }
            }
        }
        upload
            .send_data(encode_frame(FRAME_ACK, 0, &[FRAME_FIN]))
            .await
            .unwrap();
        tokio::time::timeout(ACK_TIMEOUT, async {
            while let Some(bytes) = down.data().await {
                bytes.unwrap();
            }
        })
        .await
        .unwrap();
    }
    tokio::time::timeout(ACK_TIMEOUT, async {
        loop {
            if reserved_bytes(metrics_address).await == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all reservation owners must be released after final ACK");
    tokio::time::timeout(ACK_TIMEOUT, peer)
        .await
        .unwrap()
        .unwrap();
    connection.abort();
    eprintln!("B12: 200 live sessions, session 201 HTTP 503, reserved bytes returned to 0");
}

struct TestCrypto {
    tx: ring::aead::LessSafeKey,
    rx: ring::aead::LessSafeKey,
    sent: u64,
    received: u64,
    sid: String,
    buffered: BytesMut,
}

impl TestCrypto {
    async fn handshake(up: &mut hyper::body::Sender, down: &mut hyper::Body, sid: &str) -> Self {
        use ring::{agreement, signature::KeyPair};
        let private = agreement::EphemeralPrivateKey::generate(
            &agreement::X25519,
            &ring::rand::SystemRandom::new(),
        )
        .unwrap();
        let public = private.compute_public_key().unwrap();
        let mut hello = vec![0, 1, 0, 0, 0, 0];
        hello.extend_from_slice(public.as_ref());
        hello.extend_from_slice(&[9; 16]);
        up.send_data(encode_frame(0x0a, 0, &hello)).await.unwrap();
        let mut parser = FrameCollector::new();
        let ack = tokio::time::timeout(ACK_TIMEOUT, async {
            loop {
                if let Some((kind, offset, payload)) = parser.next_frame() {
                    assert_eq!((kind, offset), (0x0b, 0));
                    break payload;
                }
                parser.push(&down.data().await.unwrap().unwrap());
            }
        })
        .await
        .unwrap();
        assert_eq!(ack.len(), 102);
        assert_eq!(&ack[..6], &[0, 1, 0, 0, 0, 0]);
        let mut transcript = b"cattunnel-xhttp-handshake-v1".to_vec();
        transcript.extend_from_slice(&1u16.to_be_bytes());
        transcript.extend_from_slice(&hello[..6]);
        transcript.extend_from_slice(sid.as_bytes());
        transcript.extend_from_slice(public.as_ref());
        transcript.extend_from_slice(&ack[6..38]);
        transcript.extend_from_slice(&hello[38..]);
        let signing = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, signing.public_key())
            .verify(&transcript, &ack[38..])
            .unwrap();
        let shared = agreement::agree_ephemeral(
            private,
            &agreement::UnparsedPublicKey::new(&agreement::X25519, &ack[6..38]),
            |material| material.to_vec(),
        )
        .unwrap();
        let hash = ring::digest::digest(&ring::digest::SHA256, &transcript);
        let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, hash.as_ref()).extract(&shared);
        let key = |info: &[u8]| {
            let infos = [info];
            let mut bytes = [0; 16];
            prk.expand(&infos, &ring::aead::AES_128_GCM)
                .unwrap()
                .fill(&mut bytes)
                .unwrap();
            ring::aead::LessSafeKey::new(
                ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &bytes).unwrap(),
            )
        };
        Self {
            tx: key(b"cattunnel-xhttp-v1 c2s"),
            rx: key(b"cattunnel-xhttp-v1 s2c"),
            sent: 0,
            received: 0,
            sid: sid.into(),
            buffered: BytesMut::new(),
        }
    }

    fn seal(&mut self, kind: u8, offset: u64, payload: &[u8]) -> Bytes {
        let mut plain = encode_frame(kind, offset, payload).to_vec();
        let mut nonce = [0; 12];
        nonce[0] = 1;
        nonce[4..].copy_from_slice(&self.sent.to_be_bytes());
        self.sent += 1;
        self.tx
            .seal_in_place_append_tag(
                ring::aead::Nonce::assume_unique_for_key(nonce),
                ring::aead::Aad::from(self.sid.as_bytes()),
                &mut plain,
            )
            .unwrap();
        let mut record = (plain.len() as u32).to_be_bytes().to_vec();
        record.extend_from_slice(&plain);
        Bytes::from(record)
    }

    async fn next_frame(&mut self, down: &mut hyper::Body) -> (u8, u64, Bytes) {
        tokio::time::timeout(ACK_TIMEOUT, async {
            loop {
                if self.buffered.len() >= 4 {
                    let len = u32::from_be_bytes(self.buffered[..4].try_into().unwrap()) as usize;
                    assert!((16..=1024 * 1024 + 16).contains(&len));
                    if self.buffered.len() >= len + 4 {
                        let record = self.buffered.split_to(len + 4);
                        let mut ciphertext = record[4..].to_vec();
                        let mut nonce = [0; 12];
                        nonce[0] = 2;
                        nonce[4..].copy_from_slice(&self.received.to_be_bytes());
                        self.received += 1;
                        let plain = self
                            .rx
                            .open_in_place(
                                ring::aead::Nonce::assume_unique_for_key(nonce),
                                ring::aead::Aad::from(self.sid.as_bytes()),
                                &mut ciphertext,
                            )
                            .unwrap();
                        let mut parser = FrameCollector::new();
                        parser.push(plain);
                        return parser
                            .next_frame()
                            .expect("one complete server frame per record");
                    }
                }
                self.buffered
                    .extend_from_slice(&down.data().await.unwrap().unwrap());
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn encrypted_resume_rekeys_after_lost_records_and_rejects_unknown_inner_frame() {
    use base64::Engine;
    use tokio::io::AsyncWriteExt;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let release = Arc::new(tokio::sync::Notify::new());
    let release_peer = release.clone();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"replayed bytes").await.unwrap();
        release_peer.notified().await;
        let mut bytes = [0; 5];
        socket.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"fresh");
        socket.write_all(b"new bytes").await.unwrap();
        let mut extra = [0; 1];
        assert_eq!(socket.read(&mut extra).await.unwrap(), 0);
    });
    let address = common::make_endpoint_address();
    let settings = Settings::builder()
        .listen_address(address)
        .unwrap()
        .listen_protocols(ListenProtocolSettings {
            http1: None,
            http2: Some(Http2Settings::builder().build()),
            quic: None,
        })
        .xhttp_enable(true)
        .xhttp_path("/xh/")
        .xhttp_secret_key(base64::engine::general_purpose::STANDARD.encode([7; 32]))
        .allow_private_network_connections(true)
        .build()
        .unwrap();
    spawn_endpoint_settings(settings).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tls =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &address, Some(b"h2")).await;
    let (mut sender, connection) = hyper::client::conn::Builder::new()
        .http2_only(true)
        .handshake(tls)
        .await
        .unwrap();
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let sid = "encrypted-rekey";
    let uri = format!("https://{}/xh/{sid}", common::MAIN_DOMAIN_NAME);
    let down = sender.send_request(
        http::Request::builder()
            .method("GET")
            .uri(&uri)
            .header("x-xhttp-version", "1")
            .body(hyper::Body::empty())
            .unwrap(),
    );
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let (mut up, body) = hyper::Body::channel();
    let post = sender.send_request(
        http::Request::builder()
            .method("POST")
            .uri(&uri)
            .header("x-xhttp-version", "1")
            .body(body)
            .unwrap(),
    );
    let mut down = tokio::time::timeout(ACK_TIMEOUT, down)
        .await
        .unwrap()
        .unwrap()
        .into_body();
    let mut crypto = TestCrypto::handshake(&mut up, &mut down, sid).await;
    up.send_data(crypto.seal(FRAME_OPEN, 0, format!("127.0.0.1:{port}").as_bytes()))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(ACK_TIMEOUT, post)
            .await
            .unwrap()
            .unwrap()
            .status(),
        http::StatusCode::OK
    );
    let (kind, _, token) = crypto.next_frame(&mut down).await;
    assert_eq!(kind, 0x0c);
    assert_eq!(token.len(), 32);
    // Lose an actual encrypted DATA record without advancing the old receive counter.
    if crypto.buffered.is_empty() {
        crypto.buffered.extend_from_slice(
            &tokio::time::timeout(ACK_TIMEOUT, down.data())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        );
    }
    assert!(!crypto.buffered.is_empty());
    drop(down);
    drop(up);
    let response = tokio::time::timeout(
        ACK_TIMEOUT,
        sender.send_request(
            http::Request::builder()
                .method("GET")
                .uri(&uri)
                .header("x-xhttp-version", "1")
                .body(hyper::Body::empty())
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let challenge = base64::engine::general_purpose::STANDARD
        .decode(response.headers()["x-xhttp-challenge"].as_bytes())
        .unwrap();
    let mut down = response.into_body();
    futures::future::poll_fn(|cx| sender.poll_ready(cx))
        .await
        .unwrap();
    let (mut up, body) = hyper::Body::channel();
    let post = sender.send_request(
        http::Request::builder()
            .method("POST")
            .uri(&uri)
            .header("x-xhttp-version", "1")
            .body(body)
            .unwrap(),
    );
    let mut fresh = TestCrypto::handshake(&mut up, &mut down, sid).await;
    let mut payload = vec![0; 16];
    let mut message = b"cattunnel-xhttp-resume-v1".to_vec();
    message.extend_from_slice(sid.as_bytes());
    message.extend_from_slice(&payload);
    message.extend_from_slice(&challenge);
    payload.extend_from_slice(
        ring::hmac::sign(
            &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &token),
            &message,
        )
        .as_ref(),
    );
    up.send_data(fresh.seal(FRAME_RESUME, 0, &payload))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(ACK_TIMEOUT, post)
            .await
            .unwrap()
            .unwrap()
            .status(),
        http::StatusCode::OK
    );
    let (kind, offset, replay) = fresh.next_frame(&mut down).await;
    assert_eq!((kind, offset), (FRAME_DATA, 0));
    assert_eq!(replay.as_ref(), b"replayed bytes");
    assert_eq!(fresh.received, 1);
    release.notify_one();
    up.send_data(fresh.seal(FRAME_DATA, 0, b"fresh"))
        .await
        .unwrap();
    let (kind, offset, bytes) = fresh.next_frame(&mut down).await;
    assert_eq!((kind, offset), (FRAME_DATA, replay.len() as u64));
    assert_eq!(bytes.as_ref(), b"new bytes");
    // Valid AEAD, reserved/unknown inner type: the transport must stop.
    up.send_data(fresh.seal(0x10, 0, &[])).await.unwrap();
    tokio::time::timeout(ACK_TIMEOUT, async {
        while let Some(chunk) = down.data().await {
            chunk.unwrap();
        }
    })
    .await
    .unwrap();
    drop(up);
    tokio::time::timeout(ACK_TIMEOUT, peer)
        .await
        .unwrap()
        .unwrap();
    connection.abort();
}
