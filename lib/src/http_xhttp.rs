//! The XHTTP transport: a pair of HTTP streams per tunneled TCP connection.
//!
//! A session consists of two requests to the same path:
//!
//! * `GET <prefix>/<sid>` - the downlink. The response body carries the frames
//!   sent by the endpoint to the client.
//! * `POST <prefix>/<sid>` - the uplink. The request body carries the frames
//!   sent by the client to the endpoint.
//!
//! Both bodies consist of the X3 frames:
//!
//! ```text
//! [type: 1 byte][offset: 8 bytes, big endian][length: 2 or 4 bytes][payload]
//! ```
//!
//! The length field is 2 bytes in the usual case. If its most significant bit
//! is set, the length is 4 bytes: the remaining 15 bits of the short field hold
//! the most significant bits of the length, and the 4 bytes that follow hold
//! the least significant ones. This way the common frame costs 11 bytes of the
//! header, and the maximum payload is 2 GiB minus one byte.
//!
//! The offset is the number of the payload bytes sent in the direction of the
//! frame before this one.  It makes the duplicates detectable after a
//! reconnection: the peer repeats the data it has not confirmed, and the
//! receiver drops the part it already has.
//!
//! The types of the frames:
//!
//! * `OPEN` - the destination address, the first frame of the uplink.
//! * `DATA` - the payload.
//! * `ACK` - the confirmation of the bytes received in the opposite direction.
//!   The offset of this frame holds the number of the confirmed bytes.
//! * `FIN` - the half close.
//! * `RST` - the error.
//! * `PAD` - the padding.
//! * `RESUME` - the first frame of the uplink of a resumed session.  The
//!   payload holds the number of the confirmed uplink bytes and the number of
//!   the received downlink bytes.
//! * `PING` - the liveness check, the first frame of the uplink.
//! * `PONG` - the answer of the liveness check.
//! * `HELLO` - the key exchange, the first frame of the uplink.
//! * `HELLO_ACK` - the answer of the key exchange.
//!
//! A session outlives its HTTP streams: when the connection dies, the session
//! is kept for a while, and the client may continue it with a new pair of
//! streams and the same session identifier.

use crate::authentication::{Authenticator, Source, Status};
use crate::http_codec::{self, PendingRespond, ResponseHeaders};
use crate::net_utils::TcpDestination;
use crate::xhttp_crypto::{Opener, RecordDecoder, RecordTake, Sealer};
use crate::{core, forwarder, log_id, log_utils, pipe, tunnel};
use bytes::{BufMut, Bytes, BytesMut};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// The frame type: the destination address, the first frame of the uplink.
pub(crate) const FRAME_OPEN: u8 = 0x01;
/// The frame type: the payload data.
pub(crate) const FRAME_DATA: u8 = 0x02;
/// The frame type: the acknowledgement of the received bytes.
pub(crate) const FRAME_ACK: u8 = 0x03;
/// The frame type: the half-close.
pub(crate) const FRAME_FIN: u8 = 0x04;
/// The frame type: the connection reset.
pub(crate) const FRAME_RST: u8 = 0x05;
/// The frame type: the padding.
pub(crate) const FRAME_PAD: u8 = 0x06;
/// The frame type: the resume of a session.
pub(crate) const FRAME_RESUME: u8 = 0x07;
/// The frame type: the liveness check.
pub(crate) const FRAME_PING: u8 = 0x08;
/// The frame type: the answer of the liveness check.
pub(crate) const FRAME_PONG: u8 = 0x09;
/// The frame type: the key exchange.
pub(crate) const FRAME_HELLO: u8 = 0x0a;
/// The frame type: the answer of the key exchange.
pub(crate) const FRAME_HELLO_ACK: u8 = 0x0b;
pub(crate) const FRAME_RESUME_TOKEN: u8 = 0x0c;
// 0x10..=0x1f: protocol 2; 0x20..=0x2f: protocol 3 (Sudoku).

/// The length of the frame header with a 2-byte length field.
pub(crate) const FRAME_HEADER_LEN: usize = 11;
/// The length of the frame header with a 4-byte length field.
pub(crate) const FRAME_HEADER_LEN_LONG: usize = 15;
/// The maximum payload of a single frame.
pub(crate) const FRAME_MAX_PAYLOAD: usize = 0x7fff_ffff;

/// The maximum time a stream waits for its pair.
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);
/// The maximum number of the sessions waiting for their pair.
const MAX_PENDING_SESSIONS: usize = 1024;
/// The maximum length of the session identifier.
const MAX_SID_LEN: usize = 64;
/// The maximum size of the frame buffer.
const MAX_BUFFERED_FRAME: usize = 2 * 1024 * 1024;
/// The length of the payload of the `RESUME` frame.
const RESUME_PAYLOAD_LEN: usize = 16;
/// The number of the unconfirmed bytes which triggers a confirmation.
const ACK_THRESHOLD: u64 = 64 * 1024;
/// The maximum size of the data in one frame of the downlink.
const DOWNSINK_CHUNK: usize = 32 * 1024;
const SESSION_BASE_RESERVE: usize = 256 * 1024;
const READER_BASE_ALLOWANCE: usize = 32 * 1024;
const PEER_READ_CHUNK: usize = 64 * 1024;
const REPLAY_BLOCK_OVERHEAD: usize = 512;

struct ReservedBytes {
    bytes: Bytes,
    _reservation: Arc<MemoryReservation>,
    _session: Arc<MemoryReservation>,
}

impl AsRef<[u8]> for ReservedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Default)]
struct ReplayBuffer {
    blocks: VecDeque<Bytes>,
    length: usize,
}

impl ReplayBuffer {
    fn len(&self) -> usize {
        self.length
    }

    fn is_empty(&self) -> bool {
        self.length == 0
    }

    fn clear(&mut self) {
        self.blocks = VecDeque::new();
        self.length = 0;
    }

    fn acknowledge(&mut self, mut bytes: usize) {
        while let Some(block) = self.blocks.pop_front() {
            if bytes < block.len() {
                self.blocks.push_front(block.slice(bytes..));
                self.length -= bytes;
                return;
            }
            bytes -= block.len();
            self.length -= block.len();
        }
        self.clear();
    }

    fn append(&mut self, bytes: &[u8], memory: &Arc<MemoryReservation>) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let cost = bytes
            .len()
            .checked_add(REPLAY_BLOCK_OVERHEAD)
            .ok_or_else(memory_exhausted)?;
        let reservation = memory.reserve_more(cost).ok_or_else(memory_exhausted)?;
        let bytes = Bytes::from_owner(ReservedBytes {
            bytes: Bytes::copy_from_slice(bytes),
            _reservation: reservation,
            _session: memory.clone(),
        });
        self.length += bytes.len();
        self.blocks.push_back(bytes);
        Ok(())
    }

    fn chunks(&self, size: usize) -> impl Iterator<Item = Vec<u8>> + '_ {
        let mut blocks = self.blocks.iter();
        let mut current: &[u8] = &[];
        let mut remaining = self.length;
        std::iter::from_fn(move || {
            if remaining == 0 {
                return None;
            }
            let mut chunk = Vec::with_capacity(size.min(remaining));
            while chunk.len() < size && remaining > 0 {
                if current.is_empty() {
                    current = blocks.next()?.as_ref();
                }
                let take = current.len().min(size - chunk.len());
                chunk.extend_from_slice(&current[..take]);
                current = &current[take..];
                remaining -= take;
            }
            Some(chunk)
        })
    }

    #[cfg(test)]
    fn to_vec(&self) -> Vec<u8> {
        self.blocks
            .iter()
            .flat_map(|block| block.iter().copied())
            .collect()
    }
}

fn memory_exhausted() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "XHTTP buffer budget exhausted")
}

/// Encode a frame with the given type, offset and payload.
pub(crate) fn encode_frame(kind: u8, offset: u64, payload: &[u8]) -> Bytes {
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

/// The result of a frame decoding attempt.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecodedFrame {
    /// A complete frame with its type, offset and payload.
    Frame {
        kind: u8,
        offset: u64,
        payload: Bytes,
    },
    /// The buffer does not hold a complete frame yet.
    Incomplete,
    /// The buffer holds a malformed frame.
    Invalid(&'static str),
}

/// The incremental decoder of the X3 frames.
#[derive(Default)]
pub(crate) struct FrameDecoder {
    buffer: BytesMut,
    invalid: bool,
}

impl FrameDecoder {
    pub(crate) fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
            invalid: false,
        }
    }

    /// Append the data to the buffer.
    pub(crate) fn push(&mut self, data: &[u8]) {
        if data.len() > MAX_BUFFERED_FRAME.saturating_sub(self.buffer.len()) {
            self.invalid = true;
            return;
        }
        self.buffer.extend_from_slice(data);
    }

    /// Try to decode the next frame.
    pub(crate) fn next_frame(&mut self) -> DecodedFrame {
        if self.invalid {
            // The peer has declared a frame which is too long: drop the connection instead of
            // buffering the data forever.
            return DecodedFrame::Invalid("Frame is too large");
        }

        if self.buffer.len() < FRAME_HEADER_LEN {
            return DecodedFrame::Incomplete;
        }

        let kind = self.buffer[0];
        if !(FRAME_OPEN..=FRAME_RESUME_TOKEN).contains(&kind) {
            self.invalid = true;
            return DecodedFrame::Invalid("Unknown frame type");
        }
        let offset = u64::from_be_bytes(self.buffer[1..9].try_into().unwrap());
        let short_len = u16::from_be_bytes(self.buffer[9..11].try_into().unwrap());

        let (payload_len, header_len) = if short_len & 0x8000 == 0 {
            (short_len as usize, FRAME_HEADER_LEN)
        } else {
            if self.buffer.len() < FRAME_HEADER_LEN_LONG {
                return DecodedFrame::Incomplete;
            }
            let high = (short_len & 0x7fff) as usize;
            let low = u32::from_be_bytes(self.buffer[11..15].try_into().unwrap()) as usize;
            ((high << 16) | low, FRAME_HEADER_LEN_LONG)
        };

        if payload_len > FRAME_MAX_PAYLOAD || payload_len > MAX_BUFFERED_FRAME - header_len {
            return DecodedFrame::Invalid("Frame payload is too large");
        }

        if self.buffer.len() < header_len + payload_len {
            return DecodedFrame::Incomplete;
        }

        let frame = self.buffer.split_to(header_len + payload_len).freeze();
        DecodedFrame::Frame {
            kind,
            offset,
            payload: frame.slice(header_len..),
        }
    }
}

/// The downlink part of a session: the `GET` request and its response.
struct DownHalf {
    respond: Box<dyn PendingRespond>,
    id: log_utils::IdChain<u64>,
    client_address: IpAddr,
    user_agent: Option<String>,
    tls_domain: String,
}

/// The uplink part of a session: the `POST` request and its body.
struct UpHalf {
    body: Box<dyn pipe::Source>,
    respond: Box<dyn PendingRespond>,
}

/// One of the two halves of a session.
enum Half {
    Down(DownHalf),
    Up(UpHalf),
}

/// The socket of the destination, kept by the session.
///
/// The socket belongs to the session, not to the connection: the session
/// outlives the connection, and the connection may die without the session
/// noticing it (the connection of the client may stay half-open).
struct PeerSlot {
    /// The write half of the socket, while no connection uses it.
    sink: std::sync::Mutex<Option<Box<dyn pipe::Sink>>>,
    /// The read half of the socket, while no connection uses it.
    source: std::sync::Mutex<Option<Box<dyn pipe::Source>>>,
    /// The halves are back in the slot.
    ready: tokio::sync::Notify,
}

/// The write half of the socket, taken by the pumps of a connection.
///
/// It goes back to the session when the pumps end, even when they are
/// cancelled: the next connection of the session may continue the transfer.
struct SinkGuard {
    slot: Arc<PeerSlot>,
    sink: Option<Box<dyn pipe::Sink>>,
}

impl SinkGuard {
    fn new(slot: Arc<PeerSlot>, sink: Box<dyn pipe::Sink>) -> Self {
        Self {
            slot,
            sink: Some(sink),
        }
    }

