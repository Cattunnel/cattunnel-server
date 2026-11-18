use crate::http_codec::{RequestHeaders, ResponseHeaders};
use crate::{http_codec, log_id, log_utils, pipe};
use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use std::collections::HashSet;
use std::io;
use std::io::ErrorKind;

const INITIAL_RESPONSE_HEADERS_BUFFER_SIZE: usize = 32;
const MAX_RESPONSE_HEADERS_NUM: usize = 128;
const ENCODED_CHUNK_SUFFIX: &str = "\r\n";

/// The maximum length of the line with the size of a chunk. The line is a
/// number in hex with an optional extension, so a longer one means a broken
/// body: the buffer of the body would grow until the memory is over.
pub(crate) const MAX_CHUNK_SIZE_LINE_LEN: usize = 64;
/// The maximum size of the trailers of a chunked body. They are the last
/// part of the body, so a bigger section means a broken body as well.
pub(crate) const MAX_CHUNK_TRAILERS_LEN: usize = 8 * 1024;

/// Wrap the `stream` with a non-CONNECT request into a wrapper which forwards the request
/// to a peer as a plain HTTP/1 one
pub(crate) fn into_forwarded(
    stream: Box<dyn http_codec::Stream>,
) -> io::Result<(Box<dyn pipe::Source>, Box<dyn pipe::Sink>)> {
    let (request, respond) = stream.split();
    let method = request.request().method.clone();
    let version = request.request().version;

    let (serialized_request, body_length) = match serialize_request(request.request()) {
        Ok(x) => x,
        Err(e) => {
            let _ = respond.send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
            return Err(e);
        }
    };

    let id = request.id();

    Ok((
        Box::new(ForwardedStreamSource {
            state: SourceState::WantSendRequest(SourceWantSendRequest {
                request,
                serialized_request,
                body_length,
            }),
            skip_consume_bytes: 0,
            id: id.clone(),
        }),
        Box::new(ForwardedStreamSink {
            state: SinkState::WaitingResponse(SinkWaitingResponse {
                headers_buffer: BytesMut::new(),
                request_method: method,
                request_version: version,
                respond,
            }),
            fake_unsent: false,
            id,
        }),
    ))
}

struct ForwardedStreamSource {
    state: SourceState,
    /// Needed to not over-consume bytes sent as part of HTTP/1-specifics (like request headers and
    /// chunk markers)
    skip_consume_bytes: usize,
    id: log_utils::IdChain<u64>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum BodyLength {
    Determined(u64),
    Chunked,
    /// A body of unknown length: a stream of frames (HTTP/2, HTTP/3), it ends
    /// with the stream.
    Unknown,
}

/// The length of the request body, as the headers of the request say.
///
/// None means the headers do not say: the caller decides what an unknown
/// length means for the protocol it speaks.
pub(crate) fn body_length(request: &RequestHeaders) -> io::Result<Option<BodyLength>> {
    let mut body_length: Option<BodyLength> = None;
    for (name, value) in &request.headers {
        match (name.as_str(), &body_length) {
            ("content-length", None) => {
                body_length = Some(BodyLength::Determined(
                    value
                        .to_str()
                        .map_err(|_| io::Error::other("Invalid Content-Length header value"))?
                        .parse::<u64>()
                        .map_err(|_| io::Error::other("Invalid Content-Length header value"))?,
                ));
            }
            ("content-length", Some(BodyLength::Determined(_))) => {
                return Err(io::Error::other(
                    "Request has multiple Content-Length headers",
                ));
            }
            ("transfer-encoding", _) if value == "chunked" => {
                body_length = Some(BodyLength::Chunked)
            }
            _ => (),
        }
    }

    if request.method == http::Method::HEAD {
        return Ok(Some(BodyLength::Determined(0)));
    }

    Ok(body_length)
}

/// A source that yields the body of a request and nothing else.
///
/// The bytes are passed through as they are, so the peer sees the same framing
/// the client used: a body of a known length is cut after that many bytes, a
/// chunked one after its last chunk, a body of unknown length until the stream
/// ends.
pub(crate) struct RequestBodySource {
    source: Box<dyn pipe::Source>,
    state: BodyState,
    /// Bytes read from the source. The first `confirmed` of them are known to
    /// belong to the body and are about to be given out.
    buffer: BytesMut,
    confirmed: usize,
    id: log_utils::IdChain<u64>,
}

enum BodyState {
    /// Send the given number of bytes and stop.
    Remaining(u64),
    /// Chunked: reading the size of the next chunk.
    ChunkSize,
    /// Chunked: sending the data of the current chunk.
    ChunkData(u64),
    /// Chunked: reading the CRLF after the data of the chunk.
    ChunkCrlf,
    /// Chunked: reading the trailers after the last chunk.
    ChunkTrailers,
    /// Send everything until the source ends.
    UntilEof,
    Done,
}

/// A source of the body of a request.
pub(crate) fn body_source(
    source: Box<dyn pipe::Source>,
    body_length: BodyLength,
) -> Box<dyn pipe::Source> {
    let id = source.id();
    let state = match body_length {
        BodyLength::Determined(0) => BodyState::Done,
        BodyLength::Determined(n) => BodyState::Remaining(n),
        BodyLength::Chunked => BodyState::ChunkSize,
        BodyLength::Unknown => BodyState::UntilEof,
    };

    Box::new(RequestBodySource {
        source,
        state,
        buffer: BytesMut::new(),
        confirmed: 0,
        id,
    })
}

impl RequestBodySource {
    /// Read more bytes into the buffer. False means the source has ended.
    async fn fill_buffer(&mut self) -> io::Result<bool> {
        match self.source.read().await? {
            pipe::Data::Chunk(chunk) => {
                self.buffer.put(chunk);
                Ok(true)
            }
            pipe::Data::Eof => Ok(false),
        }
    }
}

#[async_trait]
impl pipe::Source for RequestBodySource {
    fn id(&self) -> log_utils::IdChain<u64> {
        self.id.clone()
    }

