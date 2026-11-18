use bytes::Bytes;
use futures::stream;
use http::{Request, Response};
use log::info;
use once_cell::sync::Lazy;
use ring::digest::{digest, SHA256};
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use trusttunnel::net_utils;
use trusttunnel::settings::{
    Http1Settings, Http2Settings, ListenProtocolSettings, QuicSettings, ReverseProxySettings,
    Settings, TlsHostInfo, TlsHostsSettings,
};

#[allow(dead_code)]
mod common;

// Use a larger body to catch partial responses without flooding logs.
static RESPONSE_BODY: Lazy<Bytes> = Lazy::new(|| Bytes::from(vec![b'x'; 1024 * 1024]));

macro_rules! reverse_proxy_tests {
    ($($name:ident: $client_fn:expr,)*) => {
    $(
        #[tokio::test]
        async fn $name() {
            common::set_up_logger();
            let endpoint_address = common::make_endpoint_address();
            let (proxy_address, proxy_task) = run_proxy();

            let client_task = async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let (response, body) = $client_fn(&endpoint_address).await;
                assert_eq!(response.status, http::StatusCode::OK);
                assert_body_matches(&body);
            };
            let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, true);

            // Pin tasks so they can be polled across multiple select! invocations
            // without being dropped (dropping run_endpoint mid-transfer would tear
            // down the QuicMultiplexer and abort in-flight H3 streams).
            tokio::pin!(client_task);
            tokio::pin!(proxy_task);
            tokio::pin!(endpoint_task);

            tokio::select! {
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
                // Wait for client_task first; if proxy_task completes, continue waiting for client
                _ = &mut client_task => (),
                _ = &mut proxy_task => {
                    // Proxy completed (expected after handling request); keep endpoint
                    // alive while we wait for the client to finish draining the response.
                    tokio::select! {
                        _ = client_task => (),
                        _ = &mut endpoint_task => unreachable!(),
                        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("Client timed out after proxy completed"),
                    }
                },
            }
        }
    )*
    }
}

reverse_proxy_tests! {
    sni_h1: sni_h1_client,
    sni_h2: sni_h2_client,
    sni_h3: sni_h3_client,
    path_h1: path_h1_client,
    path_h2: path_h2_client,
    path_h3: path_h3_client,
}

#[tokio::test]
async fn path_h2_chunked() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_chunked();

    let client_task = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (response, body) = path_h2_client(&endpoint_address).await;
        assert_eq!(response.status, http::StatusCode::OK);
        assert_body_matches(&body);
    };
    let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, true);

    tokio::pin!(client_task);
    tokio::pin!(proxy_task);
    tokio::pin!(endpoint_task);

    tokio::select! {
        _ = &mut endpoint_task => unreachable!(),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
        _ = &mut client_task => (),
        _ = &mut proxy_task => {
            tokio::select! {
                _ = client_task => (),
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    panic!("Client timed out after proxy completed")
                }
            }
        },
    }
}

#[tokio::test]
async fn path_h3_chunked() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_chunked();

    let client_task = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (response, body) = path_h3_client(&endpoint_address).await;
        assert_eq!(response.status, http::StatusCode::OK);
        assert_body_matches(&body);
    };
    let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, true);

    tokio::pin!(client_task);
    tokio::pin!(proxy_task);
    tokio::pin!(endpoint_task);

    tokio::select! {
        _ = &mut endpoint_task => unreachable!(),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
        _ = &mut client_task => (),
        _ = &mut proxy_task => {
            tokio::select! {
                _ = client_task => (),
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    panic!("Client timed out after proxy completed")
                }
            }
        },
    }
}

// The reverse proxy server address comes from the endpoint configuration, so it must be
// reachable even if it belongs to a private network and client connections to private
// networks are forbidden.
#[tokio::test]
async fn path_h1_private_network_disallowed() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy();

    let client_task = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (response, body) = path_h1_client(&endpoint_address).await;
        assert_eq!(response.status, http::StatusCode::OK);
        assert_body_matches(&body);
    };
    let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, false);

    tokio::pin!(client_task);
    tokio::pin!(proxy_task);
    tokio::pin!(endpoint_task);

    tokio::select! {
        _ = &mut endpoint_task => unreachable!(),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
        _ = &mut client_task => (),
        _ = &mut proxy_task => {
            tokio::select! {
                _ = client_task => (),
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    panic!("Client timed out after proxy completed")
                }
            }
        },
    }
}

fn assert_body_matches(body: &Bytes) {
    assert_eq!(body.len(), RESPONSE_BODY.len(), "response length mismatch");
    let expected_hash = digest(&SHA256, RESPONSE_BODY.as_ref());
    let actual_hash = digest(&SHA256, body.as_ref());
    assert_eq!(
        actual_hash.as_ref(),
        expected_hash.as_ref(),
        "response hash mismatch"
    );
}