    fn get(&mut self) -> &mut Box<dyn pipe::Sink> {
        self.sink.as_mut().expect("the sink is there")
    }
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            {
                let mut slot = self.slot.sink.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(sink);
                }
            }
            self.slot.ready.notify_waiters();
        }
    }
}

/// The read half of the socket, taken by the pumps of a connection.
struct SourceGuard {
    slot: Arc<PeerSlot>,
    source: Option<Box<dyn pipe::Source>>,
}

impl SourceGuard {
    fn new(slot: Arc<PeerSlot>, source: Box<dyn pipe::Source>) -> Self {
        Self {
            slot,
            source: Some(source),
        }
    }

    fn get(&mut self) -> &mut Box<dyn pipe::Source> {
        self.source.as_mut().expect("the source is there")
    }
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            {
                let mut slot = self.slot.source.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(source);
                }
            }
            self.slot.ready.notify_waiters();
        }
    }
}

/// The signal to stop the pumps of a connection.
///
/// It is used when the session is resumed: the pumps of the old connection may
/// be stuck on the dead socket, and the new connection needs the socket of the
/// destination.
struct CancelSignal {
    /// The signal is raised.
    raised: std::sync::atomic::AtomicBool,
    /// The waiters of the signal.
    notify: tokio::sync::Notify,
}

impl CancelSignal {
    fn new() -> Self {
        Self {
            raised: std::sync::atomic::AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Raise the signal.
    fn raise(&self) {
        self.raised
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    /// Wait for the signal.
    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.raised.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            notified.await;
        }
    }
}

/// The state of the data transfer of a session.
struct SessionInner {
    /// The number of the uplink bytes written to the destination.
    up_written: u64,
    /// The number of the uplink bytes confirmed to the client: the value of the
    /// last confirmation sent in the downlink.
    up_acked_sent: u64,
    /// The number of the downlink bytes sent to the client.
    down_sent: u64,
    /// The downlink data which is not confirmed yet, it starts at
    /// `down_buffer_start`.
    down_buffer: ReplayBuffer,
    down_pending: Option<Bytes>,
    /// The offset of the first byte of `down_buffer`.
    down_buffer_start: u64,
    /// The sink of the body of the downlink.
    down_sink: Option<Box<dyn pipe::Sink>>,
    resume_challenge: Option<[u8; 32]>,
    /// The encryption of the outgoing direction.
    sealer: Option<Sealer>,
    /// The number of the live connections of the session.
    connections: u32,
    terminal: bool,
    down_eof: bool,
    down_fin_acked: bool,
    up_eof: bool,
    /// The signal to stop the pumps of the current connection.
    cancel: Arc<CancelSignal>,
}

/// A session of the tunnel: it outlives the HTTP streams which carry it.
struct Session {
    /// The identifier of the session.
    sid: String,
    /// The identity of the client the session belongs to.
    owner: String,
    /// The identifier of the session, for the logs.
    id: log_utils::IdChain<u64>,
    /// The state of the data transfer.
    inner: tokio::sync::Mutex<SessionInner>,
    /// The number of the downlink bytes confirmed by the client.  It is used to
    /// apply the back pressure: the endpoint stops reading the destination
    /// while the client does not confirm the data.
    down_acked: watch::Sender<u64>,
    /// The maximum number of the unconfirmed bytes.
    unacked_limit: usize,
    /// The downlink half of the session has arrived.
    down_ready: tokio::sync::Notify,
    /// Notified when the uplink writes new data: the downlink pump sends the
    /// confirmation of the uplink to the client.
    up_written_notify: std::sync::Arc<tokio::sync::Notify>,
    /// The socket of the destination.
    peer: Arc<PeerSlot>,
    _memory: Arc<MemoryReservation>,
    resume_lock: tokio::sync::Mutex<()>,
    resume_key: [u8; 32],
}

/// A session waiting for its pair.
struct PendingSession {
    /// The identity of the client the session belongs to.
    owner: String,
    /// The time the first half arrived at.
    created: Instant,
    /// The channel to deliver the second half to the first one.
    tx: mpsc::Sender<Half>,
    paired: bool,
    memory: Arc<MemoryReservation>,
}

/// An entry of the registry of the sessions.
enum Entry {
    /// The session is waiting for its pair.
    Pending(PendingSession),
    /// The session is established, it may be resumed.
    Live(Arc<Session>),
}

/// The registry of the sessions.
pub(crate) struct XhttpRegistry {
    entries: Mutex<HashMap<String, Entry>>,
    reserved: std::sync::atomic::AtomicUsize,
    available: tokio::sync::Notify,
}

struct MemoryReservation {
    registry: Arc<XhttpRegistry>,
    bytes: usize,
    limit: usize,
    metrics: Arc<crate::metrics::Metrics>,
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.registry
            .reserved
            .fetch_sub(self.bytes, std::sync::atomic::Ordering::Relaxed);
        self.metrics.xhttp_reserved_bytes.sub(self.bytes as i64);
        self.registry.available.notify_waiters();
    }
}

impl MemoryReservation {
    fn reserve_more(self: &Arc<Self>, bytes: usize) -> Option<Arc<Self>> {
        self.registry
            .reserve(bytes, self.limit, self.metrics.clone())
    }
}

struct ReaderReservation {
    session: Arc<MemoryReservation>,
    leases: Vec<Arc<MemoryReservation>>,
    high_water: usize,
}

impl ReaderReservation {
    fn ensure(&mut self, bytes: usize) -> io::Result<()> {
        let required = bytes
            .checked_mul(8)
            .ok_or_else(memory_exhausted)?
            .saturating_sub(READER_BASE_ALLOWANCE);
        if required > self.high_water {
            let required = required
                .checked_next_power_of_two()
                .ok_or_else(memory_exhausted)?;
            let lease = self
                .session
                .reserve_more(required - self.high_water)
                .ok_or_else(memory_exhausted)?;
            self.leases.push(lease);
            self.high_water = required;
        }
        Ok(())
    }
}

struct PendingLease {
    registry: Arc<XhttpRegistry>,
    sid: String,
    tx: mpsc::Sender<Half>,
}

impl Drop for PendingLease {
    fn drop(&mut self) {
        let mut entries = self.registry.entries.lock().unwrap();
        if matches!(entries.get(&self.sid), Some(Entry::Pending(p)) if p.tx.same_channel(&self.tx))
        {
            entries.remove(&self.sid);
        }
    }
}

/// The result of the pairing of the two halves of a session.
enum JoinOutcome {
    /// This half arrived first and has received its counterpart.
    Leader {
        down: DownHalf,
        up: UpHalf,
        memory: Arc<MemoryReservation>,
        lease: PendingLease,
    },
    /// This half arrived second and has been passed to the first one.
    Passed,
    /// The session exists: this half is attached to it.
    Live { session: Arc<Session>, half: Half },
    /// The session belongs to another client.
    OwnerMismatch,
    /// The pair has not arrived in time.
    Timeout,
    /// Too many sessions are waiting for their pair.
    TooMany,
}

impl XhttpRegistry {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            reserved: std::sync::atomic::AtomicUsize::new(0),
            available: tokio::sync::Notify::new(),
        }
    }

    fn reserve(
        self: &Arc<Self>,
        bytes: usize,
        limit: usize,
        metrics: Arc<crate::metrics::Metrics>,
    ) -> Option<Arc<MemoryReservation>> {
        let reserved = self.reserved.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= limit && *total <= i64::MAX as usize)
            },
        );
        if reserved.is_err() {
            metrics.xhttp_memory_rejections.inc();
            return None;
        }
        metrics.xhttp_reserved_bytes.add(bytes as i64);
        Some(Arc::new(MemoryReservation {
            registry: self.clone(),
            bytes,
            limit,
            metrics,
        }))
    }

    /// The number of the sessions waiting for their pair.
    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| matches!(e, Entry::Pending(_)))
            .count()
    }

    /// The number of the established sessions.
    #[cfg(test)]
    fn live_len(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| matches!(e, Entry::Live(_)))
            .count()
    }

    /// Register an established session, so it may be resumed.
    fn register_live(&self, session: Arc<Session>) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(session.sid.clone(), Entry::Live(session));
    }

    /// Remove an established session.
    fn unregister_live(&self, sid: &str, session: &Arc<Session>) {
        let mut entries = self.entries.lock().unwrap();
        let remove = match entries.get(sid) {
            Some(Entry::Live(existing)) => Arc::ptr_eq(existing, session),
            _ => false,
        };
        if remove {
            entries.remove(sid);
        }
    }
}

fn reject_half(half: Half, status: http::StatusCode) {
    let respond = match half {
        Half::Down(down) => down.respond,
        Half::Up(up) => up.respond,
    };
    let _ = respond.send_bad_response(status, vec![]);
}

/// Pair the current half of a session with its counterpart.
///
/// The half which arrives first owns the session: it waits for its counterpart
/// and runs the whole session.  The second half is handed over to it.
///
/// If the session already exists, the half is attached to it: this is the
/// resume of a session after the death of the connection.
async fn join_half(
    registry: &Arc<XhttpRegistry>,
    sid: &str,
    owner: &str,
    our_side: Half,
    context: &Arc<core::Context>,
) -> Result<JoinOutcome, JoinOutcome> {
    let (tx, mut rx) = mpsc::channel::<Half>(1);

    enum Role {
        /// The current half arrived first, the peer is awaited.
        First(Arc<MemoryReservation>),
        /// The current half arrived second, the peer is already known.
        Second(PendingSession),
    }

    let role = {
        let mut entries = registry.entries.lock().unwrap();
        entries.retain(|_, entry| match entry {
            Entry::Pending(pending) => pending.paired || pending.created.elapsed() < PAIR_TIMEOUT,
            Entry::Live(_) => true,
        });

        match entries.get(sid) {
            Some(Entry::Live(session)) => {
                if session.owner != owner {
                    return Err(JoinOutcome::OwnerMismatch);
                }
                let session = session.clone();
                drop(entries);
                return Ok(JoinOutcome::Live {
                    session,
                    half: our_side,
                });
            }
            Some(Entry::Pending(pending)) => {
                if pending.paired {
                    return Err(JoinOutcome::TooMany);
                }
                if pending.owner != owner {
                    return Err(JoinOutcome::OwnerMismatch);
                }
                let pending = PendingSession {
                    owner: pending.owner.clone(),
                    created: pending.created,
                    tx: pending.tx.clone(),
                    paired: true,
                    memory: pending.memory.clone(),
                };
                if let Some(Entry::Pending(p)) = entries.get_mut(sid) {
                    p.paired = true;
                }
                Role::Second(pending)
            }
            None => {
                let pending_count = entries
                    .values()
                    .filter(|e| matches!(e, Entry::Pending(_)))
                    .count();
                if pending_count >= MAX_PENDING_SESSIONS {
                    return Err(JoinOutcome::TooMany);
                }
                let Some(memory) = registry.reserve(
                    SESSION_BASE_RESERVE,
                    context.settings.xhttp_memory_limit,
                    context.metrics.clone(),
                ) else {
                    reject_half(our_side, http::StatusCode::SERVICE_UNAVAILABLE);
                    return Err(JoinOutcome::TooMany);
                };
                entries.insert(
                    sid.to_string(),
                    Entry::Pending(PendingSession {
                        owner: owner.to_string(),
                        created: Instant::now(),
                        tx: tx.clone(),
                        paired: false,
                        memory: memory.clone(),
                    }),
                );
                Role::First(memory)
            }
        }
    };

    match role {
        Role::First(memory) => {
            let lease = PendingLease {
                registry: registry.clone(),
                sid: sid.to_string(),
                tx,
            };
            let peer = match tokio::time::timeout(PAIR_TIMEOUT, rx.recv()).await {
                Ok(Some(peer)) => peer,
                _ => return Err(JoinOutcome::Timeout),
            };

            let (down, up) = order_sides(our_side, peer).ok_or(JoinOutcome::OwnerMismatch)?;
            Ok(JoinOutcome::Leader {
                down,
                up,
                memory,
                lease,
            })
        }
        Role::Second(pending) => match pending.tx.send(our_side).await {
            Ok(()) => Ok(JoinOutcome::Passed),
            Err(_) => Err(JoinOutcome::Timeout),
        },
    }
}