    async fn read(&mut self) -> io::Result<pipe::Data> {
        loop {
            if self.confirmed > 0 {
                let data = self.buffer.split_to(self.confirmed);
                self.confirmed = 0;
                return Ok(pipe::Data::Chunk(data.freeze()));
            }

            match self.state {
                BodyState::Done => return Ok(pipe::Data::Eof),
                BodyState::Remaining(left) => {
                    if self.buffer.is_empty() && !self.fill_buffer().await? {
                        self.state = BodyState::Done;
                        continue;
                    }
                    let to_send = std::cmp::min(self.buffer.len() as u64, left) as usize;
                    self.confirmed = to_send;
                    self.state = if to_send as u64 == left {
                        BodyState::Done
                    } else {
                        BodyState::Remaining(left - to_send as u64)
                    };
                }
                BodyState::UntilEof => {
                    if self.buffer.is_empty() && !self.fill_buffer().await? {
                        self.state = BodyState::Done;
                        continue;
                    }
                    self.confirmed = self.buffer.len();
                }
                BodyState::ChunkSize => {
                    let line_end = match self.buffer.windows(2).position(|w| w == b"\r\n") {
                        Some(pos) => pos + 2,
                        None => {
                            // The line does not end yet: the buffer grows with
                            // every byte of the body, so it has to be limited.
                            if self.buffer.len() > MAX_CHUNK_SIZE_LINE_LEN {
                                return Err(io::Error::new(
                                    ErrorKind::InvalidData,
                                    "The size line of a chunk is too long",
                                ));
                            }
                            if !self.fill_buffer().await? {
                                self.state = BodyState::Done;
                                continue;
                            }
                            continue;
                        }
                    };
                    let line = std::str::from_utf8(&self.buffer[..line_end - 2])
                        .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                    let size_hex = line.split(';').next().unwrap_or_default().trim();
                    let size = u64::from_str_radix(size_hex, 16)
                        .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                    // The size of the chunk belongs to the body as well.
                    self.confirmed = line_end;
                    self.state = if size == 0 {
                        BodyState::ChunkTrailers
                    } else {
                        BodyState::ChunkData(size)
                    };
                }
                BodyState::ChunkData(left) => {
                    if self.buffer.is_empty() && !self.fill_buffer().await? {
                        self.state = BodyState::Done;
                        continue;
                    }
                    let to_send = std::cmp::min(self.buffer.len() as u64, left) as usize;
                    self.confirmed = to_send;
                    self.state = if to_send as u64 == left {
                        BodyState::ChunkCrlf
                    } else {
                        BodyState::ChunkData(left - to_send as u64)
                    };
                }
                BodyState::ChunkCrlf => {
                    while self.buffer.len() < 2 {
                        if !self.fill_buffer().await? {
                            self.state = BodyState::Done;
                            continue;
                        }
                    }
                    if !self.buffer.starts_with(b"\r\n") {
                        return Err(io::Error::new(
                            ErrorKind::InvalidData,
                            "Malformed chunked body",
                        ));
                    }
                    self.confirmed = 2;
                    self.state = BodyState::ChunkSize;
                }
                BodyState::ChunkTrailers => {
                    // The trailers and the final CRLF belong to the body. The
                    // section is either empty (the CRLF right after the last
                    // chunk) or it ends with an empty line of its own.
                    loop {
                        if self.buffer.starts_with(b"\r\n") {
                            self.confirmed = 2;
                            self.state = BodyState::Done;
                            break;
                        }
                        if let Some(pos) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            self.confirmed = pos + 4;
                            self.state = BodyState::Done;
                            break;
                        }
                        // The end of the trailers is not here yet: the buffer
                        // grows with every byte, so it has to be limited.
                        if self.buffer.len() > MAX_CHUNK_TRAILERS_LEN {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                "The trailers of the body are too long",
                            ));
                        }
                        if !self.fill_buffer().await? {
                            self.confirmed = self.buffer.len();
                            self.state = BodyState::Done;
                            break;
                        }
                    }
                }
            }
        }
    }

    fn consume(&mut self, size: usize) -> io::Result<()> {
        self.source.consume(size)
    }
}