// The size of the body of a request with a body.
static REQUEST_BODY_LEN: usize = 4096;

// A request with a body: the body has to reach the upstream while the
// response is waited for. Before the fix the upstream never got it, the
// upstream waited for the body and the endpoint waited for the response, and
// the request hung until the upstream's body timeout.
#[tokio::test]
async fn post_body_h1() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_echo();

    let client_task = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let stream =
            common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &endpoint_address, None)
                .await;

        let response = common::do_post_request(
            stream,
            http::Version::HTTP_11,
            &format!(
                "https://{}:{}/hello/haha",
                common::MAIN_DOMAIN_NAME,
                endpoint_address.port()
            ),
            REQUEST_BODY_LEN,
        )
        .await;

        assert_eq!(response.status(), http::StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(
            body.as_ref(),
            REQUEST_BODY_LEN.to_string().as_bytes(),
            "the upstream did not get the whole body"
        );
    };
    let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, true);

    tokio::pin!(client_task);
    tokio::pin!(proxy_task);
    tokio::pin!(endpoint_task);

    tokio::select! {
        _ = &mut endpoint_task => unreachable!(),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
        _ = &mut client_task => (),
        _ = &mut proxy_task => {
            tokio::select! {
                _ = client_task => (),
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    panic!("Client timed out after proxy completed")
                }
            }
        },
    }
}

// The same over HTTP/2: there the body is a stream of frames.
#[tokio::test]
async fn post_body_h2() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_echo();

    let client_task = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let stream = common::establish_tls_connection(
            common::MAIN_DOMAIN_NAME,
            &endpoint_address,
            Some(net_utils::HTTP2_ALPN.as_bytes()),
        )
        .await;

        let response = common::do_post_request(
            stream,
            http::Version::HTTP_2,
            &format!(
                "https://{}:{}/hello/haha",
                common::MAIN_DOMAIN_NAME,
                endpoint_address.port()
            ),
            REQUEST_BODY_LEN,
        )
        .await;

        assert_eq!(response.status(), http::StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(
            body.as_ref(),
            REQUEST_BODY_LEN.to_string().as_bytes(),
            "the upstream did not get the whole body"
        );
    };
    let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, true);

    tokio::pin!(client_task);
    tokio::pin!(proxy_task);
    tokio::pin!(endpoint_task);

    tokio::select! {
        _ = &mut endpoint_task => unreachable!(),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
        _ = &mut client_task => (),
        _ = &mut proxy_task => {
            tokio::select! {
                _ = client_task => (),
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    panic!("Client timed out after proxy completed")
                }
            }
        },
    }
}

// Two requests on one HTTP/1.1 connection of the client: the response of the
// first one must not end the connection. The endpoint closes it after every
// response, the client opens a new TLS connection for a style sheet or a
// script, and a page opens badly.
#[tokio::test]
async fn keep_alive_h1_two_gets() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_multi();

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (mut sender, _conn) = h1_client(&client_endpoint).await;
        for _ in 0..2 {
            let (response, body) = send_get_h1(&mut sender, &client_endpoint).await;
            assert_eq!(response.status, http::StatusCode::OK);
            assert_body_matches(&body);
        }
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
}

// A request with a body and then a request without one, on the same
// connection: the body of the first is read to its end, the second request is
// served by the same connection. The POST tests above have to stay green.
#[tokio::test]
async fn keep_alive_h1_post_then_get() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_echo_multi();

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (mut sender, _conn) = h1_client(&client_endpoint).await;

        let response = sender
            .send_request(
                hyper::Request::post(format!(
                    "https://{}:{}/hello/haha",
                    common::MAIN_DOMAIN_NAME,
                    client_endpoint.port()
                ))
                .version(http::Version::HTTP_11)
                .body(hyper::Body::from(vec![0u8; REQUEST_BODY_LEN]))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(body.as_ref(), REQUEST_BODY_LEN.to_string().as_bytes());

        let (response, body) = send_get_h1(&mut sender, &client_endpoint).await;
        assert_eq!(response.status, http::StatusCode::OK);
        assert_eq!(body.as_ref(), b"0");
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
}

// A request with a body over HTTP/2 to the reverse proxy host: the body has
// to reach the upstream and the answer has to come back.
#[tokio::test]
async fn sni_h2_post() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_echo();

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let stream = common::establish_tls_connection(
            &format!("hello.{}", common::MAIN_DOMAIN_NAME),
            &client_endpoint,
            Some(net_utils::HTTP2_ALPN.as_bytes()),
        )
        .await;

        let response = common::do_post_request(
            stream,
            http::Version::HTTP_2,
            &format!(
                "https://hello.{}:{}",
                common::MAIN_DOMAIN_NAME,
                client_endpoint.port()
            ),
            REQUEST_BODY_LEN,
        )
        .await;

        assert_eq!(response.status(), http::StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(body.as_ref(), REQUEST_BODY_LEN.to_string().as_bytes());
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
}

