use crate::{log_utils, net_utils, tls_demultiplexer};
use rustls::crypto::aws_lc_rs;
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tls_parser::{
    parse_tls_message_handshake, parse_tls_raw_record, TlsMessage, TlsMessageHandshake,
    TlsRecordType,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_rustls::{LazyConfigAcceptor, StartHandshake};

pub(crate) struct TlsListener {}

pub(crate) struct TlsAcceptor<IO = TcpStream> {
    inner: StartHandshake<PrebufferedTcpStream<IO>>,
    client_random: Option<Vec<u8>>,
}

impl TlsListener {
    pub fn new() -> Self {
        Self {}
    }

    /// Read the ClientHello from a TCP connection or from the inner stream of
    /// CatTail.
    pub async fn listen<IO>(&self, stream: IO) -> io::Result<TlsAcceptor<IO>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let (stream, client_random) = Self::read_client_random_and_wrap_stream(stream).await?;

        // Now let rustls handle the stream normally
        LazyConfigAcceptor::new(rustls::server::Acceptor::default(), stream)
            .await
            .map(|hs| TlsAcceptor {
                inner: hs,
                client_random,
            })
    }

    async fn read_client_random_and_wrap_stream<IO>(
        mut stream: IO,
    ) -> io::Result<(PrebufferedTcpStream<IO>, Option<Vec<u8>>)>
    where
        IO: AsyncRead + Unpin,
    {
        let mut client_random = None;
        let mut prebuffer: Vec<u8> = Vec::new();
        const MAX_PREBUFFER_LEN: usize = 16 * 1024;
        const READ_CHUNK_LEN: usize = 1024;

        while prebuffer.len() < MAX_PREBUFFER_LEN {
            match Self::extract_client_random(&prebuffer) {
                ClientRandomExtraction::Found(cr) => {
                    client_random = Some(cr);
                    break;
                }
                ClientRandomExtraction::NotFound => break,
                ClientRandomExtraction::NeedMoreData => {}
            }

            let remaining = MAX_PREBUFFER_LEN - prebuffer.len();
            let read_len = READ_CHUNK_LEN.min(remaining);
            let mut tmp = vec![0u8; read_len];
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            prebuffer.extend_from_slice(&tmp[..n]);
        }

        Ok((PrebufferedTcpStream::new(prebuffer, stream), client_random))
    }

    /// The ClientHello may span several TLS records (RFC 8446 5.1 allows
    /// fragmenting handshake messages; CatTunnel's anti-DPI splits it on
    /// purpose). Handshake fragments are joined until the whole message is
    /// in, otherwise the prebuffer loop would wait for bytes the client never
    /// sends - it is waiting for our ServerHello.
    fn extract_client_random(data: &[u8]) -> ClientRandomExtraction {
        const HANDSHAKE: u8 = 0x16;
        if data.is_empty() {
            return ClientRandomExtraction::NeedMoreData;
        }
        if data[0] != HANDSHAKE {
            return ClientRandomExtraction::NotFound;
        }

        let mut handshake = Vec::new();
        let mut rest = data;
        loop {
            match parse_tls_raw_record(rest) {
                Ok((next, raw)) => {
                    if raw.hdr.record_type != TlsRecordType::Handshake {
                        return ClientRandomExtraction::NotFound;
                    }
                    handshake.extend_from_slice(raw.data);
                    rest = next;
                }
                Err(tls_parser::Err::Incomplete(_)) => return ClientRandomExtraction::NeedMoreData,
                Err(e) => {
                    log::debug!("Failed to parse TLS record: {:?}", e);
                    return ClientRandomExtraction::NotFound;
                }
            }

            if handshake.len() < 4 {
                continue;
            }
            let msg_len =
                u32::from_be_bytes([0, handshake[1], handshake[2], handshake[3]]) as usize;
            if handshake.len() < 4 + msg_len {
                continue;
            }
            return match parse_tls_message_handshake(&handshake[..4 + msg_len]) {
                Ok((_, TlsMessage::Handshake(TlsMessageHandshake::ClientHello(hello))))
                    if hello.random.len() >= 32 =>
                {
                    // The fingerprint of the client, so we know how many of each
                    // kind we serve.
                    let ciphers: Vec<u16> = hello.ciphers.iter().copied().map(u16::from).collect();
                    let ja4 = crate::ja4::compute(&crate::ja4::ClientHelloInfo {
                        version: hello.get_version().0,
                        ciphers: &ciphers,
                        extensions: hello.ext.unwrap_or_default(),
                    });
                    crate::ja4::count(&ja4);
                    log::debug!("ClientHello fingerprint: {}", ja4);

                    ClientRandomExtraction::Found(hello.random[..32].to_vec())
                }
                Ok(_) => ClientRandomExtraction::NotFound,
                Err(e) => {
                    log::debug!("Failed to parse ClientHello: {:?}", e);
                    ClientRandomExtraction::NotFound
                }
            };
        }
    }
}

enum ClientRandomExtraction {
    Found(Vec<u8>),
    NeedMoreData,
    NotFound,
}

pub(crate) struct PrebufferedTcpStream<IO = TcpStream> {
    prebuffer: Vec<u8>,
    prebuffer_pos: usize,
    stream: IO,
}

