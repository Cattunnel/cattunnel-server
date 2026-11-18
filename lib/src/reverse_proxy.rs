use crate::forwarder::TcpConnector;
use crate::http_codec::HttpCodec;
use crate::net_utils::TcpDestination;
use crate::tcp_forwarder::TcpForwarder;
use crate::tls_demultiplexer::Protocol;
use crate::{
    core, forwarder, http1_codec, http_codec, http_forwarded_stream, log_id, log_utils, pipe,
    tunnel,
};
use bytes::{Buf, BufMut, BytesMut};
use std::io;
use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

static ORIGINAL_PROTOCOL_HEADER: http::HeaderName =
    http::HeaderName::from_static("x-original-protocol");
const H3_BUFFERED_BODY_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct SessionManager {
    active_streams_num: AtomicUsize,
}

pub(crate) async fn listen(
    context: Arc<core::Context>,
    mut codec: Box<dyn HttpCodec>,
    sni: String,
    log_id: log_utils::IdChain<u64>,
) {
    let (mut shutdown_notification, _shutdown_completion) = {
        let shutdown = context.shutdown.lock().unwrap();
        (shutdown.notification_handler(), shutdown.completion_guard())
    };

    tokio::select! {
        x = shutdown_notification.wait() => {
            match x {
                Ok(_) => (),
                Err(e) => log_id!(debug, log_id, "Shutdown notification failure: {}", e),
            }
        },
        _ = listen_inner(context, codec.as_mut(), sni, &log_id) => (),
    }

    if let Err(e) = codec.graceful_shutdown().await {
        log_id!(debug, log_id, "Failed to shutdown HTTP session: {}", e);
    }
}

async fn listen_inner(
    context: Arc<core::Context>,
    codec: &mut dyn HttpCodec,
    sni: String,
    log_id: &log_utils::IdChain<u64>,
) {
    let manager = Arc::new(SessionManager::default());
    let timeout = context.settings.connection_establishment_timeout;
    loop {
        match tokio::time::timeout(timeout, codec.listen()).await {
            Ok(Ok(Some(x))) => {
                tokio::spawn({
                    let context = context.clone();
                    let manager = manager.clone();
                    let protocol = codec.protocol();
                    let sni = sni.clone();
                    let log_id = log_id.clone();
                    async move {
                        manager.active_streams_num.fetch_add(1, Ordering::AcqRel);
                        if let Err(e) = handle_stream(context, x, protocol, sni, &log_id).await {
                            log_id!(debug, log_id, "Request failed: {}", e);
                        }
                        manager.active_streams_num.fetch_sub(1, Ordering::AcqRel);
                    }
                });
            }
            Ok(Ok(None)) => {
                log_id!(trace, log_id, "Connection closed");
                break;
            }
            Ok(Err(ref e)) if e.kind() == ErrorKind::UnexpectedEof => {
                log_id!(trace, log_id, "Connection closed");
                break;
            }
            Ok(Err(e)) => {
                log_id!(debug, log_id, "Session error: {}", e);
                break;
            }
            Err(_elapsed) if manager.active_streams_num.load(Ordering::Acquire) > 0 => log_id!(
                trace,
                log_id,
                "Ignoring timeout due to there are some active streams"
            ),
            Err(_elapsed) => {
                log_id!(debug, log_id, "Closing due to timeout");
                if let Err(e) = codec.graceful_shutdown().await {
                    log_id!(debug, log_id, "Failed to shut down session: {}", e);
                }
                break;
            }
        }
    }
}