struct SourceWantSendRequest {
    request: Box<dyn http_codec::PendingRequest>,
    serialized_request: Bytes,
    body_length: BodyLength,
}

struct SourceTransferringBody {
    source: Box<dyn pipe::Source>,
    body_length: BodyLength,
    sent_bytes: u64,
}

enum SourceState {
    WantSendRequest(SourceWantSendRequest),
    TransferringBody(SourceTransferringBody),
    Done,
}

struct ForwardedStreamSink {
    state: SinkState,
    fake_unsent: bool,
    id: log_utils::IdChain<u64>,
}

struct SinkWaitingResponse {
    headers_buffer: BytesMut,
    request_method: http::Method,
    request_version: http::Version,
    respond: Box<dyn http_codec::PendingRespond>,
}

struct SinkTransferringBodyNonEncoded {
    sink: Box<dyn pipe::Sink>,
    body_length: Option<u64>,
    sent_bytes: u64,
}

struct SinkWaitingChunkPrefix {
    buffer: BytesMut,
    sink: Box<dyn pipe::Sink>,
}

struct SinkTransferringBodyChunked {
    sink: Box<dyn pipe::Sink>,
    remaining_chunk_size: Option<u64>,
}

struct SinkWaitingChunkSuffix {
    buffer: BytesMut,
    terminating_chunk: bool,
    sink: Box<dyn pipe::Sink>,
}

enum SinkState {
    Idle,
    WaitingResponse(SinkWaitingResponse),
    TransferringBodyNonEncoded(SinkTransferringBodyNonEncoded),
    WaitingChunkPrefix(SinkWaitingChunkPrefix),
    TransferringBodyChunked(SinkTransferringBodyChunked),
    WaitingChunkSuffix(SinkWaitingChunkSuffix),
}

#[async_trait]
impl pipe::Source for ForwardedStreamSource {
    fn id(&self) -> log_utils::IdChain<u64> {
        self.id.clone()
    }

    async fn read(&mut self) -> io::Result<pipe::Data> {
        match &self.state {
            SourceState::WantSendRequest(_) => return self.on_read_request(),
            // workaround to please the compiler, as it does not allow to `await` since
            // the reference to the state is dropped later
            // @todo: make it better
            SourceState::TransferringBody(_) => (),
            SourceState::Done => return Ok(pipe::Data::Eof),
        }

        assert!(matches!(self.state, SourceState::TransferringBody(_)));
        self.read_body().await
    }

    fn consume(&mut self, mut size: usize) -> io::Result<()> {
        if self.skip_consume_bytes > 0 {
            let to_sub = std::cmp::min(self.skip_consume_bytes, size);
            self.skip_consume_bytes -= to_sub;
            size -= to_sub;
            if size == 0 {
                return Ok(());
            }
        }

        match &mut self.state {
            SourceState::TransferringBody(x) => x.source.consume(size),
            _ => Ok(()),
        }
    }
}

impl ForwardedStreamSource {
    fn on_read_request(&mut self) -> io::Result<pipe::Data> {
        let state = match std::mem::replace(&mut self.state, SourceState::Done) {
            SourceState::WantSendRequest(x) => x,
            _ => unreachable!(),
        };

        self.skip_consume_bytes = state.serialized_request.len();
        self.state = match state.body_length {
            BodyLength::Determined(0) => SourceState::Done,
            body_length => SourceState::TransferringBody(SourceTransferringBody {
                source: state.request.finalize(),
                body_length,
                sent_bytes: 0,
            }),
        };

        Ok(pipe::Data::Chunk(state.serialized_request))
    }