// A gzip-compressed page, as nginx sends it with gzip on.
static GZIP_BODY: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xed, 0xd5, 0x51, 0x0e, 0x83, 0x30,
    0x08, 0x06, 0xe0, 0xab, 0xb0, 0x0b, 0xd8, 0xf8, 0x8e, 0xbc, 0xec, 0x24, 0xd5, 0xe2, 0xda, 0x0c,
    0x6d, 0x23, 0x98, 0xac, 0xb7, 0x5f, 0xa3, 0x3b, 0xc5, 0xd2, 0x97, 0x86, 0x92, 0x0f, 0x78, 0xfc,
    0xf1, 0x11, 0xf2, 0x62, 0xb5, 0x30, 0x44, 0xdb, 0x84, 0xf0, 0xf7, 0xb2, 0x0f, 0x84, 0x96, 0x4c,
    0x98, 0x02, 0x2f, 0xb9, 0xa2, 0xbb, 0x3f, 0x28, 0x69, 0x7f, 0xc3, 0xc1, 0x32, 0xa9, 0x55, 0x61,
    0x8d, 0xcc, 0x06, 0xf1, 0xe0, 0x75, 0x72, 0x9b, 0x4f, 0xfb, 0xb0, 0xa8, 0x12, 0xba, 0x7b, 0x7c,
    0xce, 0xa1, 0xb6, 0x55, 0x23, 0x3d, 0x25, 0x9f, 0xa1, 0x75, 0x47, 0xc2, 0x42, 0x9a, 0x37, 0x86,
    0x22, 0xfe, 0xd4, 0x34, 0x4b, 0xab, 0xfc, 0x8b, 0xc1, 0xf8, 0x63, 0xb0, 0xe6, 0x03, 0x2c, 0x32,
    0x5c, 0xf7, 0x40, 0x93, 0xf1, 0x00, 0x1d, 0x77, 0xdc, 0x71, 0xc7, 0x1d, 0x77, 0xfc, 0x67, 0x18,
    0x5d, 0x69, 0x49, 0x79, 0x67, 0xa4, 0xbb, 0x52, 0xf7, 0x0b, 0xed, 0xef, 0xde, 0xf9, 0x8b, 0x07,
    0x00, 0x00,
];

// The source of the page answers with a chunked gzip body and then keeps its
// connection open, as nginx does for keepalive_timeout (65 seconds by
// default). The end of the response is the last chunk, not the EOF of the
// source: waiting for that EOF froze the page for 65 seconds.
#[tokio::test]
async fn chunked_response_ends_at_the_last_chunk() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task, upstream_requests) = run_upstream_chunked_keep_open();

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let started = tokio::time::Instant::now();
        let mut stream =
            common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &client_endpoint, None)
                .await;
        stream
            .write_all(
                format!(
                    "GET /hello/haha HTTP/1.1\r\nHost: {}:{}\r\nAccept-Encoding: gzip\r\n\r\n",
                    common::MAIN_DOMAIN_NAME,
                    client_endpoint.port()
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let response = read_until(
            &mut stream,
            b"X-Upstream-Trailer: yes\r\n\r\n",
            Duration::from_secs(5),
        )
        .await;
        let elapsed = started.elapsed();
        let text = String::from_utf8_lossy(&response).to_ascii_lowercase();
        assert!(text.starts_with("http/1.1 200"), "{}", text);
        assert!(text.contains("transfer-encoding: chunked"), "{}", text);
        assert!(text.contains("content-encoding: gzip"), "{}", text);
        assert!(
            text.contains("x-upstream-trailer: yes"),
            "the trailers are not passed to the client: {}",
            text
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the response waited for the source: {:?}",
            elapsed
        );

        let upstream_request = upstream_requests.lock().unwrap()[0].to_ascii_lowercase();
        assert!(
            upstream_request.contains("connection: close"),
            "the source is not asked to close: {}",
            upstream_request
        );
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
}

// Two requests over one connection of the client, and the source never closes
// its own connection after a chunked response. The second request has to go
// over the same connection and be answered at once.
#[tokio::test]
async fn keep_alive_h1_two_gets_chunked() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task, upstream_requests) = run_upstream_chunked_keep_open();

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let started = tokio::time::Instant::now();
        let (mut sender, _conn) = h1_client(&client_endpoint).await;
        for _ in 0..2 {
            let (response, body) = send_get_h1(&mut sender, &client_endpoint).await;
            assert_eq!(response.status, http::StatusCode::OK);
            assert_eq!(body.as_ref(), GZIP_BODY);
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the requests waited for the source: {:?}",
            started.elapsed()
        );

        let seen = upstream_requests.lock().unwrap();
        assert_eq!(seen.len(), 2, "the source got {} requests", seen.len());
        for headers in seen.iter() {
            assert!(
                headers.to_ascii_lowercase().contains("connection: close"),
                "the source is not asked to close: {}",
                headers
            );
        }
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
}