async fn handle_stream(
    context: Arc<core::Context>,
    stream: Box<dyn http_codec::Stream>,
    protocol: Protocol,
    sni: String,
    log_id: &log_utils::IdChain<u64>,
) -> io::Result<()> {
    let (request, respond) = stream.split();
    log_id!(trace, log_id, "Received request: {:?}", request.request());

    let forwarder = Box::new(TcpForwarder::new_for_configured_destination(
        context.clone(),
    ));
    let settings = context.settings.reverse_proxy.as_ref().unwrap();
    let (mut server_source, mut server_sink) = forwarder
        .connect(
            log_id.clone(),
            forwarder::TcpConnectionMeta {
                client_address: Ipv4Addr::UNSPECIFIED.into(),
                destination: TcpDestination::Address(settings.server_address),
                auth: None,
                tls_domain: sni,
                user_agent: None,
            },
        )
        .await
        .map_err(|e| match e {
            tunnel::ConnectionError::Io(e) => e,
            _ => io::Error::other(format!("{}", e)),
        })?;

    let mut request_headers = request.clone_request();
    let original_version = request_headers.version;
    match protocol {
        Protocol::Http1 => (),
        Protocol::Http2 => {
            request_headers.version = http::Version::HTTP_11;
        }
        Protocol::Http3 => {
            request_headers.version = http::Version::HTTP_11;
            if settings.h3_backward_compatibility
                && request_headers.method == http::Method::GET
                && request_headers.uri.path() == "/"
            {
                request_headers.method = http::Method::CONNECT;
            }
        }
    }
    request_headers.headers.insert(
        &ORIGINAL_PROTOCOL_HEADER,
        http::HeaderValue::from_static(protocol.as_str()),
    );

    // The endpoint opens a new connection to the upstream for every request,
    // so there is nothing for the upstream to keep it for. With
    // "Connection: close" the upstream closes it right after the response,
    // and the end of a body without a length of its own comes at once instead
    // of after the keep-alive timeout of the upstream (65 seconds at nginx by
    // default). The bodies with a length are read by the length and the
    // chunked ones by the last chunk, so the close is only the last resort.
    request_headers.headers.remove(http::header::CONNECTION);
    request_headers
        .headers
        .remove(http::HeaderName::from_static("keep-alive"));
    request_headers
        .headers
        .remove(http::HeaderName::from_static("proxy-connection"));
    request_headers.headers.insert(
        http::header::CONNECTION,
        http::HeaderValue::from_static("close"),
    );

    // How long the body is, as the client says. For HTTP/2 and HTTP/3 a
    // request without a length has a body of frames, it ends with the stream.
    let body_length =
        http_forwarded_stream::body_length(&request_headers)?.unwrap_or(match protocol {
            Protocol::Http2 | Protocol::Http3 => http_forwarded_stream::BodyLength::Unknown,
            _ => http_forwarded_stream::BodyLength::Determined(0),
        });

    let encoded = http1_codec::encode_request(&request_headers);
    log_id!(
        trace,
        log_id,
        "Sending translated request: {:?}",
        request_headers
    );
    server_sink.write_all(encoded).await?;

    // The upstream does not answer until it has the whole body, so the body
    // has to be on its way while we wait for the response. Before this the
    // body was sent only after the response had come, and any request with a
    // body deadlocked: the upstream waited for the body, we waited for the
    // response, and the upstream gave up with 408 after its body timeout.
    // The task lives until the end of the request: on drop it is aborted, the
    // upstream has answered by then and the rest of the body is not needed.
    //
    // A broken body is reported through the channel: the client is to get a
    // 400, and the answer cannot wait for the upstream, which waits for the
    // body that will never come.
    let (body_error_tx, mut body_error_rx) = mpsc::unbounded_channel();
    let body_done = Arc::new(AtomicBool::new(matches!(
        body_length,
        http_forwarded_stream::BodyLength::Determined(0)
    )));
    let _body_task = BodyTask::spawn(
        request,
        body_length,
        server_sink,
        body_error_tx,
        body_done.clone(),
        log_id.clone(),
    );

    let mut body_failed = false;
    let mut body_error: Option<io::Error> = None;
    let mut buffer = BytesMut::new();
    let upstream_response = loop {
        let read = if body_failed {
            match server_source.read().await {
                Ok(read) => Some(Ok(read)),
                Err(e) => return Err(e),
            }
        } else {
            tokio::select! {
                r = server_source.read() => Some(r),
                e = body_error_rx.recv() => {
                    match e {
                        Some(e) => body_error = Some(e),
                        // The body task is gone without a report: there is
                        // nothing to wait for anymore.
                        None => body_failed = true,
                    }
                    // The upstream is not waited for anymore.
                    None
                }
            }
        };

        let read = match read {
            Some(read) => read?,
            None => break None,
        };

        match read {
            pipe::Data::Chunk(chunk) => {
                server_source.consume(chunk.len())?;
                buffer.put(chunk);
            }
            pipe::Data::Eof => {
                // Upstream closed before sending a valid HTTP response. Reply with 502
                // to avoid surfacing this as an H2 stream cancel to the client.
                return send_bad_gateway(respond, original_version);
            }
        }

        match http1_codec::decode_response(
            buffer,
            http1_codec::MAX_HEADERS_NUM,
            http1_codec::MAX_RAW_HEADERS_SIZE,
        )? {
            http1_codec::DecodeStatus::Partial(b) => buffer = b,
            http1_codec::DecodeStatus::Complete(mut h, tail) => {
                h.version = original_version; // restore the version in case it was not the same
                let transfer_encoding_raw = h
                    .headers
                    .get(http::header::TRANSFER_ENCODING)
                    .and_then(|x| x.to_str().ok())
                    .map(str::to_owned);
                let is_chunked = transfer_encoding_raw
                    .as_deref()
                    .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
                // The connection to the upstream lives one request: its
                // "Connection: close" is about that connection, not about the
                // one to the client. The codec of the client decides the
                // latter from the framing of the response.
                h.headers.remove(http::header::CONNECTION);
                h.headers
                    .remove(http::HeaderName::from_static("keep-alive"));
                h.headers
                    .remove(http::HeaderName::from_static("proxy-connection"));
                if !matches!(protocol, Protocol::Http1) {
                    // Strip hop-by-hop headers that are invalid in HTTP/2 and HTTP/3.
                    h.headers.remove(http::header::TRANSFER_ENCODING);
                    h.headers.remove(http::header::UPGRADE);
                    h.headers.remove(http::header::TE);
                    h.headers.remove(http::header::TRAILER);
                }
                break Some((h, tail.freeze(), is_chunked));
            }
        }
    };

    let (mut response, chunk, is_chunked) = match upstream_response {
        Some(response) => response,
        None => {
            // The body of the request is broken: the client gets a 400 and
            // the connection is closed.
            if let Some(e) = body_error {
                log_id!(debug, log_id, "Malformed request body: {}", e);
            }
            return send_bad_request(respond, original_version);
        }
    };

    // A connection can serve the next request only if the body of this one
    // has been read to its end. When the upstream answers before the body is
    // over, the rest of the body would be parsed as the next request, so the
    // connection is closed after the response instead.
    if matches!(protocol, Protocol::Http1) && !body_done.load(Ordering::Acquire) {
        log_id!(
            debug,
            log_id,
            "Request body is not read yet, closing the connection after the response"
        );
        response.headers.remove(http::header::CONNECTION);
        response.headers.insert(
            http::header::CONNECTION,
            http::HeaderValue::from_static("close"),
        );
    }

    let content_length = response
        .headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.parse::<usize>().ok());
    // H3 streaming is fragile in practice; buffer reasonably small bodies to avoid truncation.
    if matches!(protocol, Protocol::Http3)
        && content_length.is_some_and(|x| x <= H3_BUFFERED_BODY_LIMIT)
    {
        let total = content_length.unwrap();
        let mut body = BytesMut::with_capacity(total);
        let chunk_len = chunk.len();
        body.put(chunk);
        server_source.consume(chunk_len)?;

        let mut remaining = total.saturating_sub(chunk_len);
        while remaining > 0 {
            match server_source.read().await? {
                pipe::Data::Chunk(chunk) => {
                    server_source.consume(chunk.len())?;
                    let to_take = std::cmp::min(chunk.len(), remaining);
                    body.put(chunk.slice(..to_take));
                    remaining -= to_take;
                }
                pipe::Data::Eof => {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
            }
        }

        let mut client_sink = respond.send_response(response, false)?.into_pipe_sink();
        client_sink.write_all(body.freeze()).await?;
        client_sink.eof()?;
        return Ok(());
    }

    let chunk_len = chunk.len();

    if is_chunked && matches!(protocol, Protocol::Http1) {
        async fn need_more(
            buffer: &mut BytesMut,
            server_source: &mut Box<dyn pipe::Source>,
        ) -> io::Result<bool> {
            match server_source.read().await? {
                pipe::Data::Chunk(chunk) => {
                    server_source.consume(chunk.len())?;
                    buffer.put(chunk);
                    Ok(false)
                }
                pipe::Data::Eof => Ok(true),
            }
        }

        // The chunked framing goes to the client as it came: the size line
        // with its extensions, the data and the CRLF after it, the last chunk
        // and the trailers. The end of the response is the last chunk, not
        // the end of the connection of the upstream: the upstream keeps it
        // open (nginx does it for keepalive_timeout, 65 seconds by default),
        // and before this the page waited for that EOF and froze.
        let mut client_sink = respond.send_response(response, false)?.into_pipe_sink();
        let mut buffer = BytesMut::new();
        buffer.put(chunk);
        server_source.consume(chunk_len)?;

        loop {
            let line_end = loop {
                if let Some(pos) = buffer.windows(2).position(|w| w == b"\r\n") {
                    break pos + 2;
                }
                // The line does not end yet: the buffer grows with every
                // byte of the body, so it has to be limited. The same limit
                // as for the body of the request.
                if buffer.len() > http_forwarded_stream::MAX_CHUNK_SIZE_LINE_LEN {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "The size line of a chunk is too long",
                    ));
                }
                if need_more(&mut buffer, &mut server_source).await? {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
            };
            // The line is over: it is a number in hex with an optional
            // extension, so a longer one means a broken body.
            if line_end - 2 > http_forwarded_stream::MAX_CHUNK_SIZE_LINE_LEN {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "The size line of a chunk is too long",
                ));
            }
            let chunk_size = {
                let line = std::str::from_utf8(&buffer[..line_end - 2])
                    .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                let size_hex = line.split(';').next().unwrap_or_default().trim();
                usize::from_str_radix(size_hex, 16)
                    .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?
            };
            client_sink
                .write_all(buffer.split_to(line_end).freeze())
                .await?;

            if chunk_size == 0 {
                // The trailers and the empty line. Without trailers the body
                // ends with the CRLF right after the last chunk.
                let trailer_end = loop {
                    // The trailers grow with every byte, so they have to be
                    // limited. The same limit as for the body of the request.
                    if buffer.len() > http_forwarded_stream::MAX_CHUNK_TRAILERS_LEN {
                        return Err(io::Error::new(
                            ErrorKind::InvalidData,
                            "The trailers of the body are too long",
                        ));
                    }
                    if buffer.starts_with(b"\r\n") {
                        break 2;
                    }
                    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                    if need_more(&mut buffer, &mut server_source).await? {
                        return Err(ErrorKind::UnexpectedEof.into());
                    }
                };
                client_sink
                    .write_all(buffer.split_to(trailer_end).freeze())
                    .await?;
                break;
            }

            // The data of the chunk and the CRLF after it.
            let end = chunk_size
                .checked_add(2)
                .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "chunk size is too big"))?;
            while buffer.len() < end {
                if need_more(&mut buffer, &mut server_source).await? {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
            }
            client_sink.write_all(buffer.split_to(end).freeze()).await?;
        }

        client_sink.eof()?;
        client_sink.flush().await?;
        return Ok(());
    }

    if is_chunked && !matches!(protocol, Protocol::Http1) {
        async fn need_more(
            buffer: &mut BytesMut,
            server_source: &mut Box<dyn pipe::Source>,
            _log_id: &log_utils::IdChain<u64>,
        ) -> io::Result<bool> {
            match server_source.read().await? {
                pipe::Data::Chunk(chunk) => {
                    server_source.consume(chunk.len())?;
                    buffer.put(chunk);
                    Ok(false)
                }
                pipe::Data::Eof => Ok(true),
            }
        }

        // Decode chunked HTTP/1 body and stream raw bytes to the client.
        // For HTTP/2 and HTTP/3, buffer reasonably small responses so we can
        // set Content-Length and avoid relying on connection teardown signals.
        let buffer_for_length = !matches!(protocol, Protocol::Http1);
        let mut buffered_body = BytesMut::new();
        let mut respond_opt = Some(respond);
        let mut response_opt = Some(response);
        let mut client_sink: Option<Box<dyn pipe::Sink>> = None;
        let ensure_client_sink = |response_opt: &mut Option<http_codec::ResponseHeaders>,
                                  respond_opt: &mut Option<Box<dyn http_codec::PendingRespond>>,
                                  client_sink: &mut Option<Box<dyn pipe::Sink>>|
         -> io::Result<()> {
            if client_sink.is_some() {
                return Ok(());
            }
            let response = response_opt
                .take()
                .ok_or_else(|| io::Error::other("missing response"))?;
            let respond = respond_opt
                .take()
                .ok_or_else(|| io::Error::other("missing respond"))?;
            *client_sink = Some(respond.send_response(response, false)?.into_pipe_sink());
            Ok(())
        };

        let mut buffer = BytesMut::new();
        buffer.put(chunk);
        server_source.consume(chunk_len)?;
        loop {
            // Ensure we have a full chunk size line.
            let line_end = loop {
                if let Some(pos) = buffer.windows(2).position(|w| w == b"\r\n").map(|p| p + 2) {
                    break pos;
                }
                let eof = need_more(&mut buffer, &mut server_source, log_id).await?;
                if eof {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
            };

            let mut line = buffer.split_to(line_end);
            line.truncate(line.len().saturating_sub(2));
            let line = std::str::from_utf8(&line)
                .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
            let size_hex = line.split(';').next().unwrap_or_default().trim();
            let chunk_size = usize::from_str_radix(size_hex, 16)
                .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
            if chunk_size == 0 {
                // If there are no trailers, the stream ends with a single CRLF.
                if buffer.len() >= 2 && &buffer[..2] == b"\r\n" {
                    buffer.advance(2);
                } else {
                    // Consume trailers until CRLFCRLF or upstream EOF.
                    loop {
                        if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        let eof = need_more(&mut buffer, &mut server_source, log_id).await?;
                        if eof {
                            break;
                        }
                    }
                }
                if buffer_for_length && client_sink.is_none() {
                    if let Some(resp) = response_opt.as_mut() {
                        resp.headers
                            .insert(http::header::CONTENT_LENGTH, buffered_body.len().into());
                    }
                    ensure_client_sink(&mut response_opt, &mut respond_opt, &mut client_sink)?;
                    if let Some(sink) = client_sink.as_mut() {
                        sink.write_all(buffered_body.split().freeze()).await?;
                    }
                }
                ensure_client_sink(&mut response_opt, &mut respond_opt, &mut client_sink)?;
                if let Some(sink) = client_sink.as_mut() {
                    sink.eof()?;
                }
                return Ok(());
            }

            // Ensure we have the full chunk plus its trailing CRLF.
            while buffer.len() < chunk_size + 2 {
                let eof = need_more(&mut buffer, &mut server_source, log_id).await?;
                if eof {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
            }

            let data = buffer.split_to(chunk_size).freeze();
            let _ = buffer.split_to(2);

            if buffer_for_length && client_sink.is_none() {
                buffered_body.put(data.clone());
                if buffered_body.len() > H3_BUFFERED_BODY_LIMIT {
                    // Fall back to streaming without a known length.
                    ensure_client_sink(&mut response_opt, &mut respond_opt, &mut client_sink)?;
                    if let Some(sink) = client_sink.as_mut() {
                        sink.write_all(buffered_body.split().freeze()).await?;
                    }
                }
                continue;
            }

            ensure_client_sink(&mut response_opt, &mut respond_opt, &mut client_sink)?;
            if let Some(sink) = client_sink.as_mut() {
                sink.write_all(data).await?;
            }
        }
    }

    let mut client_sink = respond.send_response(response, false)?.into_pipe_sink();
    server_source.consume(chunk_len)?;

    // A body with a length ends at its length, whatever the upstream does
    // with the connection after it.
    if let Some(content_length) = content_length {
        log_id!(
            debug,
            log_id,
            "Reverse proxy fixed-size body: {} bytes",
            content_length
        );
        let mut remaining = content_length;
        if !chunk.is_empty() {
            let to_send = std::cmp::min(chunk.len(), remaining);
            client_sink.write_all(chunk.slice(..to_send)).await?;
            remaining -= to_send;
        }
        while remaining > 0 {
            match server_source.read().await? {
                pipe::Data::Chunk(chunk) => {
                    server_source.consume(chunk.len())?;
                    let to_send = std::cmp::min(chunk.len(), remaining);
                    client_sink.write_all(chunk.slice(..to_send)).await?;
                    remaining -= to_send;
                }
                pipe::Data::Eof => break,
            }
        }
        if let Err(e) = client_sink.eof() {
            log_id!(debug, log_id, "Failed to close client stream: {}", e);
        } else {
            log_id!(debug, log_id, "Reverse proxy client stream closed");
        }
        return Ok(());
    }

    client_sink.write_all(chunk).await?;

    // Only the response direction is left: the body of the request is already
    // on its way (see above).
    let copy_response = async {
        while let pipe::Data::Chunk(chunk) = server_source.read().await? {
            server_source.consume(chunk.len())?;
            client_sink.write_all(chunk).await?;
        }
        client_sink.eof()?;
        client_sink.flush().await?;
        Ok::<(), io::Error>(())
    };

    match tokio::time::timeout(context.settings.tcp_connections_timeout, copy_response).await {
        Ok(Ok(())) => Ok(()),
        // HTTP/2 (and sometimes HTTP/3) can surface graceful stream closure
        // as UnexpectedEof once the response has already been delivered.
        Ok(Err(e)) if e.kind() == ErrorKind::UnexpectedEof => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_elapsed) => Err(io::Error::from(ErrorKind::TimedOut)),
    }
}