    async fn read_body(&mut self) -> io::Result<pipe::Data> {
        let mut state = match std::mem::replace(&mut self.state, SourceState::Done) {
            SourceState::TransferringBody(x) => x,
            _ => unreachable!(),
        };

        let result = state.source.read().await?;

        self.state = SourceState::TransferringBody(state);
        let state = match &mut self.state {
            SourceState::TransferringBody(x) => x,
            _ => unreachable!(),
        };

        match (result, &state.body_length) {
            (pipe::Data::Chunk(mut bytes), BodyLength::Determined(n)) if state.sent_bytes < *n => {
                let to_send = std::cmp::min(bytes.len() as u64, n - state.sent_bytes);
                state.sent_bytes += to_send;
                Ok(pipe::Data::Chunk(bytes.split_to(to_send as usize)))
            }
            (pipe::Data::Chunk(_) | pipe::Data::Eof, BodyLength::Determined(_)) => {
                self.state = SourceState::Done;
                Ok(pipe::Data::Eof)
            }
            (pipe::Data::Chunk(bytes), BodyLength::Chunked | BodyLength::Unknown) => {
                state.sent_bytes += bytes.len() as u64;
                Ok(pipe::Data::Chunk(bytes))
            }
            (pipe::Data::Eof, BodyLength::Chunked | BodyLength::Unknown) => {
                self.state = SourceState::Done;
                Ok(pipe::Data::Eof)
            }
        }
    }
}

#[async_trait]
impl pipe::Sink for ForwardedStreamSink {
    fn id(&self) -> log_utils::IdChain<u64> {
        self.id.clone()
    }

    fn write(&mut self, data: Bytes) -> io::Result<Bytes> {
        match &self.state {
            SinkState::Idle => Err(io::Error::other("Invalid state")),
            SinkState::WaitingResponse(_) => match self.on_response_headers_chunk(data) {
                Err(e) if matches!(self.state, SinkState::WaitingResponse(_)) => {
                    match std::mem::replace(&mut self.state, SinkState::Idle) {
                        SinkState::WaitingResponse(x) => {
                            let _ = x
                                .respond
                                .send_bad_response(http::StatusCode::BAD_GATEWAY, vec![]);
                        }
                        _ => unreachable!(),
                    }
                    Err(e)
                }
                x => x,
            },
            SinkState::TransferringBodyNonEncoded(_) => self.on_non_encoded_chunk(data),
            SinkState::WaitingChunkPrefix(_) => self.on_encoded_chunk_prefix(data),
            SinkState::TransferringBodyChunked(_) => self.on_encoded_chunk(data),
            SinkState::WaitingChunkSuffix(_) => self.on_encoded_chunk_suffix(data),
        }
    }

    fn eof(&mut self) -> io::Result<()> {
        match std::mem::replace(&mut self.state, SinkState::Idle) {
            SinkState::Idle => Ok(()),
            SinkState::WaitingResponse(x) => x
                .respond
                .send_bad_response(http::StatusCode::BAD_GATEWAY, vec![]),
            SinkState::TransferringBodyNonEncoded(mut x) => x.sink.eof(),
            SinkState::WaitingChunkPrefix(mut x) => x.sink.eof(),
            SinkState::TransferringBodyChunked(mut x) => x.sink.eof(),
            SinkState::WaitingChunkSuffix(mut x) => x.sink.eof(),
        }
    }