// A request with a body, then a request without one, and the source answers
// with chunked bodies and keeps its connection open. The connection of the
// client has to serve both requests.
#[tokio::test]
async fn keep_alive_h1_post_then_get_chunked() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task, upstream_requests) = run_upstream_chunked_keep_open();

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (mut sender, _conn) = h1_client(&client_endpoint).await;

        let response = sender
            .send_request(
                hyper::Request::post(format!(
                    "https://{}:{}/hello/haha",
                    common::MAIN_DOMAIN_NAME,
                    client_endpoint.port()
                ))
                .version(http::Version::HTTP_11)
                .body(hyper::Body::from(vec![0u8; REQUEST_BODY_LEN]))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(body.as_ref(), GZIP_BODY);

        let (response, body) = send_get_h1(&mut sender, &client_endpoint).await;
        assert_eq!(response.status, http::StatusCode::OK);
        assert_eq!(body.as_ref(), GZIP_BODY);

        let seen = upstream_requests.lock().unwrap();
        assert_eq!(seen.len(), 2, "the source got {} requests", seen.len());
        for headers in seen.iter() {
            assert!(
                headers.to_ascii_lowercase().contains("connection: close"),
                "the source is not asked to close: {}",
                headers
            );
        }
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
}

// The same over HTTP/2: several streams on one connection.
#[tokio::test]
async fn keep_alive_h2_several_streams() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_multi();

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let stream = common::establish_tls_connection(
            common::MAIN_DOMAIN_NAME,
            &client_endpoint,
            Some(net_utils::HTTP2_ALPN.as_bytes()),
        )
        .await;
        let (mut sender, conn) = hyper::client::conn::Builder::new()
            .http2_only(true)
            .handshake::<_, hyper::Body>(stream)
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let url = format!(
            "https://{}:{}/hello/haha",
            common::MAIN_DOMAIN_NAME,
            client_endpoint.port()
        );
        for _ in 0..2 {
            let response = sender
                .send_request(
                    hyper::Request::get(url.clone())
                        .version(http::Version::HTTP_2)
                        .body(hyper::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), http::StatusCode::OK);
            let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
            assert_body_matches(&body);
        }
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
}

async fn h1_client(
    endpoint_address: &SocketAddr,
) -> (
    hyper::client::conn::SendRequest<hyper::Body>,
    tokio::task::JoinHandle<()>,
) {
    let stream =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, endpoint_address, None).await;
    let (sender, conn) = hyper::client::conn::Builder::new()
        .http2_only(false)
        .handshake::<_, hyper::Body>(stream)
        .await
        .unwrap();
    let conn = tokio::spawn(async move {
        let _ = conn.await;
    });
    (sender, conn)
}

async fn send_get_h1(
    sender: &mut hyper::client::conn::SendRequest<hyper::Body>,
    endpoint_address: &SocketAddr,
) -> (http::response::Parts, Bytes) {
    let response = sender
        .send_request(
            hyper::Request::get(format!(
                "https://{}:{}/hello/haha",
                common::MAIN_DOMAIN_NAME,
                endpoint_address.port()
            ))
            .version(http::Version::HTTP_11)
            .body(hyper::Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    (parts, hyper::body::to_bytes(body).await.unwrap())
}

fn run_endpoint_in_background(
    endpoint_address: &SocketAddr,
    proxy_address: &SocketAddr,
    allow_private_network_connections: bool,
) -> tokio::task::JoinHandle<()> {
    let endpoint_address = *endpoint_address;
    let proxy_address = *proxy_address;
    tokio::spawn(async move {
        run_endpoint(
            &endpoint_address,
            &proxy_address,
            allow_private_network_connections,
        )
        .await
    })
}

async fn run_keep_alive_test(
    client_task: impl Future<Output = ()> + Send + 'static,
    endpoint_task: tokio::task::JoinHandle<()>,
    proxy_task: impl Future<Output = ()> + Send + 'static,
) {
    let proxy_task = tokio::spawn(proxy_task);
    let test = tokio::time::timeout(Duration::from_secs(15), client_task).await;
    endpoint_task.abort();
    proxy_task.abort();
    test.expect("Timed out");
}

// The size line of a chunk never ends. The buffer of the body grows with
// every byte of it, so the endpoint has to stop waiting for the line and to
// answer with a 400.
#[tokio::test]
async fn chunked_body_with_a_too_long_size_line() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_echo_lenient();

    let client_task = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream =
            common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &endpoint_address, None)
                .await;

        stream
            .write_all(
                format!(
                    "POST /hello/haha HTTP/1.1\r\nHost: {}:{}\r\nTransfer-Encoding: chunked\r\n\r\n",
                    common::MAIN_DOMAIN_NAME,
                    endpoint_address.port()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // The size of the chunk is not over: the line grows and grows.
        stream.write_all(&[b'z'; 4096]).await.unwrap();
        stream.flush().await.unwrap();

        let status = read_status_line(&mut stream).await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "unexpected answer: {}",
            status
        );
        assert_connection_closed(&mut stream).await;
    };
    let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, true);

    tokio::pin!(client_task);
    tokio::pin!(proxy_task);
    tokio::pin!(endpoint_task);

    tokio::select! {
        _ = &mut endpoint_task => unreachable!(),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
        _ = &mut client_task => (),
        _ = &mut proxy_task => {
            tokio::select! {
                _ = client_task => (),
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    panic!("Client timed out after proxy completed")
                }
            }
        },
    }
}

