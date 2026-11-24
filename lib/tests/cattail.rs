//! CatTail end to end: the listener of the endpoint, a TLS client over the
//! CatTail codec, and the silence on every failed admission.

use base64::Engine;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::ServerName;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use trusttunnel::cattail::{self, ClientParams, Encoder, Header, PaddingScheme};
use trusttunnel::net_utils;
use trusttunnel::settings::{
    CatTailSettings, Http1Settings, Http2Settings, ListenProtocolSettings, Settings, TlsHostInfo,
    TlsHostsSettings,
};

#[allow(dead_code)]
mod common;

const KEY: [u8; 32] = [0x5a; 32];

struct Endpoint {
    main: SocketAddr,
    cattail: SocketAddr,
    _cert: common::File,
}

async fn start_endpoint(decoy_file: Option<&std::path::Path>) -> Endpoint {
    common::set_up_logger();
    let main = common::make_endpoint_address();
    let cattail_address = common::make_endpoint_address();
    let mut builder = Settings::builder()
        .listen_address(main)
        .unwrap()
        .listen_protocols(ListenProtocolSettings {
            http1: Some(Http1Settings::builder().build()),
            http2: Some(Http2Settings::builder().build()),
            quic: None,
        })
        .allow_private_network_connections(true)
        .ping_enable(true)
        .ping_path("/ping")
        .cattail(CatTailSettings {
            listen_address: cattail_address,
            key: base64::engine::general_purpose::STANDARD.encode(KEY),
            pattern: None,
        });
    if let Some(path) = decoy_file {
        builder = builder.decoy_only_ips_file(path.to_str().unwrap());
    }
    let settings = builder.build().unwrap();

    let cert = common::make_cert_key_file();
    let cert_path = cert.path.to_str().unwrap().to_string();
    let hosts = TlsHostsSettings::builder()
        .main_hosts(vec![TlsHostInfo {
            hostname: common::MAIN_DOMAIN_NAME.to_string(),
            cert_chain_path: cert_path.clone(),
            private_key_path: cert_path,
            allowed_sni: vec![],
        }])
        .build()
        .unwrap();
    tokio::spawn(common::run_endpoint_with_settings(settings, hosts));
    tokio::time::sleep(Duration::from_secs(1)).await;
    Endpoint {
        main,
        cattail: cattail_address,
        _cert: cert,
    }
}

fn tls_config(alpn: Option<&[u8]>) -> rustls::ClientConfig {
    let mut provider = aws_lc_rs::default_provider();
    provider.kx_groups = vec![
        aws_lc_rs::kx_group::X25519MLKEM768,
        aws_lc_rs::kx_group::X25519,
    ];
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(common::NoopVerifier {}))
        .with_no_client_auth();
    if let Some(alpn) = alpn {
        config.alpn_protocols.push(alpn.to_vec());
    }
    config
}

async fn tls_over<IO>(io: IO, alpn: Option<&[u8]>) -> impl AsyncRead + AsyncWrite + Unpin
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    TlsConnector::from(Arc::new(tls_config(alpn)))
        .connect(
            ServerName::try_from(common::MAIN_DOMAIN_NAME.to_string()).unwrap(),
            io,
        )
        .await
        .unwrap()
}

/// The bytes of a real ClientHello, as the TLS client would send them.
fn client_hello() -> Vec<u8> {
    let mut conn = rustls::ClientConnection::new(
        Arc::new(tls_config(Some(net_utils::HTTP2_ALPN.as_bytes()))),
        ServerName::try_from(common::MAIN_DOMAIN_NAME.to_string()).unwrap(),
    )
    .unwrap();
    let mut out = Vec::new();
    conn.write_tls(&mut out).unwrap();
    out
}

fn params(timestamp: u64, identity: [u8; 8], counter: u64) -> ClientParams {
    ClientParams {
        key: KEY,
        scheme: PaddingScheme::default(),
        timestamp,
        identity,
        counter,
    }
}

/// The first write of a client: the nonce and the frames of the ClientHello.
fn first_segment(key: &[u8; 32], open: Header) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let (mut encoder, _) = Encoder::client(key, PaddingScheme::default(), open, &mut rng);
    encoder.encode(&client_hello(), &mut rng).unwrap()
}

fn identity() -> [u8; 8] {
    rand::random()
}