impl<IO> std::fmt::Debug for PrebufferedTcpStream<IO> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrebufferedTcpStream")
            .field("prebuffer_len", &self.prebuffer.len())
            .field("prebuffer_pos", &self.prebuffer_pos)
            .finish()
    }
}

impl<IO> PrebufferedTcpStream<IO> {
    fn new(prebuffer: Vec<u8>, stream: IO) -> Self {
        Self {
            prebuffer,
            prebuffer_pos: 0,
            stream,
        }
    }
}

impl<IO: net_utils::PeerAddr> net_utils::PeerAddr for PrebufferedTcpStream<IO> {
    fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.stream.peer_addr()
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for PrebufferedTcpStream<IO> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.prebuffer_pos < self.prebuffer.len() {
            let available = self.prebuffer.len() - self.prebuffer_pos;
            let to_copy = available.min(buf.remaining());
            let start = self.prebuffer_pos;
            let end = start + to_copy;
            buf.put_slice(&self.prebuffer[start..end]);
            self.prebuffer_pos = end;
            return Poll::Ready(Ok(()));
        }

        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for PrebufferedTcpStream<IO> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> TlsAcceptor<IO> {
    pub fn sni(&self) -> Option<String> {
        self.inner.client_hello().server_name().map(String::from)
    }

    pub fn alpn(&self) -> Vec<Vec<u8>> {
        self.inner
            .client_hello()
            .alpn()
            .map(|x| x.map(Vec::from).collect())
            .unwrap_or_default()
    }

    pub fn client_random(&self) -> Option<Vec<u8>> {
        self.client_random.clone()
    }

    pub async fn accept(
        self,
        protocol: tls_demultiplexer::Protocol,
        cert_chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        _log_id: &log_utils::IdChain<u64>,
    ) -> io::Result<TlsStream<PrebufferedTcpStream<IO>>> {
        let tls_config = {
            let mut provider = aws_lc_rs::default_provider();
            provider.kx_groups = vec![
                aws_lc_rs::kx_group::X25519MLKEM768,
                aws_lc_rs::kx_group::X25519,
                aws_lc_rs::kx_group::SECP256R1,
                aws_lc_rs::kx_group::SECP384R1,
            ];
            let mut cfg = ServerConfig::builder_with_provider(Arc::new(provider))
                .with_safe_default_protocol_versions()
                .map_err(|e| io::Error::other(format!("TLS config error: {}", e)))?
                .with_no_client_auth()
                .with_single_cert(cert_chain, key)
                .map_err(|e| {
                    io::Error::other(format!("Failed to create TLS configuration: {}", e))
                })?;

            cfg.alpn_protocols = vec![protocol.as_alpn().as_bytes().to_vec()];
            Arc::new(cfg)
        };

        self.inner.into_stream(tls_config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ClientHello handshake message (no record header).
    fn client_hello(random: [u8; 32]) -> Vec<u8> {
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&random);
        body.push(0); // session id
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // one cipher suite
        body.extend_from_slice(&[0x01, 0x00]); // null compression
        body.extend_from_slice(&[0x00, 0x00]); // no extensions
        let mut msg = vec![0x01];
        msg.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        msg.extend_from_slice(&body);
        msg
    }

    fn record(fragment: &[u8]) -> Vec<u8> {
        let mut r = vec![0x16, 0x03, 0x01];
        r.extend_from_slice(&(fragment.len() as u16).to_be_bytes());
        r.extend_from_slice(fragment);
        r
    }

    fn found(e: ClientRandomExtraction) -> Option<Vec<u8>> {
        match e {
            ClientRandomExtraction::Found(r) => Some(r),
            _ => None,
        }
    }

    #[test]
    fn client_random_from_one_record() {
        let random = [7u8; 32];
        let data = record(&client_hello(random));
        assert_eq!(
            found(TlsListener::extract_client_random(&data)),
            Some(random.to_vec())
        );
    }

    /// A ClientHello split across several TLS records (allowed by RFC 8446
    /// 5.1; CatTunnel's anti-DPI does it) must not stall the prebuffer loop.
    #[test]
    fn client_random_from_split_records() {
        let random = [9u8; 32];
        let hello = client_hello(random);
        for cut in [1, 3, 4, 10, 30, hello.len() - 1] {
            let mut data = record(&hello[..cut]);
            data.extend_from_slice(&record(&hello[cut..]));
            assert_eq!(
                found(TlsListener::extract_client_random(&data)),
                Some(random.to_vec()),
                "cut at {cut}"
            );
            // only the first record arrived so far: wait for more, don't give up
            assert!(matches!(
                TlsListener::extract_client_random(&record(&hello[..cut])),
                ClientRandomExtraction::NeedMoreData
            ));
        }
        let mut three = record(&hello[..5]);
        three.extend_from_slice(&record(&hello[5..20]));
        three.extend_from_slice(&record(&hello[20..]));
        assert_eq!(
            found(TlsListener::extract_client_random(&three)),
            Some(random.to_vec())
        );
    }

    #[test]
    fn not_tls_is_not_found() {
        assert!(matches!(
            TlsListener::extract_client_random(b"GET / HTTP/1.1\r\n\r\n"),
            ClientRandomExtraction::NotFound
        ));
    }
}