// The same with the trailers: they are the last part of the body, and they
// are not over here either.
#[tokio::test]
async fn chunked_body_with_too_long_trailers() {
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();
    let (proxy_address, proxy_task) = run_proxy_echo_lenient();

    let client_task = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream =
            common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &endpoint_address, None)
                .await;

        stream
            .write_all(
                format!(
                    "POST /hello/haha HTTP/1.1\r\nHost: {}:{}\r\nTransfer-Encoding: chunked\r\n\r\n",
                    common::MAIN_DOMAIN_NAME,
                    endpoint_address.port()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // The last chunk is there, the trailers after it are not over.
        stream.write_all(b"0\r\n").await.unwrap();
        stream.write_all(&[b'z'; 16 * 1024]).await.unwrap();
        stream.flush().await.unwrap();

        let status = read_status_line(&mut stream).await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "unexpected answer: {}",
            status
        );
        assert_connection_closed(&mut stream).await;
    };
    let endpoint_task = run_endpoint(&endpoint_address, &proxy_address, true);

    tokio::pin!(client_task);
    tokio::pin!(proxy_task);
    tokio::pin!(endpoint_task);

    tokio::select! {
        _ = &mut endpoint_task => unreachable!(),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("Timed out"),
        _ = &mut client_task => (),
        _ = &mut proxy_task => {
            tokio::select! {
                _ = client_task => (),
                _ = &mut endpoint_task => unreachable!(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    panic!("Client timed out after proxy completed")
                }
            }
        },
    }
}

// The source of a chunked response never ends the size line of a chunk. The
// buffer of the endpoint would grow with every byte, so it has to cut the
// response and close both connections: the source can not write for long and
// the client gets the end of the stream.
#[tokio::test]
async fn chunked_response_with_a_too_long_size_line() {
    run_endless_chunked_response_test(b"5\r\nhello\r\n", b'z').await;
}

// The same with the trailers after the last chunk.
#[tokio::test]
async fn chunked_response_with_too_long_trailers() {
    run_endless_chunked_response_test(b"0\r\n", b'x').await;
}

async fn run_endless_chunked_response_test(prefix: &'static [u8], filler: u8) {
    const LIMIT_BYTES: usize = 1024 * 1024;
    common::set_up_logger();
    let endpoint_address = common::make_endpoint_address();

    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let proxy_address = server.local_addr().unwrap();
    let written = Arc::new(Mutex::new(0usize));
    let written_for_task = written.clone();
    let proxy_task = async move {
        let listener = TcpListener::from_std(server).unwrap();
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0u8; 4096];
        let mut received = Vec::new();
        while !received.windows(4).any(|w| w == b"\r\n\r\n") {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(n) => received.extend_from_slice(&buffer[..n]),
            }
        }
        let mut head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        head.extend_from_slice(prefix);
        if socket.write_all(&head).await.is_err() {
            return;
        }
        // The line or the trailers never end: no CRLF.
        let piece = vec![filler; 1024];
        for _ in 0..(32 * LIMIT_BYTES / piece.len()) {
            if socket.write_all(&piece).await.is_err() {
                break;
            }
            *written_for_task.lock().unwrap() += piece.len();
            tokio::task::yield_now().await;
        }
    };

    let client_endpoint = endpoint_address;
    let client_task = async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream =
            common::establish_tls_connection(common::MAIN_DOMAIN_NAME, &client_endpoint, None)
                .await;
        stream
            .write_all(
                format!(
                    "GET /hello/haha HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
                    common::MAIN_DOMAIN_NAME,
                    client_endpoint.port()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // The endpoint closes the connection instead of waiting for the end.
        let mut rest = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut rest)).await;
        assert!(read.is_ok(), "the connection of the client was not closed");
    };
    let endpoint_task = run_endpoint_in_background(&endpoint_address, &proxy_address, true);

    run_keep_alive_test(client_task, endpoint_task, proxy_task).await;
    let written = *written.lock().unwrap();
    assert!(
        written < LIMIT_BYTES,
        "the source wrote {} bytes: the endpoint kept reading",
        written
    );
}