    async fn wait_writable(&mut self) -> io::Result<()> {
        if self.fake_unsent {
            self.fake_unsent = false;
            return Ok(());
        }

        match &mut self.state {
            SinkState::Idle | SinkState::WaitingResponse(_) => {
                Err(io::Error::other("Invalid state"))
            }
            SinkState::WaitingChunkPrefix(_) | SinkState::WaitingChunkSuffix(_) => Ok(()),
            SinkState::TransferringBodyNonEncoded(x) => x.sink.wait_writable().await,
            SinkState::TransferringBodyChunked(x) => x.sink.wait_writable().await,
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        let sink = match self.state {
            SinkState::Idle | SinkState::WaitingResponse(_) => return Ok(()),
            SinkState::TransferringBodyNonEncoded(ref mut x) => &mut x.sink,
            SinkState::WaitingChunkPrefix(ref mut x) => &mut x.sink,
            SinkState::TransferringBodyChunked(ref mut x) => &mut x.sink,
            SinkState::WaitingChunkSuffix(ref mut x) => &mut x.sink,
        };

        sink.flush().await
    }
}

impl ForwardedStreamSink {
    fn on_response_headers_chunk(&mut self, data: Bytes) -> io::Result<Bytes> {
        let state = match &mut self.state {
            SinkState::WaitingResponse(x) => x,
            _ => unreachable!(),
        };

        let ((response, body_length), tail) = match state.parse_response(data, &self.id)? {
            (Some(x), tail) => (x, tail),
            (None, tail) => return Ok(tail),
        };

        if (100..200).contains(&response.status.as_u16()) {
            state.respond.send_intermediate_response(response)?;
            return Ok(tail);
        }

        let state = match std::mem::replace(&mut self.state, SinkState::Idle) {
            SinkState::WaitingResponse(x) => x,
            _ => unreachable!(),
        };

        let sink = state.respond.send_response(
            response,
            matches!(body_length, Some(BodyLength::Determined(0))),
        )?;

        self.state = match body_length {
            Some(BodyLength::Chunked) => SinkState::WaitingChunkPrefix(SinkWaitingChunkPrefix {
                buffer: Default::default(),
                sink: sink.into_pipe_sink(),
            }),
            None | Some(BodyLength::Determined(_)) | Some(BodyLength::Unknown) => {
                SinkState::TransferringBodyNonEncoded(SinkTransferringBodyNonEncoded {
                    sink: sink.into_pipe_sink(),
                    body_length: body_length.map(|x| match x {
                        BodyLength::Determined(x) => x,
                        BodyLength::Chunked | BodyLength::Unknown => unreachable!(),
                    }),
                    sent_bytes: 0,
                })
            }
        };

        self.fake_unsent = !tail.is_empty();
        Ok(tail)
    }