/// Connect, send `segment` and expect not a single byte back and a FIN no
/// earlier than 10 s after the connect.
async fn expect_silence(address: SocketAddr, segment: Vec<u8>) {
    let started = Instant::now();
    let mut tcp = TcpStream::connect(address).await.unwrap();
    tcp.write_all(&segment).await.unwrap();
    let mut buf = [0u8; 1024];
    let mut received = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(70), tcp.read(&mut buf))
            .await
            .expect("the endpoint did not close the connection in 70 s")
        {
            Ok(0) => break,
            Ok(n) => received += n,
            Err(e) => panic!("the connection ended with an error, not a FIN: {e}"),
        }
    }
    let elapsed = started.elapsed();
    assert_eq!(received, 0, "the endpoint answered");
    assert!(elapsed >= cattail::SILENCE_MIN, "closed after {elapsed:?}");
    assert!(
        elapsed <= cattail::SILENCE_MAX + Duration::from_secs(6),
        "closed after {elapsed:?}"
    );
}

/// The inner bytes reach the TLS acceptor of the main listener: a real TLS
/// client over the CatTail codec gets the ping answer, over HTTP/2 and
/// HTTP/1.1, twice with the same identity.
#[tokio::test]
async fn tls_over_cattail_reaches_the_tls_acceptor() {
    let endpoint = start_endpoint(None).await;
    let id = identity();
    for (counter, version, alpn) in [
        (
            0,
            http::Version::HTTP_2,
            Some(net_utils::HTTP2_ALPN.as_bytes()),
        ),
        (1, http::Version::HTTP_11, None),
    ] {
        let tcp = TcpStream::connect(endpoint.cattail).await.unwrap();
        let inner = cattail::client_wrap(tcp, params(cattail::unix_now(), id, counter));
        let tls = tls_over(inner, alpn).await;
        let (parts, _) = tokio::time::timeout(
            Duration::from_secs(10),
            common::do_get_request(
                tls,
                version,
                &format!(
                    "https://{}:{}/ping",
                    common::MAIN_DOMAIN_NAME,
                    endpoint.main.port()
                ),
                &[],
            ),
        )
        .await
        .expect("no answer over CatTail");
        assert_eq!(parts.status, http::StatusCode::OK, "{version:?}");
    }
}

#[tokio::test]
async fn wrong_key_gets_silence() {
    let endpoint = start_endpoint(None).await;
    let other_key = [0x5b; 32];
    let segment = first_segment(&other_key, Header::open(cattail::unix_now(), identity(), 0));
    expect_silence(endpoint.cattail, segment).await;
}

#[tokio::test]
async fn bytes_that_are_not_cattail_get_silence() {
    let endpoint = start_endpoint(None).await;
    // a plain TLS ClientHello, as a scanner of the port would send it
    expect_silence(endpoint.cattail, client_hello()).await;
}

/// The same first segment twice: the first one is answered, the copy gets
/// silence.
#[tokio::test]
async fn replayed_first_segment_gets_silence() {
    let endpoint = start_endpoint(None).await;
    let segment = first_segment(&KEY, Header::open(cattail::unix_now(), identity(), 3));

    let mut tcp = TcpStream::connect(endpoint.cattail).await.unwrap();
    tcp.write_all(&segment).await.unwrap();
    let mut buf = [0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf))
        .await
        .expect("the first segment was not answered")
        .unwrap();
    assert!(n > cattail::NONCE_LEN, "the answer is {n} bytes");

    expect_silence(endpoint.cattail, segment).await;
}

/// Timestamps 121 s in the past or in the future, and one inside the window
/// but before the start of the listener: all get silence.
#[tokio::test]
async fn timestamps_out_of_the_window_get_silence() {
    let endpoint = start_endpoint(None).await;
    let now = cattail::unix_now();
    tokio::join!(
        expect_silence(
            endpoint.cattail,
            first_segment(&KEY, Header::open(now - 121, identity(), 0))
        ),
        expect_silence(
            endpoint.cattail,
            first_segment(&KEY, Header::open(now + 121, identity(), 0))
        ),
        expect_silence(
            endpoint.cattail,
            first_segment(&KEY, Header::open(now - 30, identity(), 0))
        ),
    );
}

/// An address of the decoy trap gets silence even with the right key.
#[tokio::test]
async fn decoy_only_address_gets_silence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("decoy.txt");
    std::fs::write(&path, "127.0.0.1\n").unwrap();
    let endpoint = start_endpoint(Some(&path)).await;
    let segment = first_segment(&KEY, Header::open(cattail::unix_now(), identity(), 0));
    expect_silence(endpoint.cattail, segment).await;
}