// The first line of the answer of the endpoint.
async fn read_status_line(stream: &mut (impl AsyncRead + Unpin)) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await.unwrap();
        if n == 0 {
            break;
        }
        line.push(byte[0]);
        if line.ends_with(b"\r\n") {
            break;
        }
    }

    String::from_utf8_lossy(&line).to_string()
}

// The endpoint closes the connection after a broken body: the client gets
// the end of the stream instead of waiting for it.
async fn assert_connection_closed(stream: &mut (impl AsyncRead + Unpin)) {
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
    assert!(read.is_ok(), "the connection was not closed");
}

// Reads the answer until the needle is there. The answer does not have to
// end: a chunked response ends at the last chunk, not at the EOF.
async fn read_until(
    stream: &mut (impl AsyncRead + Unpin),
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let mut received = Vec::new();
    let read = tokio::time::timeout(timeout, async {
        let mut buffer = [0u8; 4096];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..read]);
            if received.windows(needle.len()).any(|w| w == needle) {
                break;
            }
        }
    });
    read.await.expect("the answer did not come in time");
    received
}

// An upstream that answers with a chunked gzip body and then keeps the
// connection open, as nginx does while keepalive_timeout runs. The headers of
// every request it got are kept for the test.
fn run_upstream_chunked_keep_open() -> (
    SocketAddr,
    impl Future<Output = ()>,
    Arc<Mutex<Vec<String>>>,
) {
    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let server_addr = server.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_task = seen.clone();
    let task = async move {
        let listener = TcpListener::from_std(server).unwrap();
        loop {
            let Ok((mut socket, peer)) = listener.accept().await else {
                return;
            };
            info!("New connection from {}", peer);
            let seen = seen_for_task.clone();
            tokio::spawn(async move {
                let _ = serve_chunked_keep_open(&mut socket, seen).await;
            });
        }
    };
    (server_addr, task, seen)
}

async fn serve_chunked_keep_open(
    socket: &mut tokio::net::TcpStream,
    seen: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut buffer = [0u8; 4096];
    let mut received = Vec::new();
    let header_end = loop {
        if let Some(pos) = received.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let read = socket.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        received.extend_from_slice(&buffer[..read]);
    };

    let headers = String::from_utf8_lossy(&received[..header_end]).to_string();
    let lower = headers.to_ascii_lowercase();
    seen.lock().unwrap().push(headers);

    // The body of the request, if any. A real upstream answers only after it.
    let mut rest = received[header_end..].to_vec();
    if let Some(value) = lower
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
    {
        let mut remaining = value.trim().parse::<usize>().unwrap_or(0);
        remaining = remaining.saturating_sub(rest.len());
        while remaining > 0 {
            let read = socket.read(&mut buffer).await?;
            if read == 0 {
                return Ok(());
            }
            remaining = remaining.saturating_sub(read);
        }
    } else if lower.contains("transfer-encoding: chunked") {
        while !rest.windows(5).any(|w| w == b"0\r\n\r\n") {
            let read = socket.read(&mut buffer).await?;
            if read == 0 {
                return Ok(());
            }
            rest.extend_from_slice(&buffer[..read]);
        }
    }

    let mut response = Vec::new();
    response.extend_from_slice(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/html\r\n\
          Content-Encoding: gzip\r\n\
          Transfer-Encoding: chunked\r\n\
          Trailer: X-Upstream-Trailer\r\n\
          \r\n",
    );
    for piece in GZIP_BODY.chunks(50) {
        response.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        response.extend_from_slice(piece);
        response.extend_from_slice(b"\r\n");
    }
    response.extend_from_slice(b"0\r\nX-Upstream-Trailer: yes\r\n\r\n");
    socket.write_all(&response).await?;
    socket.flush().await?;

    // The source does not close the connection after the response: this is
    // what nginx does while keepalive_timeout runs. Reading ends when the
    // endpoint closes its side.
    loop {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return Ok(()),
            Ok(_) => (),
        }
    }
}

async fn sni_h1_client(endpoint_address: &SocketAddr) -> (http::response::Parts, Bytes) {
    let stream = common::establish_tls_connection(
        &format!("hello.{}", common::MAIN_DOMAIN_NAME),
        endpoint_address,
        None,
    )
    .await;

    common::do_get_request(
        stream,
        http::Version::HTTP_11,
        &format!(
            "https://hello.{}:{}",
            common::MAIN_DOMAIN_NAME,
            endpoint_address.port()
        ),
        &[],
    )
    .await
}