/// Put the downlink and the uplink halves in the canonical order.
fn order_sides(a: Half, b: Half) -> Option<(DownHalf, UpHalf)> {
    match (a, b) {
        (Half::Down(down), Half::Up(up)) | (Half::Up(up), Half::Down(down)) => Some((down, up)),
        _ => None,
    }
}

/// The reader of the frames from the body of the uplink request.
struct FrameReader {
    source: Box<dyn pipe::Source>,
    decoder: FrameDecoder,
    /// The decryption of the records, it is set when the body is encrypted.
    opener: Option<Opener>,
    /// The buffer of the encrypted records.
    records: RecordDecoder,
    memory: Option<Arc<Mutex<ReaderReservation>>>,
}

struct ReaderBytes {
    bytes: Bytes,
    _memory: Arc<Mutex<ReaderReservation>>,
}

impl AsRef<[u8]> for ReaderBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl FrameReader {
    #[cfg(test)]
    fn new(source: Box<dyn pipe::Source>) -> Self {
        Self {
            source,
            decoder: FrameDecoder::new(),
            opener: None,
            records: RecordDecoder::new(),
            memory: None,
        }
    }

    fn with_memory(source: Box<dyn pipe::Source>, memory: Arc<MemoryReservation>) -> Self {
        Self {
            source,
            decoder: FrameDecoder::new(),
            opener: None,
            records: RecordDecoder::new(),
            memory: Some(Arc::new(Mutex::new(ReaderReservation {
                session: memory,
                leases: Vec::new(),
                high_water: 0,
            }))),
        }
    }

    fn reserve_input(&self, extra: usize) -> io::Result<()> {
        if let Some(memory) = &self.memory {
            let bytes = self
                .decoder
                .buffer
                .capacity()
                .checked_add(self.records.capacity())
                .and_then(|bytes| bytes.checked_add(extra))
                .ok_or_else(memory_exhausted)?;
            memory.lock().unwrap().ensure(bytes)?;
        }
        Ok(())
    }

    fn retain_memory(&self, bytes: Bytes) -> Bytes {
        match &self.memory {
            Some(memory) => Bytes::from_owner(ReaderBytes {
                bytes,
                _memory: memory.clone(),
            }),
            None => bytes,
        }
    }

    /// Read the next frame. Returns `None` if the body has ended.
    async fn next_frame(&mut self) -> io::Result<Option<(u8, u64, Bytes)>> {
        if !self.decoder.invalid && self.decoder.buffer.is_empty() && self.records.is_empty() {
            if let Some(memory) = &self.memory {
                if Arc::strong_count(memory) == 1 {
                    self.decoder = FrameDecoder::new();
                    self.records = RecordDecoder::new();
                    let mut reservation = memory.lock().unwrap();
                    reservation.leases.clear();
                    reservation.high_water = 0;
                }
            }
        }
        loop {
            if let Some(opener) = self.opener.as_mut() {
                // The encrypted records: the frames are inside them.
                loop {
                    match self.records.take_record() {
                        RecordTake::Incomplete => break,
                        RecordTake::Invalid => {
                            // The length of the record is not valid: the stream
                            // is broken, the session must not keep the buffer.
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "The length of the XHTTP record is not valid",
                            ));
                        }
                        RecordTake::Record(record) => {
                            let plain = opener.open(&record).map_err(|e| {
                                io::Error::new(io::ErrorKind::InvalidData, e.to_string())
                            })?;
                            self.decoder.push(&plain);
                        }
                    }
                }

                match self.decoder.next_frame() {
                    DecodedFrame::Frame {
                        kind,
                        offset,
                        payload,
                    } => return Ok(Some((kind, offset, self.retain_memory(payload)))),
                    DecodedFrame::Invalid(e) => {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, e))
                    }
                    DecodedFrame::Incomplete => (),
                }
            } else {
                // The plain frames: the first frame of the session is not encrypted.
                match self.decoder.next_frame() {
                    DecodedFrame::Frame {
                        kind,
                        offset,
                        payload,
                    } => return Ok(Some((kind, offset, self.retain_memory(payload)))),
                    DecodedFrame::Invalid(e) => {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, e))
                    }
                    DecodedFrame::Incomplete => (),
                }
            }

            match self.source.read().await? {
                pipe::Data::Chunk(bytes) => {
                    let len = bytes.len();
                    self.reserve_input(len)?;
                    if self.opener.is_none() {
                        self.decoder.push(&bytes);
                    } else {
                        self.records.push(&bytes)?;
                    }
                    self.source.consume(len)?;
                }
                pipe::Data::Eof => {
                    return match self.decoder.next_frame() {
                        DecodedFrame::Incomplete
                            if self.decoder.buffer.is_empty() && self.records.is_empty() =>
                        {
                            Ok(None)
                        }
                        DecodedFrame::Incomplete => Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Truncated XHTTP body",
                        )),
                        DecodedFrame::Invalid(e) => Err(io::Error::other(e)),
                        DecodedFrame::Frame {
                            kind,
                            offset,
                            payload,
                        } => Ok(Some((kind, offset, self.retain_memory(payload)))),
                    }
                }
            }
        }
    }
}

/// Parse the destination address from the payload of the `OPEN` frame.
fn parse_destination(payload: &Bytes) -> io::Result<TcpDestination> {
    let text = std::str::from_utf8(payload)
        .map_err(|_| io::Error::other("The destination address is not a valid UTF-8 string"))?;
    let authority: http::uri::Authority = text
        .parse()
        .map_err(|_| io::Error::other("The destination address is malformed"))?;

    let port = authority
        .port_u16()
        .ok_or_else(|| io::Error::other("The destination address lacks a port"))?;

    match authority.as_str().parse() {
        Ok(addr) => Ok(TcpDestination::Address(addr)),
        Err(_) => Ok(TcpDestination::HostName((
            authority.host().to_string(),
            port,
        ))),
    }
}

/// The identity of the client the session belongs to.
fn session_owner(
    authenticator: Option<&Arc<dyn Authenticator>>,
    source: Option<&Source<'_>>,
    id: &log_utils::IdChain<u64>,
) -> Option<String> {
    let Some(authenticator) = authenticator else {
        // Without an authenticator the endpoint serves everybody: the session
        // identifier is the only secret.
        return Some("anonymous".to_string());
    };

    let source = source?;
    match authenticator.authenticate(source, id) {
        Status::Pass => Some(
            authenticator
                .username(source)
                .unwrap_or_else(|| format!("{source:?}")),
        ),
        Status::Reject => None,
    }
}

/// Handle a single XHTTP request: either a half of a session or a request
/// which does not belong to any session.
pub(crate) async fn handle_request(
    context: Arc<core::Context>,
    stream: Box<dyn http_codec::Stream>,
    tls_domain: String,
) {
    let id = stream.id();
    let (request, respond) = stream.split();

    let method = request.request().method.clone();
    let path = request.request().uri.path().to_string();

    let Some(sid) = parse_sid(&path, context.settings.xhttp_path.as_deref()) else {
        log_id!(debug, id, "XHTTP: malformed session identifier");
        let _ = respond.send_bad_response(http::StatusCode::NOT_FOUND, vec![]);
        return;
    };

    let auth_source = match request.auth_info() {
        Ok(x) => x.map(Source::into_owned),
        Err(e) => {
            log_id!(
                debug,
                id,
                "XHTTP: failed to get the authorization info: {}",
                e
            );
            let _ = respond.send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
            return;
        }
    };

    let decoy_only = context.is_decoy_only(request.client_address().ok());
    let owner = if decoy_only {
        None
    } else {
        session_owner(context.authenticator.as_ref(), auth_source.as_ref(), &id)
    };
    let Some(owner) = owner else {
        log_id!(debug, id, "XHTTP: authentication failed");
        let _ = respond.send_bad_response(
            context
                .settings
                .auth_failure_status_code
                .try_into()
                .unwrap_or(http::StatusCode::PROXY_AUTHENTICATION_REQUIRED),
            vec![],
        );
        return;
    };

    let client_address = match request.client_address() {
        Ok(x) => x,
        Err(e) => {
            log_id!(debug, id, "XHTTP: failed to get the client address: {}", e);
            let _ = respond.send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
            return;
        }
    };
    let user_agent = request.user_agent();
    let versions = request.request().headers.get_all("x-xhttp-version");
    let mut versions = versions.iter();
    if versions.next() != Some(&http::HeaderValue::from_static("1"))
        || versions.next().is_some()
        || !context.settings.xhttp_supported_versions.contains(&1)
    {
        let _ = respond.send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
        return;
    }

    let half = match method {
        http::Method::GET => Half::Down(DownHalf {
            respond,
            id: id.clone(),
            client_address,
            user_agent,
            tls_domain,
        }),
        http::Method::POST => {
            let body = request.finalize();
            Half::Up(UpHalf { body, respond })
        }
        _ => {
            log_id!(debug, id, "XHTTP: unsupported method: {}", method);
            let _ = respond.send_bad_response(http::StatusCode::METHOD_NOT_ALLOWED, vec![]);
            return;
        }
    };

    let outcome = match join_half(&context.xhttp, &sid, &owner, half, &context).await {
        Ok(x) => x,
        Err(JoinOutcome::OwnerMismatch) => {
            log_id!(debug, id, "XHTTP: the session belongs to another client");
            return;
        }
        Err(JoinOutcome::Timeout) => {
            log_id!(debug, id, "XHTTP: the session pair has not arrived in time");
            return;
        }
        Err(JoinOutcome::TooMany) => {
            log_id!(debug, id, "XHTTP: too many pending sessions");
            return;
        }
        Err(_) => {
            log_id!(debug, id, "XHTTP: the session cannot be joined");
            return;
        }
    };

    match outcome {
        JoinOutcome::OwnerMismatch | JoinOutcome::Timeout | JoinOutcome::TooMany => {
            // The errors are reported above.
        }
        JoinOutcome::Leader {
            down,
            up,
            memory,
            lease,
        } => {
            log_id!(debug, id, "XHTTP: session paired");
            start_session(context, sid, owner, (down, up), auth_source, memory, lease).await;
        }
        JoinOutcome::Passed => {
            // The other half of the session owns it now.
        }
        JoinOutcome::Live { session, half } => {
            log_id!(debug, id, "XHTTP: the session is resumed");
            match half {
                Half::Down(down) => {
                    attach_down(&session, down).await;
                }
                Half::Up(up) => {
                    resume_session(context, session, up).await;
                }
            }
        }
    }
}

/// Extract the session identifier from the request path.
fn parse_sid(path: &str, prefix: Option<&str>) -> Option<String> {
    let prefix = prefix?;
    let sid = path.strip_prefix(prefix)?;
    if sid.is_empty() || sid.len() > MAX_SID_LEN {
        return None;
    }
    if !sid
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    Some(sid.to_string())
}

/// Build the response of the downlink.
fn downlink_response() -> ResponseHeaders {
    let mut response: ResponseHeaders = http::Response::builder()
        .status(http::StatusCode::OK)
        .body(())
        .expect("the response is valid")
        .into_parts()
        .0;
    response
        .headers
        .insert("cache-control", http::HeaderValue::from_static("no-store"));
    response
        .headers
        .insert("x-accel-buffering", http::HeaderValue::from_static("no"));
    response
}

/// Build the response of the uplink.
fn uplink_response() -> ResponseHeaders {
    let mut response: ResponseHeaders = http::Response::builder()
        .status(http::StatusCode::OK)
        .body(())
        .expect("the response is valid")
        .into_parts()
        .0;
    response
        .headers
        .insert("cache-control", http::HeaderValue::from_static("no-store"));
    response
        .headers
        .insert("x-xhttp-version", http::HeaderValue::from_static("1"));
    response
}

fn random_secret() -> io::Result<[u8; 32]> {
    use ring::rand::SecureRandom;
    let mut value = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| io::Error::other("XHTTP random generator failed"))?;
    Ok(value)
}

fn resume_message(sid: &str, offsets: &[u8], challenge: &[u8; 32]) -> Vec<u8> {
    let mut message = b"cattunnel-xhttp-resume-v1".to_vec();
    message.extend_from_slice(sid.as_bytes());
    message.extend_from_slice(offsets);
    message.extend_from_slice(challenge);
    message
}