    fn on_non_encoded_chunk(&mut self, mut data: Bytes) -> io::Result<Bytes> {
        log_id!(trace, self.id, "Non encoded chunk: {} bytes", data.len());
        let state = match &mut self.state {
            SinkState::TransferringBodyNonEncoded(x) => x,
            _ => unreachable!(),
        };

        let to_send = match state.body_length {
            None => data.len(),
            Some(n) => {
                if n <= state.sent_bytes {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
                std::cmp::min(data.len(), (n - state.sent_bytes) as usize)
            }
        };

        if to_send == 0 {
            return Ok(data);
        }

        let chunk_size = data.len();
        let unsent = state.sink.write(data.slice(..to_send))?;
        state.sent_bytes += (chunk_size - unsent.len()) as u64;

        if Some(state.sent_bytes) == state.body_length {
            assert!(unsent.is_empty());
            assert_eq!(data.len(), to_send);
            state.sink.eof()?;
        }

        Ok(data.split_off(to_send - unsent.len()))
    }

    fn on_encoded_chunk_prefix(&mut self, data: Bytes) -> io::Result<Bytes> {
        let state = match &mut self.state {
            SinkState::WaitingChunkPrefix(x) => x,
            _ => unreachable!(),
        };

        let mut data = if state.buffer.is_empty() {
            data
        } else {
            state.buffer.extend_from_slice(&data);
            std::mem::take(&mut state.buffer).freeze()
        };

        let (chunk_size, tail) = match httparse::parse_chunk_size(&data) {
            Ok(httparse::Status::Complete((pos, chunk_size))) => {
                log_id!(trace, self.id, "Encoded chunk size: {} bytes", chunk_size);
                (chunk_size, data.split_off(pos))
            }
            Ok(httparse::Status::Partial) => {
                state.buffer = BytesMut::from(data.as_ref());
                return Ok(Bytes::new());
            }
            Err(httparse::InvalidChunkSize) => {
                return Err(io::Error::other("Invalid encoded chunk size"))
            }
        };

        if chunk_size == 0 {
            self.state = SinkState::WaitingChunkSuffix(SinkWaitingChunkSuffix {
                buffer: Default::default(),
                terminating_chunk: true,
                sink: match std::mem::replace(&mut self.state, SinkState::Idle) {
                    SinkState::WaitingChunkPrefix(x) => x.sink,
                    _ => unreachable!(),
                },
            });
        } else {
            self.state = SinkState::TransferringBodyChunked(SinkTransferringBodyChunked {
                sink: match std::mem::replace(&mut self.state, SinkState::Idle) {
                    SinkState::WaitingChunkPrefix(x) => x.sink,
                    _ => unreachable!(),
                },
                remaining_chunk_size: Some(chunk_size),
            });
        }

        self.fake_unsent = !tail.is_empty();
        Ok(tail)
    }

    fn on_encoded_chunk(&mut self, mut data: Bytes) -> io::Result<Bytes> {
        let mut state = match std::mem::replace(&mut self.state, SinkState::Idle) {
            SinkState::TransferringBodyChunked(x) => x,
            _ => unreachable!(),
        };

        let to_send =
            std::cmp::min(data.len() as u64, state.remaining_chunk_size.unwrap()) as usize;
        let unsent = state.sink.write(data.slice(..to_send))?;

        let remaining = state
            .remaining_chunk_size
            .take()
            .unwrap()
            .saturating_sub(to_send as u64);
        log_id!(
            trace,
            self.id,
            "Encoded chunk: {} bytes (remaining {} bytes)",
            to_send,
            remaining
        );
        if remaining > 0 {
            state.remaining_chunk_size = Some(remaining);
        } else {
            self.state = SinkState::WaitingChunkSuffix(SinkWaitingChunkSuffix {
                buffer: BytesMut::with_capacity(ENCODED_CHUNK_SUFFIX.len()),
                terminating_chunk: false,
                sink: state.sink,
            });
        }
        self.fake_unsent = !data.is_empty();

        Ok(data.split_off(to_send - unsent.len()))
    }

    fn on_encoded_chunk_suffix(&mut self, mut data: Bytes) -> io::Result<Bytes> {
        let mut state = match std::mem::replace(&mut self.state, SinkState::Idle) {
            SinkState::WaitingChunkSuffix(x) => x,
            _ => unreachable!(),
        };

        let suffix = if state.buffer.is_empty() {
            data.split_to(std::cmp::min(data.len(), ENCODED_CHUNK_SUFFIX.len()))
        } else {
            let to_read =
                std::cmp::min(data.len(), ENCODED_CHUNK_SUFFIX.len() - state.buffer.len());
            state.buffer.extend_from_slice(&data.split_to(to_read));
            std::mem::take(&mut state.buffer).freeze()
        };

        if !ENCODED_CHUNK_SUFFIX.as_bytes().starts_with(&suffix) {
            return Err(io::Error::other("Invalid encoded chunk suffix"));
        }

        if suffix.len() < ENCODED_CHUNK_SUFFIX.len() {
            state.buffer = BytesMut::from(suffix.as_ref());
            self.state = SinkState::WaitingChunkSuffix(state);
        } else if state.terminating_chunk {
            if !data.is_empty() {
                log_id!(
                    debug,
                    self.id,
                    "Dropping non-processed {} bytes coming after terminating encoded chunk",
                    data.len()
                );
            }
            data = Bytes::new();
            state.sink.eof()?;
        } else {
            self.state = SinkState::WaitingChunkPrefix(SinkWaitingChunkPrefix {
                buffer: Default::default(),
                sink: state.sink,
            });
        }

        Ok(data)
    }
}

impl SinkWaitingResponse {
    #[allow(clippy::type_complexity)]
    fn parse_response(
        &mut self,
        data: Bytes,
        log_id: &log_utils::IdChain<u64>,
    ) -> io::Result<(Option<(ResponseHeaders, Option<BodyLength>)>, Bytes)> {
        let mut data = if self.headers_buffer.is_empty() {
            data
        } else {
            self.headers_buffer.extend_from_slice(&data);
            std::mem::take(&mut self.headers_buffer).freeze()
        };

        let mut parse_headers_buffer =
            vec![httparse::EMPTY_HEADER; INITIAL_RESPONSE_HEADERS_BUFFER_SIZE];
        loop {
            let mut response = httparse::Response::new(parse_headers_buffer.as_mut());
            match response.parse(data.as_ref()) {
                Ok(httparse::Status::Complete(pos)) => {
                    log_id!(trace, log_id, "Received response: {:?}", response);
                    return Ok((Some(self.convert_response(response)?), data.split_off(pos)));
                }
                Ok(httparse::Status::Partial) => {
                    self.headers_buffer = BytesMut::from(data.as_ref());
                    return Ok((None, Bytes::new()));
                }
                Err(httparse::Error::TooManyHeaders)
                    if parse_headers_buffer.len() < MAX_RESPONSE_HEADERS_NUM =>
                {
                    parse_headers_buffer
                        .resize(2 * parse_headers_buffer.len(), httparse::EMPTY_HEADER)
                }
                Err(e) => return Err(io::Error::other(format!("Failed to parse response: {}", e))),
            }
        }
    }