async fn sni_h2_client(endpoint_address: &SocketAddr) -> (http::response::Parts, Bytes) {
    let stream = common::establish_tls_connection(
        &format!("hello.{}", common::MAIN_DOMAIN_NAME),
        endpoint_address,
        Some(net_utils::HTTP2_ALPN.as_bytes()),
    )
    .await;

    common::do_get_request(
        stream,
        http::Version::HTTP_2,
        &format!(
            "https://hello.{}:{}",
            common::MAIN_DOMAIN_NAME,
            endpoint_address.port()
        ),
        &[],
    )
    .await
}

async fn sni_h3_client(endpoint_address: &SocketAddr) -> (http::response::Parts, Bytes) {
    let mut conn = common::Http3Session::connect(
        endpoint_address,
        &format!("hello.{}", common::MAIN_DOMAIN_NAME),
        None,
    )
    .await;

    conn.exchange(
        Request::get(format!(
            "https://hello.{}:{}",
            common::MAIN_DOMAIN_NAME,
            endpoint_address.port()
        ))
        .body(hyper::Body::empty())
        .unwrap(),
    )
    .await
}

async fn path_h1_client(endpoint_address: &SocketAddr) -> (http::response::Parts, Bytes) {
    let stream =
        common::establish_tls_connection(common::MAIN_DOMAIN_NAME, endpoint_address, None).await;

    common::do_get_request(
        stream,
        http::Version::HTTP_11,
        &format!(
            "https://{}:{}/hello/haha",
            common::MAIN_DOMAIN_NAME,
            endpoint_address.port()
        ),
        &[(http::header::UPGRADE.as_str(), "1")],
    )
    .await
}

async fn path_h2_client(endpoint_address: &SocketAddr) -> (http::response::Parts, Bytes) {
    let stream = common::establish_tls_connection(
        common::MAIN_DOMAIN_NAME,
        endpoint_address,
        Some(net_utils::HTTP2_ALPN.as_bytes()),
    )
    .await;

    common::do_get_request(
        stream,
        http::Version::HTTP_2,
        &format!(
            "https://{}:{}/hello/haha",
            common::MAIN_DOMAIN_NAME,
            endpoint_address.port()
        ),
        &[],
    )
    .await
}

async fn path_h3_client(endpoint_address: &SocketAddr) -> (http::response::Parts, Bytes) {
    let mut conn =
        common::Http3Session::connect(endpoint_address, common::MAIN_DOMAIN_NAME, None).await;

    conn.exchange(
        Request::get(format!(
            "https://{}:{}/hello/haha",
            common::MAIN_DOMAIN_NAME,
            endpoint_address.port()
        ))
        .body(hyper::Body::empty())
        .unwrap(),
    )
    .await
}

async fn run_endpoint(
    endpoint_address: &SocketAddr,
    proxy_address: &SocketAddr,
    allow_private_network_connections: bool,
) {
    let settings = Settings::builder()
        .listen_address(endpoint_address)
        .unwrap()
        .listen_protocols(ListenProtocolSettings {
            http1: Some(Http1Settings::builder().build()),
            http2: Some(Http2Settings::builder().build()),
            quic: Some(QuicSettings::builder().build()),
        })
        .reverse_proxy(
            ReverseProxySettings::builder()
                .server_address(proxy_address)
                .unwrap()
                .path_mask("/hello".to_string())
                .build()
                .unwrap(),
        )
        .allow_private_network_connections(allow_private_network_connections)
        .build()
        .unwrap();

    let cert_key_file = common::make_cert_key_file();
    let cert_key_path = cert_key_file.path.to_str().unwrap();
    let hosts_settings = TlsHostsSettings::builder()
        .main_hosts(vec![TlsHostInfo {
            hostname: common::MAIN_DOMAIN_NAME.to_string(),
            cert_chain_path: cert_key_path.to_string(),
            private_key_path: cert_key_path.to_string(),
            allowed_sni: vec![],
        }])
        .reverse_proxy_hosts(vec![TlsHostInfo {
            hostname: format!("hello.{}", common::MAIN_DOMAIN_NAME),
            cert_chain_path: cert_key_path.to_string(),
            private_key_path: cert_key_path.to_string(),
            allowed_sni: vec![],
        }])
        .build()
        .unwrap();

    common::run_endpoint_with_settings(settings, hosts_settings).await;
}

fn run_proxy() -> (SocketAddr, impl Future<Output = ()>) {
    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let server_addr = server.local_addr().unwrap();
    (server_addr, async move {
        let (socket, peer) = TcpListener::from_std(server)
            .unwrap()
            .accept()
            .await
            .unwrap();
        info!("New connection from {}", peer);
        hyper::server::conn::Http::new()
            .http1_only(true)
            .serve_connection(socket, hyper::service::service_fn(request_handler))
            .await
            .unwrap();
    })
}