fn verify_resume_proof(key: &[u8; 32], sid: &str, payload: &[u8], challenge: &[u8; 32]) -> bool {
    if payload.len() != RESUME_PAYLOAD_LEN + 32 {
        return false;
    }
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::verify(
        &key,
        &resume_message(sid, &payload[..16], challenge),
        &payload[16..],
    )
    .is_ok()
}

fn versioned_response(challenge: Option<&[u8; 32]>) -> ResponseHeaders {
    let mut response = downlink_response();
    response
        .headers
        .insert("x-xhttp-version", http::HeaderValue::from_static("1"));
    if let Some(challenge) = challenge {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(challenge);
        response.headers.insert(
            "x-xhttp-challenge",
            http::HeaderValue::from_str(&encoded).unwrap(),
        );
    }
    response
}

/// Attach the downlink half to an existing session: send the response and put
/// the sink of the body into the session.
async fn attach_down(session: &Arc<Session>, down: DownHalf) {
    let challenge = match random_secret() {
        Ok(value) => Some(value),
        Err(_) => return,
    };
    let mut inner = session.inner.lock().await;
    if inner.down_sink.is_some() || inner.terminal {
        let _ = down
            .respond
            .send_bad_response(http::StatusCode::CONFLICT, vec![]);
        return;
    }
    let sink = match down
        .respond
        .send_response(versioned_response(challenge.as_ref()), false)
    {
        Ok(x) => x.into_pipe_sink(),
        Err(_) => return,
    };
    inner.down_sink = Some(sink);
    inner.resume_challenge = challenge;
    drop(inner);
    session.down_ready.notify_waiters();
    let weak = Arc::downgrade(session);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        if let Some(session) = weak.upgrade() {
            let mut inner = session.inner.lock().await;
            if inner.resume_challenge == challenge {
                inner.down_sink.take();
                inner.resume_challenge.take();
            }
        }
    });
}

/// Wait for the downlink half of a resumed session.
async fn wait_for_down_sink(
    session: &Arc<Session>,
) -> Option<(Box<dyn pipe::Sink>, Option<[u8; 32]>)> {
    let deadline = Duration::from_secs(10);
    let wait = async {
        loop {
            let notified = session.down_ready.notified();
            let sink = {
                let mut inner = session.inner.lock().await;
                inner
                    .down_sink
                    .take()
                    .map(|sink| (sink, inner.resume_challenge.take()))
            };
            if let Some(sink) = sink {
                return Some(sink);
            }
            notified.await;
        }
    };

    tokio::time::timeout(deadline, wait).await.ok().flatten()
}

/// Read the next frame of the uplink, or report the failure.
async fn read_frame(reader: &mut FrameReader) -> io::Result<(u8, u64, Bytes)> {
    match tokio::time::timeout(PAIR_TIMEOUT, reader.next_frame())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "XHTTP handshake timeout"))??
    {
        Some(x) => Ok(x),
        None => Err(io::Error::other("The uplink body is empty")),
    }
}

async fn write_handshake(sink: &mut Box<dyn pipe::Sink>, data: Bytes) -> io::Result<()> {
    tokio::time::timeout(PAIR_TIMEOUT, sink.write_all(data))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "XHTTP handshake write timeout"))?
}

/// Start a new session: read the first frame of the uplink, connect to the
/// destination and pump the data.
async fn start_session(
    context: Arc<core::Context>,
    sid: String,
    owner: String,
    halves: (DownHalf, UpHalf),
    auth: Option<Source<'static>>,
    memory: Arc<MemoryReservation>,
    _lease: PendingLease,
) {
    let (down, up) = halves;
    let id = down.id.clone();
    let mut reader = FrameReader::with_memory(up.body, memory.clone());

    // The response of the downlink is sent before the destination is
    // connected: the key exchange is answered in its body.
    let mut down_sink = match down.respond.send_response(versioned_response(None), false) {
        Ok(x) => x.into_pipe_sink(),
        Err(e) => {
            log_id!(
                debug,
                id,
                "XHTTP: failed to send the downlink response: {}",
                e
            );
            let _ = up
                .respond
                .send_bad_response(http::StatusCode::BAD_GATEWAY, vec![]);
            return;
        }
    };

    // The first frame of the uplink: the key exchange, the liveness check or
    // the destination address.
    let (kind, _, payload) = match read_frame(&mut reader).await {
        Ok(x) => x,
        Err(e) => {
            log_id!(debug, id, "XHTTP: failed to read the uplink body: {}", e);
            let _ = up
                .respond
                .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
            return;
        }
    };

    let mut session_sealer = None;
    if kind == FRAME_HELLO {
        match start_crypto(&context, &sid, &payload) {
            Some((sealer, opener, ack)) => {
                // The answer of the endpoint goes into the body of the downlink.
                let frame = encode_frame(FRAME_HELLO_ACK, 0, &ack);
                if let Err(e) = write_handshake(&mut down_sink, frame).await {
                    log_id!(
                        debug,
                        id,
                        "XHTTP: failed to send the key exchange answer: {}",
                        e
                    );
                    let _ = up
                        .respond
                        .send_bad_response(http::StatusCode::BAD_GATEWAY, vec![]);
                    return;
                }
                if reader
                    .records
                    .push(&reader.decoder.buffer.split().freeze())
                    .is_err()
                {
                    let _ = up
                        .respond
                        .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
                    return;
                }
                reader.opener = Some(opener);
                session_sealer = Some(sealer);
            }
            None => {
                let _ = up
                    .respond
                    .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
                return;
            }
        }
    }

    // The second frame, if the key exchange has happened.
    let (kind, _, payload) = if reader.opener.is_some() {
        drop(payload);
        match read_frame(&mut reader).await {
            Ok(x) => x,
            Err(e) => {
                log_id!(debug, id, "XHTTP: failed to read the uplink body: {}", e);
                let _ = up
                    .respond
                    .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
                return;
            }
        }
    } else {
        (kind, 0, payload)
    };

    if kind == FRAME_PING {
        run_ping(id, down_sink, up.respond, &payload, session_sealer).await;
        return;
    }

    if kind != FRAME_OPEN {
        log_id!(debug, id, "XHTTP: unexpected first frame type: {}", kind);
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
        return;
    }

    // The destination address.
    let destination = match parse_destination(&payload) {
        Ok(x) => x,
        Err(e) => {
            log_id!(debug, id, "XHTTP: invalid destination: {}", e);
            let _ = up
                .respond
                .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
            return;
        }
    };

    drop(payload);
    let resume_key = {
        let key = match random_secret() {
            Ok(key) => key,
            Err(_) => return,
        };
        let token = encode_frame(FRAME_RESUME_TOKEN, 0, &key);
        let token = match session_sealer.as_mut() {
            Some(sealer) => match sealer.seal(&token) {
                Ok(record) => Bytes::from(record),
                Err(_) => return,
            },
            None => token,
        };
        if write_handshake(&mut down_sink, token).await.is_err() {
            return;
        }
        key
    };

    let meta = forwarder::TcpConnectionMeta {
        client_address: down.client_address,
        destination,
        auth,
        tls_domain: down.tls_domain,
        user_agent: down.user_agent,
    };

    let connector = core::Core::make_forwarder(context.clone()).tcp_connector();

    log_id!(debug, id, "XHTTP: connecting to the destination");
    let (peer_source, peer_sink) = match tokio::time::timeout(
        context.settings.connection_establishment_timeout,
        connector.connect(id.clone(), meta),
    )
    .await
    .unwrap_or(Err(tunnel::ConnectionError::Timeout))
    {
        Ok(x) => x,
        Err(e) => {
            log_id!(debug, id, "XHTTP: failed to connect: {}", e);
            let _ = up
                .respond
                .send_bad_response(http::StatusCode::BAD_GATEWAY, vec![]);
            return;
        }
    };

    // The session is established: it is registered, so it may be resumed.
    let (down_acked, _) = watch::channel(0u64);
    let session = Arc::new(Session {
        sid: sid.clone(),
        owner: owner.clone(),
        id: id.clone(),
        inner: tokio::sync::Mutex::new(SessionInner {
            up_written: 0,
            up_acked_sent: 0,
            down_sent: 0,
            down_buffer: ReplayBuffer::default(),
            down_pending: None,
            down_buffer_start: 0,
            down_sink: None,
            resume_challenge: None,
            sealer: session_sealer,
            connections: 0,
            terminal: false,
            down_eof: false,
            down_fin_acked: false,
            up_eof: false,
            cancel: Arc::new(CancelSignal::new()),
        }),
        down_acked,
        unacked_limit: context.settings.xhttp_unacked_limit,
        _memory: memory,
        resume_lock: tokio::sync::Mutex::new(()),
        resume_key,
        down_ready: tokio::sync::Notify::new(),
        up_written_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        peer: Arc::new(PeerSlot {
            sink: std::sync::Mutex::new(Some(peer_sink)),
            source: std::sync::Mutex::new(Some(peer_source)),
            ready: tokio::sync::Notify::new(),
        }),
    });

    context.xhttp.register_live(session.clone());

    // The response of the uplink: the stream stays open until the session is
    // over, so the client may keep sending the frames.
    let up_sink = match up.respond.send_response(uplink_response(), false) {
        Ok(x) => x.into_pipe_sink(),
        Err(e) => {
            log_id!(
                debug,
                id,
                "XHTTP: failed to send the uplink response: {}",
                e
            );
            context.xhttp.unregister_live(&sid, &session);
            return;
        }
    };

    log_id!(debug, id, "XHTTP: session established");

    let generation = pump_session(session.clone(), reader, up_sink, down_sink).await;

    // The session stays in the registry: the client may resume it.
    spawn_reaper(context, session, generation).await;
}

/// Run a session which is resumed after the death of the connection.
async fn resume_session(context: Arc<core::Context>, session: Arc<Session>, up: UpHalf) {
    let id = session.id.clone();
    let Ok(_resume_guard) = session.resume_lock.try_lock() else {
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::CONFLICT, vec![]);
        return;
    };

    // The session is not kept for the resume: the timeout is zero.  The
    // client is told that the session is over, so it closes the connections of
    // the application instead of waiting for the data which will not come.
    if context.settings.xhttp_resume_timeout.is_zero() {
        log_id!(
            debug,
            id,
            "XHTTP: the session is not resumed, the timeout is zero"
        );
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
        return;
    }

    let mut reader = FrameReader::with_memory(up.body, session._memory.clone());
    let mut replacement_sealer = None;

    // The first frame of the uplink of a resumed session is the key exchange
    // or the resume itself.
    let (kind, _, payload) = match read_frame(&mut reader).await {
        Ok(x) => x,
        Err(e) => {
            log_id!(debug, id, "XHTTP: failed to read the uplink body: {}", e);
            let _ = up
                .respond
                .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
            return;
        }
    };

    // The downlink of the session: the `GET` half may arrive later.
    let Some((mut down_sink, challenge)) = wait_for_down_sink(&session).await else {
        log_id!(debug, id, "XHTTP: the downlink of the session is not ready");
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
        return;
    };

    if kind == FRAME_HELLO {
        let Some((sealer, opener, ack)) = start_crypto(&context, &session.sid, &payload) else {
            let _ = up
                .respond
                .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
            return;
        };
        if write_handshake(&mut down_sink, encode_frame(FRAME_HELLO_ACK, 0, &ack))
            .await
            .is_err()
        {
            return;
        }
        replacement_sealer = Some(sealer);
        if reader
            .records
            .push(&reader.decoder.buffer.split().freeze())
            .is_err()
        {
            return;
        }
        reader.opener = Some(opener);
    } else if session.inner.lock().await.sealer.is_some() {
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
        return;
    }

    // The next frame is the resume itself.
    let (kind, _, payload) = if reader.opener.is_some() {
        drop(payload);
        match read_frame(&mut reader).await {
            Ok(x) => x,
            Err(e) => {
                log_id!(debug, id, "XHTTP: failed to read the uplink body: {}", e);
                let _ = up
                    .respond
                    .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
                return;
            }
        }
    } else {
        (kind, 0, payload)
    };

    if kind != FRAME_RESUME {
        log_id!(debug, id, "XHTTP: unexpected frame type: {}", kind);
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
        return;
    }

    let Some(challenge) = challenge else {
        return;
    };
    if !verify_resume_proof(&session.resume_key, &session.sid, &payload, &challenge) {
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::FORBIDDEN, vec![]);
        return;
    }
    let offsets = Bytes::copy_from_slice(&payload[..16]);
    drop(payload);
    // Validate before cancelling the old transport.
    if !valid_resume_offsets(&session, &offsets).await {
        let _ = up
            .respond
            .send_bad_response(http::StatusCode::BAD_REQUEST, vec![]);
        return;
    }
    let old_cancel = session.inner.lock().await.cancel.clone();
    old_cancel.raise();
    // Wait until both old guards have returned, then apply offsets against the final state.
    let Some(sink_guard) = take_sink(&session).await else {
        return;
    };
    let Some(source_guard) = take_source(&session).await else {
        return;
    };
    if apply_resume(&session, &offsets).await.is_err() {
        return;
    }
    drop(offsets);
    {
        let mut inner = session.inner.lock().await;
        inner.cancel = Arc::new(CancelSignal::new());
        if let Some(sealer) = replacement_sealer {
            inner.sealer = Some(sealer);
        }
    }
    drop(sink_guard);
    drop(source_guard);

    // The response of the uplink.
    let up_sink = match up.respond.send_response(uplink_response(), false) {
        Ok(x) => x.into_pipe_sink(),
        Err(e) => {
            log_id!(
                debug,
                id,
                "XHTTP: failed to send the uplink response: {}",
                e
            );
            return;
        }
    };

    log_id!(debug, id, "XHTTP: session resumed");
    drop(_resume_guard);

    let generation = pump_session(session.clone(), reader, up_sink, down_sink).await;

    // The session stays in the registry: the client may resume it.
    spawn_reaper(context, session, generation).await;
}