    fn convert_response(
        &self,
        response: httparse::Response,
    ) -> io::Result<(ResponseHeaders, Option<BodyLength>)> {
        let mut builder = http::response::Response::builder()
            .version(self.request_version)
            .status(response.code.unwrap());
        let mut body_length = None;
        let mut drop_headers =
            HashSet::from(["proxy-connection", "keep-alive", "upgrade"].map(|h| h.to_string()));
        for h in response.headers {
            match (h.name.to_ascii_lowercase().as_str(), self.request_version) {
                (x, _) if drop_headers.contains(x) => (),
                ("connection", _) => {
                    if let Ok(x) = std::str::from_utf8(h.value) {
                        drop_headers.extend(
                            x.split(',')
                                .filter(|x| *x != "close")
                                .map(|x| x.trim().to_lowercase()),
                        );
                    }
                }
                ("transfer-encoding", http::Version::HTTP_2 | http::Version::HTTP_3) => {
                    if h.value == "chunked".as_bytes() {
                        body_length = Some(BodyLength::Chunked);
                    }
                    drop_headers.insert("content-length".to_string());
                    drop_headers.insert("transfer-encoding".to_string());
                }
                (x, _) => {
                    if body_length.is_none() && x == "content-length" {
                        body_length = Some(BodyLength::Determined(
                            std::str::from_utf8(h.value)
                                .map_err(|e| {
                                    io::Error::other(format!(
                                        "Invalid Content-Length header value: {:?}, error={}",
                                        h.value, e
                                    ))
                                })?
                                .parse::<u64>()
                                .map_err(|e| {
                                    io::Error::other(format!(
                                        "Invalid Content-Length header value: {:?}, error={}",
                                        h.value, e
                                    ))
                                })?,
                        ));
                    }
                    builder = builder.header(h.name.trim_end(), h.value)
                }
            }
        }

        if self.request_method == http::Method::HEAD
            || response
                .code
                .is_some_and(|x| (100..200).contains(&x) || x == 204 || x == 304)
        {
            body_length = Some(BodyLength::Determined(0));
        }

        let response = builder
            .body(())
            .map_err(|e| io::Error::other(format!("Invalid response: {}", e)))?
            .into_parts()
            .0;

        Ok((response, body_length))
    }
}

fn version_major_digit(v: http::Version) -> u32 {
    match v {
        http::Version::HTTP_09 => 0,
        _ => 1,
    }
}

fn version_minor_digit(v: http::Version) -> u32 {
    match v {
        http::Version::HTTP_09 => 0,
        http::Version::HTTP_10 => 0,
        http::Version::HTTP_11 => 1,
        http::Version::HTTP_2 => 1,
        http::Version::HTTP_3 => 1,
        _ => unreachable!(),
    }
}

fn serialize_request(request: &RequestHeaders) -> io::Result<(Bytes, BodyLength)> {
    let mut serialized = BytesMut::new();
    serialized.put(
        format!(
            "{} {} HTTP/{}.{}\r\n",
            request.method.as_str(),
            if request.method != http::Method::OPTIONS {
                request
                    .uri
                    .path_and_query()
                    .map_or(request.uri.path(), |x| x.as_str())
            } else {
                "*"
            },
            version_major_digit(request.version),
            version_minor_digit(request.version),
        )
        .as_bytes(),
    );

    let target_host = request.uri.authority().ok_or_else(|| {
        io::Error::other(format!("Request URI lacks host name: {:?}", request.uri))
    })?;

    let mut host_inserted = false;
    for (name, value) in &request.headers {
        match name.as_str() {
            "proxy-authorization" => (),
            "proxy-connection" => (),
            "host" => {
                if !host_inserted {
                    host_inserted = true;
                    serialized.put("host: ".as_bytes());
                    serialized.put(target_host.as_str().as_bytes());
                    serialized.put("\r\n".as_bytes());
                } else {
                    return Err(io::Error::other("Request has multiple Host headers"));
                }
            }
            name => {
                serialized.put(format!("{}: ", name).as_bytes());
                serialized.put(value.as_bytes());
                serialized.put("\r\n".as_bytes());
            }
        }
    }

    if !host_inserted {
        serialized.put("host: ".as_bytes());
        serialized.put(target_host.as_str().as_bytes());
        serialized.put("\r\n".as_bytes());
    }

    serialized.put("\r\n".as_bytes());

    let body_length = body_length(request)?.unwrap_or(match request.version {
        http::Version::HTTP_2 | http::Version::HTTP_3 => BodyLength::Chunked,
        _ => BodyLength::Determined(0),
    });

    Ok((serialized.freeze(), body_length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// A source that gives the given pieces one by one and then ends.
    struct PiecesSource {
        pieces: std::collections::VecDeque<Bytes>,
        id: log_utils::IdChain<u64>,
    }

    impl PiecesSource {
        fn new(pieces: Vec<&[u8]>) -> Box<dyn pipe::Source> {
            Box::new(Self {
                pieces: pieces.iter().map(|x| Bytes::copy_from_slice(x)).collect(),
                id: log_utils::IdChain::empty(),
            })
        }
    }

    #[async_trait]
    impl pipe::Source for PiecesSource {
        fn id(&self) -> log_utils::IdChain<u64> {
            self.id.clone()
        }

        async fn read(&mut self) -> io::Result<pipe::Data> {
            match self.pieces.pop_front() {
                Some(chunk) => Ok(pipe::Data::Chunk(chunk)),
                None => Ok(pipe::Data::Eof),
            }
        }

        fn consume(&mut self, _size: usize) -> io::Result<()> {
            Ok(())
        }
    }

    async fn read_all(source: &mut Box<dyn pipe::Source>) -> io::Result<Bytes> {
        let mut out = BytesMut::new();
        loop {
            match source.read().await? {
                pipe::Data::Chunk(chunk) => out.put(chunk),
                pipe::Data::Eof => break,
            }
        }
        Ok(out.freeze())
    }

    #[tokio::test]
    async fn body_of_a_known_length() {
        let mut source = body_source(
            PiecesSource::new(vec![b"hello ", b"world and more"]),
            BodyLength::Determined(5),
        );

        assert_eq!(
            read_all(&mut source).await.unwrap(),
            Bytes::from_static(b"hello")
        );
    }

    #[tokio::test]
    async fn body_of_a_known_length_over_several_pieces() {
        let mut source = body_source(
            PiecesSource::new(vec![b"he", b"ll", b"o"]),
            BodyLength::Determined(5),
        );

        assert_eq!(
            read_all(&mut source).await.unwrap(),
            Bytes::from_static(b"hello")
        );
    }

    #[tokio::test]
    async fn body_of_an_unknown_length() {
        let mut source = body_source(
            PiecesSource::new(vec![b"one ", b"two"]),
            BodyLength::Unknown,
        );

        assert_eq!(
            read_all(&mut source).await.unwrap(),
            Bytes::from_static(b"one two")
        );
    }

    #[tokio::test]
    async fn no_body_at_all() {
        let mut source = body_source(
            PiecesSource::new(vec![b"should not be read"]),
            BodyLength::Determined(0),
        );

        assert!(read_all(&mut source).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn chunked_body() {
        let mut source = body_source(
            PiecesSource::new(vec![b"7\r\n1234567\r\n0\r\n\r\n"]),
            BodyLength::Chunked,
        );

        assert_eq!(
            read_all(&mut source).await.unwrap(),
            Bytes::from_static(b"7\r\n1234567\r\n0\r\n\r\n")
        );
    }

    #[tokio::test]
    async fn chunked_body_over_several_pieces() {
        let mut source = body_source(
            PiecesSource::new(vec![b"7\r\n", b"12345", b"67", b"\r\n0\r\n", b"\r\n"]),
            BodyLength::Chunked,
        );

        assert_eq!(
            read_all(&mut source).await.unwrap(),
            Bytes::from_static(b"7\r\n1234567\r\n0\r\n\r\n")
        );
    }

    /// A source that never ends: a client that waits for the answer.
    struct OpenSource {
        pieces: std::collections::VecDeque<Bytes>,
        id: log_utils::IdChain<u64>,
    }

    #[async_trait]
    impl pipe::Source for OpenSource {
        fn id(&self) -> log_utils::IdChain<u64> {
            self.id.clone()
        }

        async fn read(&mut self) -> io::Result<pipe::Data> {
            match self.pieces.pop_front() {
                Some(chunk) => Ok(pipe::Data::Chunk(chunk)),
                None => std::future::pending().await,
            }
        }

        fn consume(&mut self, _size: usize) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn chunked_body_of_a_client_that_waits() {
        let mut source = body_source(
            Box::new(OpenSource {
                pieces: std::collections::VecDeque::from(vec![Bytes::from_static(
                    b"7\r\n1234567\r\n0\r\n\r\n",
                )]),
                id: log_utils::IdChain::empty(),
            }),
            BodyLength::Chunked,
        );

        assert_eq!(
            read_all(&mut source).await.unwrap(),
            Bytes::from_static(b"7\r\n1234567\r\n0\r\n\r\n")
        );
    }

    #[tokio::test]
    async fn chunked_body_of_several_chunks() {
        let mut source = body_source(
            PiecesSource::new(vec![b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"]),
            BodyLength::Chunked,
        );

        assert_eq!(
            read_all(&mut source).await.unwrap(),
            Bytes::from_static(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n")
        );
    }
}