// The endpoint opens a new connection to the upstream for every request, so a
// client that keeps its connection needs a proxy that accepts more than one.
fn run_proxy_multi() -> (SocketAddr, impl Future<Output = ()>) {
    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let server_addr = server.local_addr().unwrap();
    (server_addr, async move {
        let listener = TcpListener::from_std(server).unwrap();
        loop {
            let (socket, peer) = listener.accept().await.unwrap();
            info!("New connection from {}", peer);
            tokio::spawn(async move {
                let _ = hyper::server::conn::Http::new()
                    .http1_only(true)
                    .serve_connection(socket, hyper::service::service_fn(request_handler))
                    .await;
            });
        }
    })
}

fn run_proxy_echo_multi() -> (SocketAddr, impl Future<Output = ()>) {
    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let server_addr = server.local_addr().unwrap();
    (server_addr, async move {
        let listener = TcpListener::from_std(server).unwrap();
        loop {
            let (socket, peer) = listener.accept().await.unwrap();
            info!("New connection from {}", peer);
            tokio::spawn(async move {
                let _ = hyper::server::conn::Http::new()
                    .http1_only(true)
                    .serve_connection(socket, hyper::service::service_fn(request_handler_echo))
                    .await;
            });
        }
    })
}

// A proxy that answers with the size of the body it got: it has to read the
// whole body before it answers, like a real one does.
fn run_proxy_echo() -> (SocketAddr, impl Future<Output = ()>) {
    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let server_addr = server.local_addr().unwrap();
    (server_addr, async move {
        let (socket, peer) = TcpListener::from_std(server)
            .unwrap()
            .accept()
            .await
            .unwrap();
        info!("New connection from {}", peer);
        hyper::server::conn::Http::new()
            .http1_only(true)
            .serve_connection(socket, hyper::service::service_fn(request_handler_echo))
            .await
            .unwrap();
    })
}

// The same, but the error of the connection is not a failure: the endpoint
// closes the connection when the body of the request is broken, and then the
// body never ends and the proxy has nothing to answer.
fn run_proxy_echo_lenient() -> (SocketAddr, impl Future<Output = ()>) {
    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let server_addr = server.local_addr().unwrap();
    (server_addr, async move {
        let (socket, peer) = TcpListener::from_std(server)
            .unwrap()
            .accept()
            .await
            .unwrap();
        info!("New connection from {}", peer);
        let _ = hyper::server::conn::Http::new()
            .http1_only(true)
            .serve_connection(
                socket,
                hyper::service::service_fn(request_handler_echo_lenient),
            )
            .await;
    })
}

fn run_proxy_chunked() -> (SocketAddr, impl Future<Output = ()>) {
    let server = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let _ = server.set_nonblocking(true);
    let server_addr = server.local_addr().unwrap();
    (server_addr, async move {
        let (socket, peer) = TcpListener::from_std(server)
            .unwrap()
            .accept()
            .await
            .unwrap();
        info!("New connection from {}", peer);
        hyper::server::conn::Http::new()
            .http1_only(true)
            .serve_connection(socket, hyper::service::service_fn(request_handler_chunked))
            .await
            .unwrap();
    })
}

async fn request_handler(
    request: Request<hyper::Body>,
) -> Result<Response<hyper::Body>, hyper::Error> {
    info!("Received request: {:?}", request);
    Ok(Response::builder()
        .body(hyper::Body::from(RESPONSE_BODY.clone()))
        .unwrap())
}

// Answers with the size of the body it got. The body is read to the end: a
// proxy that answers before the body is over would hide the bug.
async fn request_handler_echo(
    request: Request<hyper::Body>,
) -> Result<Response<hyper::Body>, hyper::Error> {
    info!("Received request: {:?}", request);
    let body = hyper::body::to_bytes(request.into_body()).await.unwrap();
    Ok(Response::builder()
        .body(hyper::Body::from(format!("{}", body.len())))
        .unwrap())
}

// The same, but a body that ends with an error is not a failure: the body of
// a broken request never ends, and the endpoint closes the connection then.
async fn request_handler_echo_lenient(
    request: Request<hyper::Body>,
) -> Result<Response<hyper::Body>, hyper::Error> {
    info!("Received request: {:?}", request);
    let body = match hyper::body::to_bytes(request.into_body()).await {
        Ok(body) => body,
        Err(e) => {
            info!("The body is not complete: {}", e);
            return Ok(Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(hyper::Body::empty())
                .unwrap());
        }
    };
    Ok(Response::builder()
        .body(hyper::Body::from(format!("{}", body.len())))
        .unwrap())
}

async fn request_handler_chunked(
    request: Request<hyper::Body>,
) -> Result<Response<hyper::Body>, hyper::Error> {
    info!("Received request: {:?}", request);
    let chunk_size = 16 * 1024;
    let chunks = RESPONSE_BODY
        .chunks(chunk_size)
        .map(|c| Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(c)))
        .collect::<Vec<_>>();

    Ok(Response::builder()
        .body(hyper::Body::wrap_stream(stream::iter(chunks)))
        .unwrap())
}