/// Remove the session when the client does not resume it in time.
async fn spawn_reaper(
    context: Arc<core::Context>,
    session: Arc<Session>,
    generation: Arc<CancelSignal>,
) {
    let timeout = if session.inner.lock().await.terminal {
        Duration::ZERO
    } else {
        context.settings.xhttp_resume_timeout
    };
    let sid = session.sid.clone();
    let session = Arc::downgrade(&session);
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        let Some(session) = session.upgrade() else {
            return;
        };
        let connected = {
            let inner = session.inner.lock().await;
            inner.connections > 0 || !Arc::ptr_eq(&generation, &inner.cancel)
        };
        if !connected {
            context.xhttp.unregister_live(&sid, &session);
        }
    });
}

async fn valid_resume_offsets(session: &Arc<Session>, payload: &[u8]) -> bool {
    if payload.len() != RESUME_PAYLOAD_LEN {
        return false;
    }
    let up = u64::from_be_bytes(payload[..8].try_into().unwrap());
    let down = u64::from_be_bytes(payload[8..].try_into().unwrap());
    let inner = session.inner.lock().await;
    !inner.terminal
        && up <= inner.up_written
        && down >= inner.down_buffer_start
        && down <= inner.down_sent
}

/// Apply the information of the `RESUME` frame.
async fn apply_resume(session: &Arc<Session>, payload: &Bytes) -> io::Result<()> {
    if payload.len() != RESUME_PAYLOAD_LEN {
        return Err(io::Error::other("Invalid RESUME length"));
    }

    let up_confirmed = u64::from_be_bytes(payload[..8].try_into().unwrap());
    let down_received = u64::from_be_bytes(payload[8..16].try_into().unwrap());

    let mut inner = session.inner.lock().await;
    if inner.terminal
        || up_confirmed > inner.up_written
        || down_received < inner.down_buffer_start
        || down_received > inner.down_sent
    {
        return Err(io::Error::other(
            "Invalid RESUME offsets or terminal session",
        ));
    }

    // The client repeats the data it has not confirmed: the endpoint drops the
    // part it has already written, this is done by the offsets of the frames.
    //
    // The client has received the data up to `down_received`, so it does not
    // need it again.
    if down_received > inner.down_buffer_start {
        let confirmed = (down_received - inner.down_buffer_start) as usize;
        if confirmed >= inner.down_buffer.len() {
            inner.down_buffer.clear();
        } else {
            inner.down_buffer.acknowledge(confirmed);
        }
        inner.down_buffer_start = down_received;
        session.down_acked.send_replace(down_received);
    }
    // Repeat an ACK which may have been lost with the previous downlink.
    inner.up_acked_sent = up_confirmed;
    Ok(())
}

/// Perform the key exchange of a new session.
fn start_crypto(
    context: &Arc<core::Context>,
    sid: &str,
    payload: &Bytes,
) -> Option<(Sealer, Opener, [u8; crate::xhttp_crypto::HELLO_ACK_LEN])> {
    let seed = context
        .settings
        .xhttp_secret_key
        .as_ref()
        .and_then(|x| crate::xhttp_crypto::decode_seed(x))?;

    crate::xhttp_crypto::server_start(sid, &seed, payload).ok()
}

/// Answer the liveness check.
async fn run_ping(
    id: log_utils::IdChain<u64>,
    mut sink: Box<dyn pipe::Sink>,
    up_respond: Box<dyn PendingRespond>,
    payload: &Bytes,
    sealer: Option<Sealer>,
) {
    let frame = encode_frame(FRAME_PONG, 0, payload);

    let data = match sealer {
        Some(mut sealer) => match sealer.seal(&frame) {
            Ok(record) => Bytes::from(record),
            Err(e) => {
                log_id!(debug, id, "XHTTP: cannot seal the answer: {}", e);
                return;
            }
        },
        None => frame,
    };

    if let Err(e) = write_handshake(&mut sink, data).await {
        log_id!(debug, id, "XHTTP: failed to send the answer: {}", e);
        return;
    }
    let _ = sink.eof();

    if let Ok(x) = up_respond.send_response(uplink_response(), false) {
        let mut up_sink = x.into_pipe_sink();
        let _ = up_sink.eof();
    }

    log_id!(debug, id, "XHTTP: the liveness check is answered");
}

/// Pump the data of a session until the connection dies.
async fn pump_session(
    session: Arc<Session>,
    mut reader: FrameReader,
    mut up_sink: Box<dyn pipe::Sink>,
    down_sink: Box<dyn pipe::Sink>,
) -> Arc<CancelSignal> {
    let id = session.id.clone();

    // The cancel signal of this connection: it is raised when the session is
    // resumed, so the pumps of the old connection stop and release the socket
    // of the destination.
    let cancel = {
        let mut inner = session.inner.lock().await;
        inner.connections += 1;
        inner.cancel.clone()
    };

    // The socket of the destination is taken by the pumps, so it survives the
    // death of the connection.
    let Some(mut sink_guard) = take_sink(&session).await else {
        log_id!(debug, id, "XHTTP: the destination is not connected");
        {
            let mut inner = session.inner.lock().await;
            inner.connections = inner.connections.saturating_sub(1);
        }
        let _ = up_sink.eof();
        return cancel;
    };

    let Some(mut source_guard) = take_source(&session).await else {
        log_id!(debug, id, "XHTTP: the destination is not connected");
        {
            let mut inner = session.inner.lock().await;
            inner.connections = inner.connections.saturating_sub(1);
        }
        let _ = up_sink.eof();
        return cancel;
    };

    {
        let mut uplink = Box::pin(pump_uplink(
            session.clone(),
            &mut reader,
            sink_guard.get(),
            &cancel,
        ));
        let mut downlink = Box::pin(pump_downlink(
            session.clone(),
            source_guard.get(),
            down_sink,
            &cancel,
        ));

        // The pumps are not dropped when one of them is over: a dropped pump
        // may lose the data it has read from the destination, and the client
        // would get a hole in the stream.  The other pump is stopped with the
        // cancel signal and is waited for.
        let stop = cancel.clone();
        tokio::select! {
            r = &mut uplink => {
                if let Err(e) = r {
                    log_id!(debug, id, "XHTTP: uplink error: {}", e);
                }
                stop.raise();
                if let Err(e) = downlink.await {
                    log_id!(debug, id, "XHTTP: downlink error: {}", e);
                }
            }
            r = &mut downlink => {
                if let Err(e) = r {
                    log_id!(debug, id, "XHTTP: downlink error: {}", e);
                }
                stop.raise();
                if let Err(e) = uplink.await {
                    log_id!(debug, id, "XHTTP: uplink error: {}", e);
                }
            }
        }
    }

    // The session is over: close the uplink stream.
    let _ = up_sink.eof();

    // The destination socket is returned by the guards; resumed encrypted
    // transports establish fresh keys and counters.
    {
        let mut inner = session.inner.lock().await;
        inner.connections = inner.connections.saturating_sub(1);
    }

    log_id!(debug, id, "XHTTP: session closed");
    cancel
}

/// Take the write half of the socket of the destination.
async fn take_sink(session: &Arc<Session>) -> Option<SinkGuard> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let notified = session.peer.ready.notified();
        {
            let mut slot = session.peer.sink.lock().unwrap();
            if let Some(sink) = slot.take() {
                return Some(SinkGuard::new(session.peer.clone(), sink));
            }
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return None;
        }
    }
}

/// Take the read half of the socket of the destination.
async fn take_source(session: &Arc<Session>) -> Option<SourceGuard> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let notified = session.peer.ready.notified();
        {
            let mut slot = session.peer.source.lock().unwrap();
            if let Some(source) = slot.take() {
                return Some(SourceGuard::new(session.peer.clone(), source));
            }
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return None;
        }
    }
}

/// Pump the frames from the uplink to the destination.
async fn pump_uplink(
    session: Arc<Session>,
    reader: &mut FrameReader,
    sink: &mut Box<dyn pipe::Sink>,
    cancel: &CancelSignal,
) -> io::Result<()> {
    loop {
        let frame = tokio::select! {
            r = reader.next_frame() => match r {
                Ok(frame) => frame,
                Err(e) => {
                    if e.kind() == io::ErrorKind::InvalidData {
                        session.inner.lock().await.terminal = true;
                    }
                    return Err(e);
                }
            },
            _ = cancel.wait() => {
                log_id!(debug, session.id, "XHTTP: the uplink is stopped");
                return Ok(());
            }
        };

        let Some((kind, offset, payload)) = frame else {
            if session.inner.lock().await.up_eof {
                cancel.wait().await;
            }
            return Ok(());
        };

        match kind {
            FRAME_DATA => {
                if session.inner.lock().await.up_eof {
                    return Err(io::Error::other("DATA after FIN"));
                }
                let written = session.inner.lock().await.up_written;
                if offset > written {
                    return Err(io::Error::other("Unexpected frame offset"));
                }
                let skip = written - offset;
                if skip >= payload.len() as u64 {
                    continue;
                }
                let mut pending = payload.slice(skip as usize..);
                while !pending.is_empty() {
                    let before = pending.len();
                    pending = match sink.write(pending) {
                        Ok(rest) => rest,
                        Err(e) => {
                            session.inner.lock().await.terminal = true;
                            return Err(e);
                        }
                    };
                    let accepted = before - pending.len();
                    if accepted > 0 {
                        let mut inner = session.inner.lock().await;
                        inner.up_written = inner
                            .up_written
                            .checked_add(accepted as u64)
                            .ok_or_else(|| io::Error::other("Uplink offset overflow"))?;
                        drop(inner);
                        session.up_written_notify.notify_one();
                    }
                    if !pending.is_empty() {
                        tokio::select! {
                            r = sink.wait_writable() => r?,
                            _ = cancel.wait() => return Ok(()),
                        }
                    }
                }
            }
            FRAME_ACK => {
                // The client confirms the downlink bytes.
                let mut inner = session.inner.lock().await;
                let fin_ack = payload.as_ref() == [FRAME_FIN];
                if (!payload.is_empty() && !fin_ack)
                    || offset > inner.down_sent
                    || (fin_ack && (!inner.down_eof || offset != inner.down_sent))
                {
                    return Err(io::Error::other("Unexpected acknowledgement"));
                }
                log_id!(
                    trace,
                    session.id,
                    "XHTTP: the client confirms {} bytes, the buffer starts at {}",
                    offset,
                    inner.down_buffer_start
                );
                if offset > inner.down_buffer_start {
                    let confirmed = (offset - inner.down_buffer_start) as usize;
                    if confirmed >= inner.down_buffer.len() {
                        inner.down_buffer.clear();
                    } else {
                        inner.down_buffer.acknowledge(confirmed);
                    }
                    inner.down_buffer_start = offset;
                }
                if fin_ack {
                    inner.down_fin_acked = true;
                    inner.terminal = true;
                    session.up_written_notify.notify_waiters();
                }
                drop(inner);
                session.down_acked.send_if_modified(|acked| {
                    if offset > *acked {
                        *acked = offset;
                        true
                    } else {
                        false
                    }
                });
            }
            FRAME_FIN => {
                log_id!(
                    debug,
                    session.id,
                    "XHTTP: the uplink is closed by the client"
                );
                if !payload.is_empty() || offset != session.inner.lock().await.up_written {
                    return Err(io::Error::other("Invalid FIN"));
                }
                if !session.inner.lock().await.up_eof {
                    sink.eof()?;
                    sink.flush().await?;
                    session.inner.lock().await.up_eof = true;
                }
            }
            FRAME_RST => {
                log_id!(
                    debug,
                    session.id,
                    "XHTTP: the uplink is reset by the client"
                );
                return Err(io::Error::other("The session has been reset"));
            }
            FRAME_OPEN | FRAME_RESUME | FRAME_PING | FRAME_PONG | FRAME_HELLO | FRAME_HELLO_ACK
            | FRAME_RESUME_TOKEN => {
                return Err(io::Error::other("Unexpected frame"));
            }
            FRAME_PAD => (),
            other => {
                log_id!(debug, session.id, "XHTTP: unknown frame type: {}", other);
            }
        }
    }
}