/// The task that forwards the body of the request to the upstream.
///
/// It is aborted when the request is over: the upstream has answered, the rest
/// of the body is not needed anymore.
struct BodyTask(Option<tokio::task::JoinHandle<()>>);

impl BodyTask {
    fn spawn(
        request: Box<dyn http_codec::PendingRequest>,
        body_length: http_forwarded_stream::BodyLength,
        sink: Box<dyn pipe::Sink>,
        error_tx: mpsc::UnboundedSender<io::Error>,
        done: Arc<AtomicBool>,
        log_id: log_utils::IdChain<u64>,
    ) -> Self {
        let task = tokio::spawn(async move {
            let mut source = http_forwarded_stream::body_source(request.finalize(), body_length);
            let mut sink = sink;
            let mut complete = true;
            loop {
                let chunk = match source.read().await {
                    Ok(pipe::Data::Chunk(chunk)) => chunk,
                    Ok(pipe::Data::Eof) => break,
                    Err(e) => {
                        log_id!(debug, log_id, "Failed to read the request body: {}", e);
                        // The body is broken: the client is to get a 400.
                        let _ = error_tx.send(e);
                        complete = false;
                        break;
                    }
                };
                if let Err(e) = source.consume(chunk.len()) {
                    log_id!(debug, log_id, "Failed to consume the request body: {}", e);
                    complete = false;
                    break;
                }
                if let Err(e) = sink.write_all(chunk).await {
                    log_id!(debug, log_id, "Failed to send the request body: {}", e);
                    complete = false;
                    break;
                }
            }
            if complete {
                done.store(true, Ordering::Release);
            }
            // The socket to the upstream is not half-closed: the upstream
            // knows the end of the body by the length or by the last chunk,
            // and a half-closed connection makes it drop the response.
            //
            // The source of the body is kept here until the task is aborted:
            // for HTTP/1 it is the connection of the client itself, and
            // dropping it earlier would end the session before the response
            // is delivered.
            std::future::pending::<()>().await;
        });

        Self(Some(task))
    }
}

impl Drop for BodyTask {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

fn send_bad_gateway(
    respond: Box<dyn http_codec::PendingRespond>,
    version: http::Version,
) -> io::Result<()> {
    let response = http::Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .version(version)
        .body(())
        .map_err(|e| io::Error::other(format!("bad gateway: {}", e)))?;
    let (parts, _) = response.into_parts();
    respond.send_response(parts, true).map(|_| ())
}

fn send_bad_request(
    respond: Box<dyn http_codec::PendingRespond>,
    version: http::Version,
) -> io::Result<()> {
    let response = http::Response::builder()
        .status(http::StatusCode::BAD_REQUEST)
        .version(version)
        .body(())
        .map_err(|e| io::Error::other(format!("bad request: {}", e)))?;
    let (parts, _) = response.into_parts();
    respond.send_response(parts, true).map(|_| ())
}