async fn finish_down(
    session: &Arc<Session>,
    sink: &mut Box<dyn pipe::Sink>,
    cancel: &CancelSignal,
) -> io::Result<()> {
    let offset = session.inner.lock().await.down_sent;
    write_down(session, sink, cancel, encode_frame(FRAME_FIN, offset, &[])).await?;
    {
        let wait = async {
            loop {
                let notified = session.up_written_notify.notified();
                if session.inner.lock().await.down_fin_acked {
                    break;
                }
                tokio::select! {
                    _ = notified => (),
                    _ = cancel.wait() => break,
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(10), wait).await;
    }
    sink.eof()
}

/// Pump the data from the destination to the downlink.
async fn pump_downlink(
    session: Arc<Session>,
    source: &mut Box<dyn pipe::Source>,
    mut sink: Box<dyn pipe::Sink>,
    cancel: &CancelSignal,
) -> io::Result<()> {
    // The offset of the frames is the number of the bytes taken from the
    // destination: it continues where the previous connection stopped.
    //
    // The data which the client has not confirmed is repeated: it may have been
    // lost together with the connection.
    let (pending_start, pending, down_eof) = {
        let inner = session.inner.lock().await;
        (
            inner.down_buffer_start,
            inner.down_buffer.clone(),
            inner.down_eof,
        )
    };

    if !pending.is_empty() {
        log_id!(
            debug,
            session.id,
            "XHTTP: {} bytes are repeated after the resume, from {} to {}",
            pending.len(),
            pending_start,
            pending_start + pending.len() as u64
        );

        let mut offset = pending_start;
        for chunk in pending.chunks(DOWNSINK_CHUNK) {
            let frame = encode_frame(FRAME_DATA, offset, &chunk);
            offset += chunk.len() as u64;
            write_down(&session, &mut sink, cancel, frame).await?;
        }
    }

    drop(pending);
    if down_eof {
        return finish_down(&session, &mut sink, cancel).await;
    }

    let up_notify = session.up_written_notify.clone();
    loop {
        // Send the confirmation of the uplink which may have become due while
        // the downlink was busy: the permit stored by notify_one is not lost,
        // but the condition is also checked here, so a missed wake-up cannot
        // stall the client's window.
        {
            let ack = {
                let mut inner = session.inner.lock().await;
                let up_written = inner.up_written;
                if up_written > inner.up_acked_sent
                    && up_written - inner.up_acked_sent >= ACK_THRESHOLD
                {
                    inner.up_acked_sent = up_written;
                    Some(encode_frame(FRAME_ACK, up_written, &[]))
                } else {
                    None
                }
            };
            if let Some(ack) = ack {
                let sealed = seal_frame(&session, ack).await?;
                write_down_bytes(&mut sink, cancel, sealed).await?;
            }
        }

        // The read is polled first: the data which has been read from the
        // destination must not be lost, even if the connection dies at the
        // same moment.  The cancel is noticed when there is nothing to read,
        // and at the send of the frames.
        //
        // The uplink writes may arrive without any downlink data: the
        // confirmation of the uplink is then sent here, so the client does not
        // stall on a full window.
        let pending = session.inner.lock().await.down_pending.take();
        let bytes = if let Some(bytes) = pending {
            bytes
        } else {
            let bytes = tokio::select! {
            biased;
            r = source.read() => match r? {
                pipe::Data::Chunk(bytes) => bytes,
                pipe::Data::Eof => {
                    log_id!(debug, session.id, "XHTTP: the destination is closed");
                    session.inner.lock().await.down_eof = true;
                    return finish_down(&session, &mut sink, cancel).await;
                }
            },
            _ = up_notify.notified() => {
                let ack = {
                    let mut inner = session.inner.lock().await;
                    let up_written = inner.up_written;
                    if up_written > inner.up_acked_sent
                        && up_written - inner.up_acked_sent >= ACK_THRESHOLD
                    {
                        inner.up_acked_sent = up_written;
                        Some(encode_frame(FRAME_ACK, up_written, &[]))
                    } else {
                        None
                    }
                };
                if let Some(ack) = ack {
                    let sealed = seal_frame(&session, ack).await?;
                    write_down_bytes(&mut sink, cancel, sealed).await?;
                }
                continue;
            },
            _ = cancel.wait() => {
                log_id!(debug, session.id, "XHTTP: the downlink is stopped");
                return Ok(());
            }
            };
            source.consume(bytes.len())?;
            bytes
        };
        if bytes.len() > PEER_READ_CHUNK {
            session.inner.lock().await.terminal = true;
            return Err(io::Error::other(
                "XHTTP peer read exceeds the reserved chunk",
            ));
        }

        // Retain the read chunk across both flow-control and global-budget waits.
        let mut rx = session.down_acked.subscribe();
        let start = loop {
            let available = session._memory.registry.available.notified();
            tokio::pin!(available);
            available.as_mut().enable();
            let confirmed = *rx.borrow_and_update();
            {
                let mut inner = session.inner.lock().await;
                if inner.down_sent.saturating_sub(confirmed) < session.unacked_limit as u64 {
                    match inner.down_buffer.append(&bytes, &session._memory) {
                        Ok(()) => {
                            let start = inner.down_sent;
                            inner.down_sent = start
                                .checked_add(bytes.len() as u64)
                                .ok_or_else(|| io::Error::other("Downlink offset overflow"))?;
                            break start;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                        Err(error) => return Err(error),
                    }
                }
            }
            tokio::select! {
                _ = &mut available => (),
                _ = rx.changed() => (),
                _ = cancel.wait() => {
                    session.inner.lock().await.down_pending = Some(bytes);
                    return Ok(());
                }
            }
        };

        // The data is sent in the chunks of a moderate size: the frames must
        // not be too long for the records.
        let mut sent = 0usize;
        while sent < bytes.len() {
            let chunk = std::cmp::min(bytes.len() - sent, DOWNSINK_CHUNK);
            let frame = encode_frame(FRAME_DATA, start + sent as u64, &bytes[sent..sent + chunk]);

            tokio::select! {
                r = sink.write_all(seal_frame(&session, frame).await?) => r?,
                _ = cancel.wait() => {
                    log_id!(debug, session.id, "XHTTP: the downlink is stopped");
                    return Ok(());
                }
            }
            sent += chunk;
        }

        // The confirmation of the uplink is sent when the unconfirmed data
        // reaches the threshold, so the client does not keep it forever.
        let ack = {
            let mut inner = session.inner.lock().await;
            let up_written = inner.up_written;
            if up_written > inner.up_acked_sent && up_written - inner.up_acked_sent >= ACK_THRESHOLD
            {
                inner.up_acked_sent = up_written;
                Some(encode_frame(FRAME_ACK, up_written, &[]))
            } else {
                None
            }
        };

        if let Some(ack) = ack {
            write_down(&session, &mut sink, cancel, ack).await?;
        }
    }
}

async fn write_down_bytes(
    sink: &mut Box<dyn pipe::Sink>,
    cancel: &CancelSignal,
    data: Bytes,
) -> io::Result<()> {
    tokio::select! {
        r = sink.write_all(data) => r,
        _ = cancel.wait() => Err(io::Error::new(io::ErrorKind::Interrupted, "XHTTP transport cancelled")),
    }
}

async fn write_down(
    session: &Arc<Session>,
    sink: &mut Box<dyn pipe::Sink>,
    cancel: &CancelSignal,
    frame: Bytes,
) -> io::Result<()> {
    let data = seal_frame(session, frame).await?;
    write_down_bytes(sink, cancel, data).await
}

/// Encrypt the frame if the session is encrypted.
async fn seal_frame(session: &Arc<Session>, frame: Bytes) -> io::Result<Bytes> {
    let mut inner = session.inner.lock().await;
    match inner.sealer.as_mut() {
        Some(sealer) => sealer
            .seal(&frame)
            .map(Bytes::from)
            .map_err(|e| io::Error::other(e.to_string())),
        None => Ok(frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session() -> Arc<Session> {
        let registry = Arc::new(XhttpRegistry::new());
        let metrics = crate::metrics::Metrics::new(false).unwrap();
        let memory = registry
            .reserve(SESSION_BASE_RESERVE, 64 * 1024 * 1024, metrics)
            .unwrap();
        let (down_acked, _) = watch::channel(0);
        Arc::new(Session {
            sid: "test-sid".into(),
            owner: "test".into(),
            id: log_utils::IdChain::empty(),
            inner: tokio::sync::Mutex::new(SessionInner {
                up_written: 0,
                up_acked_sent: 0,
                down_sent: 0,
                down_buffer: ReplayBuffer::default(),
                down_pending: None,
                down_buffer_start: 0,
                down_sink: None,
                resume_challenge: None,
                sealer: None,
                connections: 0,
                terminal: false,
                down_eof: false,
                down_fin_acked: false,
                up_eof: false,
                cancel: Arc::new(CancelSignal::new()),
            }),
            down_acked,
            unacked_limit: 1024 * 1024,
            down_ready: tokio::sync::Notify::new(),
            up_written_notify: Arc::new(tokio::sync::Notify::new()),
            peer: Arc::new(PeerSlot {
                sink: Mutex::new(None),
                source: Mutex::new(None),
                ready: tokio::sync::Notify::new(),
            }),
            _memory: memory,
            resume_lock: tokio::sync::Mutex::new(()),
            resume_key: [7; 32],
        })
    }

    #[tokio::test]
    async fn encryption_failure_cannot_return_plaintext() {
        let session = test_session();
        let rng = ring::rand::SystemRandom::new();
        let ephemeral =
            ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::X25519, &rng).unwrap();
        let public = ephemeral.compute_public_key().unwrap();
        let mut hello = [0u8; crate::xhttp_crypto::HELLO_LEN];
        hello[..6].copy_from_slice(&[0, 1, 0, 0, 0, 0]);
        hello[6..38].copy_from_slice(public.as_ref());
        let (sealer, _, _) =
            crate::xhttp_crypto::server_start("test-sid", &[1; 32], &hello).unwrap();
        session.inner.lock().await.sealer = Some(sealer);
        let plain = Bytes::from(vec![0x5a; crate::xhttp_crypto::MAX_RECORD_LEN + 1]);
        assert!(seal_frame(&session, plain).await.is_err());
    }

    #[test]
    fn aggregate_budget_is_released_by_last_reservation_owner() {
        let registry = Arc::new(XhttpRegistry::new());
        let metrics = crate::metrics::Metrics::new(false).unwrap();
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.live_len(), 0);
        let first = registry.reserve(60, 100, metrics.clone()).unwrap();
        let copy = first.clone();
        assert!(registry.reserve(41, 100, metrics.clone()).is_none());
        let second = registry.reserve(40, 100, metrics.clone()).unwrap();
        assert_eq!(metrics.xhttp_reserved_bytes.get(), 100);
        drop(first);
        assert!(registry.reserve(1, 100, metrics.clone()).is_none());
        drop(copy);
        assert_eq!(metrics.xhttp_reserved_bytes.get(), 40);
        drop(second);
        assert_eq!(metrics.xhttp_reserved_bytes.get(), 0);
        assert_eq!(metrics.xhttp_memory_rejections.get(), 2);
        assert!(registry.reserve(usize::MAX, 100, metrics).is_none());
    }

    #[test]
    fn replay_budget_survives_ack_and_last_clone_and_preserves_frame_boundaries() {
        let registry = Arc::new(XhttpRegistry::new());
        let metrics = crate::metrics::Metrics::new(false).unwrap();
        let limit = SESSION_BASE_RESERVE + 2 * (100 + REPLAY_BLOCK_OVERHEAD);
        let memory = registry
            .reserve(SESSION_BASE_RESERVE, limit, metrics.clone())
            .unwrap();
        let mut buffer = ReplayBuffer::default();
        buffer.append(&[1; 100], &memory).unwrap();
        buffer.append(&[2; 100], &memory).unwrap();
        assert_eq!(metrics.xhttp_reserved_bytes.get(), limit as i64);
        assert!(buffer.append(&[3], &memory).is_err());
        let replay = buffer.clone();
        let chunks: Vec<_> = replay.chunks(150).collect();
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [150, 50]);
        assert_eq!(chunks.concat(), [vec![1; 100], vec![2; 100]].concat());
        buffer.acknowledge(100);
        buffer.clear();
        drop(memory);
        assert_eq!(metrics.xhttp_reserved_bytes.get(), limit as i64);
        drop(replay);
        assert_eq!(metrics.xhttp_reserved_bytes.get(), 0);
    }

    #[tokio::test]
    async fn decoder_budget_follows_returned_payload_owner() {
        let registry = Arc::new(XhttpRegistry::new());
        let metrics = crate::metrics::Metrics::new(false).unwrap();
        let memory = registry
            .reserve(SESSION_BASE_RESERVE, 4 * 1024 * 1024, metrics.clone())
            .unwrap();
        let frame = encode_frame(FRAME_DATA, 0, &vec![7; PEER_READ_CHUNK]);
        let mut reader =
            FrameReader::with_memory(Box::new(TestSource(Some(frame))), memory.clone());
        let (_, _, payload) = reader.next_frame().await.unwrap().unwrap();
        assert!(metrics.xhttp_reserved_bytes.get() > SESSION_BASE_RESERVE as i64);
        drop(reader);
        drop(memory);
        assert_eq!(payload.len(), PEER_READ_CHUNK);
        assert!(metrics.xhttp_reserved_bytes.get() > 0);
        drop(payload);
        assert_eq!(metrics.xhttp_reserved_bytes.get(), 0);
    }

    #[tokio::test]
    async fn idle_decoder_returns_its_burst_reservation() {
        let registry = Arc::new(XhttpRegistry::new());
        let metrics = crate::metrics::Metrics::new(false).unwrap();
        let memory = registry
            .reserve(SESSION_BASE_RESERVE, 4 * 1024 * 1024, metrics.clone())
            .unwrap();
        let frame = encode_frame(FRAME_DATA, 0, &vec![7; PEER_READ_CHUNK]);
        let mut reader = FrameReader::with_memory(Box::new(TestSource(Some(frame))), memory);
        drop(reader.next_frame().await.unwrap().unwrap());
        assert!(metrics.xhttp_reserved_bytes.get() > SESSION_BASE_RESERVE as i64);
        assert!(reader.next_frame().await.unwrap().is_none());
        assert_eq!(
            metrics.xhttp_reserved_bytes.get(),
            SESSION_BASE_RESERVE as i64
        );
        drop(reader);
        assert_eq!(metrics.xhttp_reserved_bytes.get(), 0);
    }

    #[tokio::test]
    async fn budget_wait_wakes_when_another_reservation_is_freed() {
        let session = test_session();
        let metrics = session._memory.metrics.clone();
        let blocker = session
            ._memory
            .reserve_more(session._memory.limit - SESSION_BASE_RESERVE)
            .unwrap();
        let (frames, mut received) = mpsc::unbounded_channel();
        let pumping = session.clone();
        let task = tokio::spawn(async move {
            let mut source: Box<dyn pipe::Source> =
                Box::new(TestSource(Some(Bytes::from_static(b"data"))));
            pump_downlink(
                pumping,
                &mut source,
                Box::new(FrameSink {
                    frames,
                    fail: false,
                }),
                &CancelSignal::new(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while metrics.xhttp_memory_rejections.get() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(received.try_recv().is_err());
        drop(blocker);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap()
                .unwrap(),
            encode_frame(FRAME_DATA, 0, b"data")
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap()
                .unwrap(),
            encode_frame(FRAME_FIN, 4, &[])
        );
        acknowledge_final_fin(session.clone(), 4).await;
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(session);
        assert_eq!(metrics.xhttp_reserved_bytes.get(), 0);
    }

    #[tokio::test]
    async fn budget_backpressure_cancel_retains_peer_chunk_for_resume() {
        let session = test_session();
        let registry = session._memory.registry.clone();
        let remaining = session._memory.limit - SESSION_BASE_RESERVE;
        let blocker = session._memory.reserve_more(remaining).unwrap();
        let mut source: Box<dyn pipe::Source> =
            Box::new(TestSource(Some(Bytes::from_static(b"data"))));
        let (frames, _received) = mpsc::unbounded_channel();
        let cancel = CancelSignal::new();
        cancel.raise();
        tokio::time::timeout(
            Duration::from_secs(1),
            pump_downlink(
                session.clone(),
                &mut source,
                Box::new(FrameSink {
                    frames,
                    fail: false,
                }),
                &cancel,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(session.inner.lock().await.down_sent, 0);
        assert_eq!(
            session.inner.lock().await.down_pending.as_deref(),
            Some(b"data".as_slice())
        );
        drop(blocker);
        let (frames, mut received) = mpsc::unbounded_channel();
        let pumping = session.clone();
        let task = tokio::spawn(async move {
            pump_downlink(
                pumping,
                &mut source,
                Box::new(FrameSink {
                    frames,
                    fail: false,
                }),
                &CancelSignal::new(),
            )
            .await
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap()
                .unwrap(),
            encode_frame(FRAME_DATA, 0, b"data")
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap()
                .unwrap(),
            encode_frame(FRAME_FIN, 4, &[])
        );
        acknowledge_final_fin(session.clone(), 4).await;
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(session);
        assert_eq!(
            registry.reserved.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn resume_proof_is_bound_to_sid_offsets_and_challenge() {
        let key: [u8; 32] = std::array::from_fn(|i| i as u8);
        let challenge: [u8; 32] = std::array::from_fn(|i| (i + 32) as u8);
        let mut payload = 7u64.to_be_bytes().to_vec();
        payload.extend_from_slice(&11u64.to_be_bytes());
        let expected = [
            0x68, 0x60, 0x25, 0x9e, 0xcf, 0xa3, 0x7f, 0xd5, 0x77, 0x27, 0xe6, 0xcc, 0xd0, 0xcb,
            0xd9, 0x56, 0xf5, 0x15, 0x39, 0xd0, 0x04, 0x32, 0x33, 0x0f, 0xd1, 0x4f, 0x1f, 0xb9,
            0xa1, 0x8a, 0xc9, 0x8d,
        ];
        payload.extend_from_slice(&expected);
        assert!(verify_resume_proof(&key, "test-sid", &payload, &challenge));
        assert!(!verify_resume_proof(
            &key,
            "other-sid",
            &payload,
            &challenge
        ));
        let mut other = challenge;
        other[0] ^= 1;
        assert!(!verify_resume_proof(&key, "test-sid", &payload, &other));
        payload[0] ^= 1;
        assert!(!verify_resume_proof(&key, "test-sid", &payload, &challenge));
        assert!(!verify_resume_proof(
            &key,
            "test-sid",
            &payload[..16],
            &challenge
        ));
    }

    #[tokio::test]
    async fn malformed_resume_cannot_discard_buffered_data() {
        let session = test_session();
        {
            let mut inner = session.inner.lock().await;
            inner.up_written = 8;
            inner.down_sent = 12;
            inner.down_buffer_start = 4;
            inner
                .down_buffer
                .append(b"abcdefgh", &session._memory)
                .unwrap();
        }
        for (up, down) in [(9u64, 8u64), (8, 13), (8, 3)] {
            let mut payload = up.to_be_bytes().to_vec();
            payload.extend_from_slice(&down.to_be_bytes());
            assert!(apply_resume(&session, &Bytes::from(payload)).await.is_err());
            assert_eq!(
                session.inner.lock().await.down_buffer.to_vec().as_slice(),
                b"abcdefgh"
            );
        }
        assert!(apply_resume(&session, &Bytes::from_static(b"short"))
            .await
            .is_err());
        let mut valid = 8u64.to_be_bytes().to_vec();
        valid.extend_from_slice(&8u64.to_be_bytes());
        apply_resume(&session, &Bytes::from(valid)).await.unwrap();
        assert_eq!(
            session.inner.lock().await.down_buffer.to_vec().as_slice(),
            b"efgh"
        );
        assert_eq!(*session.down_acked.borrow(), 8);
    }

    struct TestSource(Option<Bytes>);
    #[async_trait::async_trait]
    impl pipe::Source for TestSource {
        fn id(&self) -> log_utils::IdChain<u64> {
            log_utils::IdChain::empty()
        }
        async fn read(&mut self) -> io::Result<pipe::Data> {
            Ok(self.0.take().map_or(pipe::Data::Eof, pipe::Data::Chunk))
        }
        fn consume(&mut self, _: usize) -> io::Result<()> {
            Ok(())
        }
    }

    struct PartialSink {
        received: Arc<Mutex<Vec<u8>>>,
        cancel: Option<Arc<CancelSignal>>,
    }

    struct FrameSink {
        frames: mpsc::UnboundedSender<Bytes>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl pipe::Sink for FrameSink {
        fn id(&self) -> log_utils::IdChain<u64> {
            log_utils::IdChain::empty()
        }
        fn write(&mut self, data: Bytes) -> io::Result<Bytes> {
            if self.fail {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "lost transport"));
            }
            self.frames.send(data).unwrap();
            Ok(Bytes::new())
        }
        fn eof(&mut self) -> io::Result<()> {
            Ok(())
        }
        async fn wait_writable(&mut self) -> io::Result<()> {
            unreachable!("FrameSink accepts complete writes")
        }
    }

    fn resumable_session() -> Arc<Session> {
        let mut session = test_session();
        Arc::get_mut(&mut session).unwrap().resume_key = [7; 32];
        session
    }

    async fn complete_final_ack(session: Arc<Session>) {
        let (frames, mut received) = mpsc::unbounded_channel();
        let down_session = session.clone();
        let down = tokio::spawn(async move {
            let mut source: Box<dyn pipe::Source> = Box::new(TestSource(None));
            pump_downlink(
                down_session,
                &mut source,
                Box::new(FrameSink {
                    frames,
                    fail: false,
                }),
                &CancelSignal::new(),
            )
            .await
        });
        let mut decoder = FrameDecoder::new();
        let fin_offset = loop {
            let bytes = tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap()
                .unwrap();
            decoder.push(&bytes);
            match decoder.next_frame() {
                DecodedFrame::Frame {
                    kind: FRAME_FIN,
                    offset,
                    payload,
                } => {
                    assert!(payload.is_empty());
                    break offset;
                }
                DecodedFrame::Frame {
                    kind: FRAME_DATA, ..
                } => (),
                other => panic!("unexpected frame: {other:?}"),
            }
        };
        assert_eq!(fin_offset, session.inner.lock().await.down_sent);
        assert!(!session.inner.lock().await.terminal);
        acknowledge_final_fin(session.clone(), fin_offset).await;
        tokio::time::timeout(Duration::from_secs(1), down)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let inner = session.inner.lock().await;
        assert!(inner.terminal);
        assert!(inner.down_fin_acked);
        assert!(inner.down_buffer.is_empty());
    }

    async fn acknowledge_final_fin(session: Arc<Session>, fin_offset: u64) {
        let mut reader = FrameReader::new(Box::new(TestSource(Some(encode_frame(
            FRAME_ACK,
            fin_offset,
            &[FRAME_FIN],
        )))));
        let mut sink: Box<dyn pipe::Sink> = Box::new(PartialSink {
            received: Arc::new(Mutex::new(Vec::new())),
            cancel: None,
        });
        pump_uplink(
            session.clone(),
            &mut reader,
            &mut sink,
            &CancelSignal::new(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn final_ack_completes_normal_eof() {
        complete_final_ack(resumable_session()).await;
    }

    #[tokio::test]
    async fn lost_fin_is_repeated_on_replacement_transport() {
        let session = resumable_session();
        let (frames, _received) = mpsc::unbounded_channel();
        let mut source: Box<dyn pipe::Source> = Box::new(TestSource(None));
        assert!(pump_downlink(
            session.clone(),
            &mut source,
            Box::new(FrameSink { frames, fail: true }),
            &CancelSignal::new()
        )
        .await
        .is_err());
        assert!(session.inner.lock().await.down_eof);
        assert!(!session.inner.lock().await.terminal);
        complete_final_ack(session).await;
    }

    #[tokio::test]
    async fn lost_final_ack_keeps_replay_data_until_resumed() {
        let session = resumable_session();
        {
            let mut inner = session.inner.lock().await;
            inner.down_sent = 7;
            inner
                .down_buffer
                .append(b"payload", &session._memory)
                .unwrap();
            inner.down_eof = true;
        }
        let (frames, mut received) = mpsc::unbounded_channel();
        let mut source: Box<dyn pipe::Source> = Box::new(TestSource(None));
        tokio::time::timeout(
            Duration::from_secs(12),
            pump_downlink(
                session.clone(),
                &mut source,
                Box::new(FrameSink {
                    frames,
                    fail: false,
                }),
                &CancelSignal::new(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            received.try_recv().unwrap(),
            encode_frame(FRAME_DATA, 0, b"payload")
        );
        assert_eq!(
            received.try_recv().unwrap(),
            encode_frame(FRAME_FIN, 7, &[])
        );
        assert!(!session.inner.lock().await.terminal);
        assert_eq!(session.inner.lock().await.down_buffer.to_vec(), b"payload");
        complete_final_ack(session).await;
    }
    #[async_trait::async_trait]
    impl pipe::Sink for PartialSink {
        fn id(&self) -> log_utils::IdChain<u64> {
            log_utils::IdChain::empty()
        }
        fn write(&mut self, data: Bytes) -> io::Result<Bytes> {
            let accepted = if self.cancel.is_some() {
                data.len().min(3)
            } else {
                data.len()
            };
            self.received
                .lock()
                .unwrap()
                .extend_from_slice(&data[..accepted]);
            if let Some(cancel) = self.cancel.take() {
                cancel.raise();
            }
            Ok(data.slice(accepted..))
        }
        fn eof(&mut self) -> io::Result<()> {
            Ok(())
        }
        async fn wait_writable(&mut self) -> io::Result<()> {
            std::future::pending().await
        }
    }

    #[test]
    fn unknown_and_reserved_frame_types_are_rejected() {
        for kind in 0u8..=255 {
            let mut decoder = FrameDecoder::new();
            decoder.push(&encode_frame(kind, 0, &[]));
            let result = decoder.next_frame();
            if (FRAME_OPEN..=FRAME_RESUME_TOKEN).contains(&kind) {
                assert!(matches!(result, DecodedFrame::Frame { .. }));
            } else {
                assert!(matches!(result, DecodedFrame::Invalid(_)));
                assert!(matches!(decoder.next_frame(), DecodedFrame::Invalid(_)));
            }
        }
    }

    #[tokio::test]
    async fn upload_cancel_and_replay_preserve_exactly_once_bytes() {
        let session = test_session();
        let cancel = Arc::new(CancelSignal::new());
        let received = Arc::new(Mutex::new(Vec::new()));
        let frame = encode_frame(FRAME_DATA, 0, b"abcdef");
        let mut reader = FrameReader::new(Box::new(TestSource(Some(frame.clone()))));
        let mut sink: Box<dyn pipe::Sink> = Box::new(PartialSink {
            received: received.clone(),
            cancel: Some(cancel.clone()),
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            pump_uplink(session.clone(), &mut reader, &mut sink, &cancel),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(session.inner.lock().await.up_written, 3);
        assert_eq!(received.lock().unwrap().as_slice(), b"abc");
        let mut reader = FrameReader::new(Box::new(TestSource(Some(frame))));
        let mut sink: Box<dyn pipe::Sink> = Box::new(PartialSink {
            received: received.clone(),
            cancel: None,
        });
        pump_uplink(
            session.clone(),
            &mut reader,
            &mut sink,
            &CancelSignal::new(),
        )
        .await
        .unwrap();
        assert_eq!(session.inner.lock().await.up_written, 6);
        assert_eq!(received.lock().unwrap().as_slice(), b"abcdef");
    }

    #[tokio::test]
    async fn truncated_body_is_an_error() {
        let frame = encode_frame(FRAME_DATA, 0, b"abc");
        let mut reader =
            FrameReader::new(Box::new(TestSource(Some(frame.slice(..frame.len() - 1)))));
        assert_eq!(
            reader.next_frame().await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn frame_roundtrip_short_payload() {
        let payload = b"hello";
        let encoded = encode_frame(FRAME_DATA, 42, payload);
        assert_eq!(encoded.len(), FRAME_HEADER_LEN + payload.len());

        let mut decoder = FrameDecoder::new();
        decoder.push(&encoded);
        match decoder.next_frame() {
            DecodedFrame::Frame {
                kind,
                offset,
                payload: decoded,
            } => {
                assert_eq!(kind, FRAME_DATA);
                assert_eq!(offset, 42);
                assert_eq!(&decoded[..], payload);
            }
            x => panic!("unexpected result: {x:?}"),
        }
    }

    #[test]
    fn frame_wire_format() {
        // The wire format is shared with the client, so the bytes are pinned here.
        let encoded = encode_frame(FRAME_DATA, 0x0102_0304_0506_0708, b"hi");
        let expected: Vec<u8> = vec![
            0x02, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x00, 0x02, 0x68, 0x69,
        ];
        assert_eq!(&encoded[..], &expected[..]);
    }

    #[test]
    fn frame_wire_format_long_length() {
        let payload = vec![0u8; 0x8000];
        let encoded = encode_frame(FRAME_DATA, 0, &payload);
        let expected: Vec<u8> = vec![
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80,
            0x00,
        ];
        assert_eq!(&encoded[..expected.len()], &expected[..]);
        assert_eq!(encoded.len(), expected.len() + payload.len());
    }

    #[test]
    fn frame_roundtrip_empty_payload() {
        let encoded = encode_frame(FRAME_FIN, 0, &[]);
        assert_eq!(encoded.len(), FRAME_HEADER_LEN);

        let mut decoder = FrameDecoder::new();
        decoder.push(&encoded);
        match decoder.next_frame() {
            DecodedFrame::Frame {
                kind,
                offset,
                payload,
            } => {
                assert_eq!(kind, FRAME_FIN);
                assert_eq!(offset, 0);
                assert!(payload.is_empty());
            }
            x => panic!("unexpected result: {x:?}"),
        }
    }

    #[test]
    fn frame_roundtrip_long_payload() {
        let payload = vec![0xabu8; 0x8000];
        let encoded = encode_frame(FRAME_DATA, 0, &payload);
        let mut decoder = FrameDecoder::new();
        decoder.push(&encoded);
        match decoder.next_frame() {
            DecodedFrame::Frame {
                payload: decoded, ..
            } => {
                assert_eq!(decoded.len(), payload.len());
                assert_eq!(&decoded[..], &payload[..]);
            }
            x => panic!("unexpected result: {x:?}"),
        }
    }

    #[test]
    fn frame_decoding_is_incremental() {
        let encoded = encode_frame(FRAME_DATA, 7, b"0123456789");
        let mut decoder = FrameDecoder::new();

        for byte in &encoded[..encoded.len() - 1] {
            decoder.push(&[*byte]);
            assert_eq!(decoder.next_frame(), DecodedFrame::Incomplete);
        }

        decoder.push(&encoded[encoded.len() - 1..]);
        match decoder.next_frame() {
            DecodedFrame::Frame {
                kind,
                offset,
                payload,
            } => {
                assert_eq!(kind, FRAME_DATA);
                assert_eq!(offset, 7);
                assert_eq!(&payload[..], b"0123456789");
            }
            x => panic!("unexpected result: {x:?}"),
        }
    }

    #[test]
    fn frame_decoding_of_two_frames() {
        let mut buffer = encode_frame(FRAME_OPEN, 0, b"example.com:443").to_vec();
        buffer.extend_from_slice(&encode_frame(FRAME_DATA, 0, b"data"));

        let mut decoder = FrameDecoder::new();
        decoder.push(&buffer);

        match decoder.next_frame() {
            DecodedFrame::Frame {
                kind,
                offset,
                payload,
            } => {
                assert_eq!(kind, FRAME_OPEN);
                assert_eq!(offset, 0);
                assert_eq!(&payload[..], b"example.com:443");
            }
            x => panic!("unexpected result: {x:?}"),
        }

        match decoder.next_frame() {
            DecodedFrame::Frame {
                kind,
                offset,
                payload,
            } => {
                assert_eq!(kind, FRAME_DATA);
                assert_eq!(offset, 0);
                assert_eq!(&payload[..], b"data");
            }
            x => panic!("unexpected result: {x:?}"),
        }

        assert_eq!(decoder.next_frame(), DecodedFrame::Incomplete);
    }

    #[test]
    fn frame_decoding_of_too_long_frame() {
        let header: Vec<u8> = vec![
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x7f, 0xff, 0xff,
            0x7f,
        ];
        let mut decoder = FrameDecoder::new();
        decoder.push(&header);
        assert!(matches!(decoder.next_frame(), DecodedFrame::Invalid(_)));

        decoder.push(&vec![0u8; MAX_BUFFERED_FRAME]);
        assert_eq!(
            decoder.next_frame(),
            DecodedFrame::Invalid("Frame is too large")
        );
    }

    #[test]
    fn frame_decoding_of_a_truncated_frame() {
        let encoded = encode_frame(FRAME_DATA, 0, b"0123456789");
        let mut decoder = FrameDecoder::new();
        decoder.push(&encoded[..encoded.len() - 3]);
        assert_eq!(decoder.next_frame(), DecodedFrame::Incomplete);
    }

    #[test]
    fn destination_parsing() {
        match parse_destination(&Bytes::from_static(b"example.com:443")) {
            Ok(TcpDestination::HostName((host, port))) => {
                assert_eq!(host, "example.com");
                assert_eq!(port, 443);
            }
            x => panic!("unexpected result: {x:?}"),
        }

        match parse_destination(&Bytes::from_static(b"127.0.0.1:8080")) {
            Ok(TcpDestination::Address(addr)) => {
                assert_eq!(addr.to_string(), "127.0.0.1:8080");
            }
            x => panic!("unexpected result: {x:?}"),
        }

        match parse_destination(&Bytes::from_static(b"[::1]:8080")) {
            Ok(TcpDestination::Address(addr)) => {
                assert_eq!(addr.to_string(), "[::1]:8080");
            }
            x => panic!("unexpected result: {x:?}"),
        }

        assert!(parse_destination(&Bytes::from_static(b"example.com")).is_err());
        assert!(parse_destination(&Bytes::from_static(b"not a host:80")).is_err());
    }

    #[test]
    fn sid_parsing() {
        assert_eq!(
            parse_sid("/api/v2/abcDEF123", Some("/api/v2/")),
            Some("abcDEF123".to_string())
        );
        assert_eq!(parse_sid("/api/v2/", Some("/api/v2/")), None);
        assert_eq!(parse_sid("/other/abc", Some("/api/v2/")), None);
        assert_eq!(parse_sid("/api/v2/a/b", Some("/api/v2/")), None);
        assert_eq!(parse_sid("/api/v2/abc", None), None);
        assert_eq!(parse_sid("/api/v2/a b", Some("/api/v2/")), None);
    }
}
