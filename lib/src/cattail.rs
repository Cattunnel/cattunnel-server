//! CatTail: an outer layer under the TLS of the client (docs/cattail.md).
//!
//! The client wraps the bytes of its TLS connection into AEAD frames with
//! random padding, so the wire carries no TLS record headers from the first
//! byte on. The endpoint checks the first frame (the admission), stays silent
//! on any failure, and hands the inner bytes to the same TLS acceptor as its
//! main TCP listener.
//!
//! The AEAD (AES-128-GCM, a direction byte and a 64-bit counter in the nonce)
//! and the HKDF are the ones of NineLives suite 1, see [crate::xhttp_crypto].

use crate::core::{Context, Core};
use crate::{log_id, log_utils, net_utils, xhttp_crypto};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use ring::{aead, hkdf};
use socket2::{Domain, Protocol as SockProtocol, SockRef, Socket, Type};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

/// The length of the endpoint key.
pub const KEY_LEN: usize = 32;
/// The length of the client and of the server nonce.
pub const NONCE_LEN: usize = 24;
/// The length of the plaintext header.
pub const HEADER_LEN: usize = 32;
/// The length of the AEAD tag.
pub const TAG_LEN: usize = xhttp_crypto::TAG_LEN;
/// The length of the sealed header.
pub const SEALED_HEADER_LEN: usize = HEADER_LEN + TAG_LEN;
/// The bytes the endpoint needs for the admission: the nonce and the header.
pub const FIRST_SEGMENT_LEN: usize = NONCE_LEN + SEALED_HEADER_LEN;
/// The maximum length of the body of a frame.
pub const MAX_BODY_LEN: usize = 16384;
/// The maximum wire size of a frame without padding.
pub const MAX_FRAME_LEN: usize = SEALED_HEADER_LEN + MAX_BODY_LEN + TAG_LEN;
/// The type of the first client frame.
pub const TYPE_OPEN: u8 = 0x01;
/// The type of every other frame.
pub const TYPE_DATA: u8 = 0x02;
/// The version of the protocol.
pub const VERSION: u8 = 0x01;
/// The direction byte of the AEAD nonce, client to server.
pub const DIRECTION_C2S: u8 = 0x01;
/// The direction byte of the AEAD nonce, server to client.
pub const DIRECTION_S2C: u8 = 0x02;
/// The accepted distance between the clocks, seconds.
pub const TIMESTAMP_WINDOW_SECS: u64 = 120;
/// The counters remembered per identity.
pub const REPLAY_WINDOW: u64 = 1024;
/// The identities remembered by the endpoint.
pub const REPLAY_IDENTITIES: usize = 16384;
/// The silence on a failed admission lasts from this...
pub const SILENCE_MIN: Duration = Duration::from_secs(10);
/// ... to this, counted from the TCP accept.
pub const SILENCE_MAX: Duration = Duration::from_secs(60);
/// After the FIN of a silent connection the input is discarded this long.
const FIN_DRAIN: Duration = Duration::from_secs(5);
/// At most this many connections are kept silent at a time: the others get
/// the FIN at once, so a flood cannot use up the file descriptors.
const MAX_SILENT_CONNECTIONS: usize = 4096;
/// The buffer of the in-process pipe to the TLS acceptor, each direction.
const DUPLEX_LEN: usize = 64 * 1024;
/// The reservation in the memory budget of NineLives for one admitted
/// connection: both pipe directions and a frame in each direction.
const CONNECTION_RESERVE: usize = 2 * DUPLEX_LEN + 2 * (MAX_FRAME_LEN + 1024);
/// The printable range of the nonce prefix.
const PRINTABLE: std::ops::RangeInclusive<u8> = 0x21..=0x7e;

const INFO_C2S: &[u8] = b"cattail-v1 c2s";
const INFO_S2C: &[u8] = b"cattail-v1 s2c";

/// The default padding scheme of the endpoint, docs/cattail.md 4.1.
pub const DEFAULT_PATTERN: &str =
    "stop=3\n0=400-1400,c,200-900\n1=300-1200,c,100-700\n2=100-600,c\nmid=0-32\nend=0-96";

/// An error of the CatTail codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The AEAD tag does not match.
    Decrypt,
    /// The seal counter is exhausted.
    Counter,
    /// The frame breaks the format.
    Frame(&'static str),
    /// The padding scheme string is not valid.
    Pattern(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Decrypt => write!(f, "the CatTail frame cannot be decrypted"),
            Error::Counter => write!(f, "the CatTail seal counter is exhausted"),
            Error::Frame(x) => write!(f, "invalid CatTail frame: {x}"),
            Error::Pattern(x) => write!(f, "invalid CatTail padding scheme: {x}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

fn derive(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; xhttp_crypto::AEAD_KEY_LEN] {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let info = [info];
    let okm = prk
        .expand(&info, xhttp_crypto::KeyLen(xhttp_crypto::AEAD_KEY_LEN))
        .expect("16 bytes is a valid HKDF-SHA256 output");
    let mut key = [0u8; xhttp_crypto::AEAD_KEY_LEN];
    okm.fill(&mut key)
        .expect("the output has the requested length");
    key
}

/// The key of the client to server direction.
pub fn key_c2s(key: &[u8; KEY_LEN], client_nonce: &[u8; NONCE_LEN]) -> [u8; 16] {
    derive(client_nonce, key, INFO_C2S)
}

/// The key of the server to client direction.
pub fn key_s2c(
    key: &[u8; KEY_LEN],
    client_nonce: &[u8; NONCE_LEN],
    server_nonce: &[u8; NONCE_LEN],
) -> [u8; 16] {
    let mut salt = [0u8; 2 * NONCE_LEN];
    salt[..NONCE_LEN].copy_from_slice(client_nonce);
    salt[NONCE_LEN..].copy_from_slice(server_nonce);
    derive(&salt, key, INFO_S2C)
}

/// Decode the base64 form of the endpoint key.
pub fn decode_key(encoded: &str) -> Option<[u8; KEY_LEN]> {
    xhttp_crypto::decode_seed(encoded.trim())
}

/// The plaintext header of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub frame_type: u8,
    pub version: u8,
    pub timestamp: u64,
    pub identity: [u8; 8],
    /// 48 bits on the wire.
    pub counter: u64,
    pub body_len: u16,
    pub mid_len: u16,
    pub end_len: u16,
}

impl Header {
    /// The header of the first client frame.
    pub fn open(timestamp: u64, identity: [u8; 8], counter: u64) -> Self {
        Self {
            frame_type: TYPE_OPEN,
            version: VERSION,
            timestamp,
            identity,
            counter,
            body_len: 0,
            mid_len: 0,
            end_len: 0,
        }
    }

    /// The header of every other frame.
    pub fn data() -> Self {
        Self {
            frame_type: TYPE_DATA,
            ..Self::open(0, [0; 8], 0)
        }
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0] = self.frame_type;
        b[1] = self.version;
        b[2..10].copy_from_slice(&self.timestamp.to_be_bytes());
        b[10..18].copy_from_slice(&self.identity);
        b[18..24].copy_from_slice(&(self.counter & 0xffff_ffff_ffff).to_be_bytes()[2..]);
        b[24..26].copy_from_slice(&self.body_len.to_be_bytes());
        b[26..28].copy_from_slice(&self.mid_len.to_be_bytes());
        b[28..30].copy_from_slice(&self.end_len.to_be_bytes());
        b
    }

    pub fn decode(b: &[u8; HEADER_LEN]) -> Self {
        let mut counter = [0u8; 8];
        counter[2..].copy_from_slice(&b[18..24]);
        Self {
            frame_type: b[0],
            version: b[1],
            timestamp: u64::from_be_bytes(b[2..10].try_into().unwrap()),
            identity: b[10..18].try_into().unwrap(),
            counter: u64::from_be_bytes(counter),
            body_len: u16::from_be_bytes([b[24], b[25]]),
            mid_len: u16::from_be_bytes([b[26], b[27]]),
            end_len: u16::from_be_bytes([b[28], b[29]]),
        }
    }

    /// Check the type, the version and the body length.
    pub fn check(&self, expected_type: u8) -> Result<(), Error> {
        if self.frame_type != expected_type {
            return Err(Error::Frame("unexpected type"));
        }
        if self.version != VERSION {
            return Err(Error::Frame("unknown version"));
        }
        if self.body_len as usize > MAX_BODY_LEN {
            return Err(Error::Frame("body too long"));
        }
        Ok(())
    }
}

fn aead_key(key: &[u8; 16]) -> aead::LessSafeKey {
    let unbound = aead::UnboundKey::new(&aead::AES_128_GCM, key).expect("16 bytes AES key");
    aead::LessSafeKey::new(unbound)
}

/// The sealing side of one direction.
pub struct Sealer {
    key: aead::LessSafeKey,
    direction: u8,
    counter: u64,
}

impl Sealer {
    pub fn new(key: &[u8; 16], direction: u8) -> Self {
        Self {
            key: aead_key(key),
            direction,
            counter: 0,
        }
    }

    fn seal_into(&mut self, plain: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        if self.counter == u64::MAX {
            return Err(Error::Counter);
        }
        let nonce = aead::Nonce::assume_unique_for_key(xhttp_crypto::make_nonce(
            self.direction,
            self.counter,
        ));
        self.counter += 1;
        let start = out.len();
        out.extend_from_slice(plain);
        let tag = self
            .key
            .seal_in_place_separate_tag(nonce, aead::Aad::empty(), &mut out[start..])
            .map_err(|_| Error::Counter)?;
        out.extend_from_slice(tag.as_ref());
        Ok(())
    }

    /// Append one frame to `out`. The lengths of the header are taken from
    /// the arguments.
    pub fn frame_into(
        &mut self,
        mut header: Header,
        body: &[u8],
        mid: &[u8],
        end: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        if body.len() > MAX_BODY_LEN
            || mid.len() > u16::MAX as usize
            || end.len() > u16::MAX as usize
        {
            return Err(Error::Frame("frame too long"));
        }
        header.body_len = body.len() as u16;
        header.mid_len = mid.len() as u16;
        header.end_len = end.len() as u16;
        self.seal_into(&header.encode(), out)?;
        out.extend_from_slice(mid);
        if !body.is_empty() {
            self.seal_into(body, out)?;
        }
        out.extend_from_slice(end);
        Ok(())
    }
}

/// The opening side of one direction. The counter moves only after a
/// successful open.
pub struct Opener {
    key: aead::LessSafeKey,
    direction: u8,
    counter: u64,
}

impl Opener {
    pub fn new(key: &[u8; 16], direction: u8) -> Self {
        Self {
            key: aead_key(key),
            direction,
            counter: 0,
        }
    }

    fn open_in_place<'a>(&mut self, data: &'a mut [u8]) -> Result<&'a [u8], Error> {
        if self.counter == u64::MAX {
            return Err(Error::Counter);
        }
        let nonce = aead::Nonce::assume_unique_for_key(xhttp_crypto::make_nonce(
            self.direction,
            self.counter,
        ));
        let plain = self
            .key
            .open_in_place(nonce, aead::Aad::empty(), data)
            .map_err(|_| Error::Decrypt)?;
        self.counter += 1;
        Ok(plain)
    }

    pub fn open_header(&mut self, sealed: &[u8; SEALED_HEADER_LEN]) -> Result<Header, Error> {
        let mut buf = *sealed;
        let plain = self.open_in_place(&mut buf)?;
        Ok(Header::decode(plain.try_into().expect("32 bytes")))
    }

    pub fn open_body(&mut self, mut sealed: Vec<u8>) -> Result<Vec<u8>, Error> {
        let len = self.open_in_place(&mut sealed)?.len();
        sealed.truncate(len);
        Ok(sealed)
    }
}

/// Fill `buf`; `Ok(false)` when the stream ends before the first byte.
async fn read_exact_or_eof<R: AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut [u8],
) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..]).await?;
        if n == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        filled += n;
    }
    Ok(true)
}

async fn skip<R: AsyncRead + Unpin>(reader: &mut R, mut len: usize) -> io::Result<()> {
    let mut scratch = [0u8; 1024];
    while len > 0 {
        let n = len.min(scratch.len());
        reader.read_exact(&mut scratch[..n]).await?;
        len -= n;
    }
    Ok(())
}

/// Read the padding and the body of a frame whose header is opened.
pub async fn read_frame_rest<R: AsyncRead + Unpin>(
    reader: &mut R,
    opener: &mut Opener,
    header: &Header,
) -> io::Result<Vec<u8>> {
    skip(reader, header.mid_len as usize).await?;
    let body = if header.body_len > 0 {
        let mut sealed = vec![0u8; header.body_len as usize + TAG_LEN];
        reader.read_exact(&mut sealed).await?;
        opener.open_body(sealed)?
    } else {
        Vec::new()
    };
    skip(reader, header.end_len as usize).await?;
    Ok(body)
}

/// Read one DATA frame; `Ok(None)` on a FIN at the frame boundary.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    opener: &mut Opener,
) -> io::Result<Option<Vec<u8>>> {
    let mut sealed = [0u8; SEALED_HEADER_LEN];
    if !read_exact_or_eof(reader, &mut sealed).await? {
        return Ok(None);
    }
    let header = opener.open_header(&sealed)?;
    header.check(TYPE_DATA)?;
    read_frame_rest(reader, opener, &header).await.map(Some)
}

/// An item of a line of the padding scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// A frame of a wire size drawn from the range.
    Size(u16, u16),
    /// Stop cutting this write if no data is left.
    Check,
}

/// The padding scheme, docs/cattail.md section 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddingScheme {
    pub stop: usize,
    pub lines: Vec<Option<Vec<Item>>>,
    pub mid: (u16, u16),
    pub end: (u16, u16),
    pub nonce: (u8, u8),
}

/// The plan of one frame: how many bytes of data it carries and its padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePlan {
    pub body: usize,
    pub mid: u16,
    pub end: u16,
}

const MAX_STOP: usize = 16;
const MAX_ITEMS: usize = 16;
const MAX_STEADY_PADDING: u32 = 4096;

fn parse_range(value: &str, min: u32, max: u32) -> Result<(u32, u32), Error> {
    let bad = || Error::Pattern(format!("bad range {value:?}"));
    let (lo, hi) = match value.split_once('-') {
        Some((lo, hi)) => (lo.trim(), hi.trim()),
        None => (value, value),
    };
    let lo: u32 = lo.parse().map_err(|_| bad())?;
    let hi: u32 = hi.parse().map_err(|_| bad())?;
    if lo > hi || lo < min || hi > max {
        return Err(bad());
    }
    Ok((lo, hi))
}

impl Default for PaddingScheme {
    fn default() -> Self {
        Self::parse(DEFAULT_PATTERN).expect("the default scheme is valid")
    }
}

impl PaddingScheme {
    /// Parse a scheme string. Unknown keys are ignored.
    pub fn parse(pattern: &str) -> Result<Self, Error> {
        let mut scheme = Self {
            stop: 0,
            lines: vec![None; MAX_STOP],
            mid: (0, 0),
            end: (0, 0),
            nonce: (0, 0),
        };
        for line in pattern.split(['\n', ';']) {
            let line = line.replace('\r', "");
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| Error::Pattern(format!("no '=' in {line:?}")))?;
            let (key, value) = (key.trim(), value.trim());
            match key {
                "stop" => {
                    let (stop, _) = parse_range(value, 0, MAX_STOP as u32)
                        .ok()
                        .filter(|(lo, hi)| lo == hi)
                        .ok_or_else(|| Error::Pattern(format!("bad stop {value:?}")))?;
                    scheme.stop = stop as usize;
                }
                "mid" | "end" => {
                    let (lo, hi) = parse_range(value, 0, MAX_STEADY_PADDING)?;
                    let range = (lo as u16, hi as u16);
                    if key == "mid" {
                        scheme.mid = range;
                    } else {
                        scheme.end = range;
                    }
                }
                "nonce" => {
                    let (lo, hi) = parse_range(value, 0, NONCE_LEN as u32)?;
                    scheme.nonce = (lo as u8, hi as u8);
                }
                _ if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) => {
                    let index: usize = key
                        .parse()
                        .ok()
                        .filter(|i| *i < MAX_STOP)
                        .ok_or_else(|| Error::Pattern(format!("bad line number {key:?}")))?;
                    let mut items = Vec::new();
                    for item in value.split(',') {
                        let item = item.trim();
                        if item == "c" {
                            items.push(Item::Check);
                        } else {
                            let (lo, hi) =
                                parse_range(item, SEALED_HEADER_LEN as u32, MAX_FRAME_LEN as u32)?;
                            items.push(Item::Size(lo as u16, hi as u16));
                        }
                    }
                    if items.len() > MAX_ITEMS {
                        return Err(Error::Pattern(format!("too many items in line {index}")));
                    }
                    scheme.lines[index] = Some(items);
                }
                _ => {}
            }
        }
        Ok(scheme)
    }

    /// Cut write number `write_index` of `data_len` bytes into frames.
    pub fn plan<R: Rng>(&self, write_index: u64, data_len: usize, rng: &mut R) -> Vec<FramePlan> {
        let mut left = data_len;
        let mut plans = Vec::new();
        let shaped = (write_index < self.stop as u64)
            .then(|| self.lines[write_index as usize].as_ref())
            .flatten();
        for item in shaped.into_iter().flatten() {
            match *item {
                Item::Check => {
                    if left == 0 {
                        break;
                    }
                }
                Item::Size(lo, hi) => {
                    let size = rng.gen_range(lo..=hi) as usize;
                    let (body, padding) = if left > 0 && size > SEALED_HEADER_LEN + TAG_LEN {
                        let body = left
                            .min(size - SEALED_HEADER_LEN - TAG_LEN)
                            .min(MAX_BODY_LEN);
                        (body, size - SEALED_HEADER_LEN - TAG_LEN - body)
                    } else {
                        (0, size - SEALED_HEADER_LEN)
                    };
                    left -= body;
                    let mid = rng.gen_range(0..=padding);
                    plans.push(FramePlan {
                        body,
                        mid: mid as u16,
                        end: (padding - mid) as u16,
                    });
                }
            }
        }
        while left > 0 {
            let body = left.min(MAX_BODY_LEN);
            left -= body;
            plans.push(FramePlan {
                body,
                mid: rng.gen_range(self.mid.0..=self.mid.1),
                end: rng.gen_range(self.end.0..=self.end.1),
            });
        }
        plans
    }

    /// A fresh nonce of the sender with the printable prefix of the scheme.
    pub fn make_nonce<R: Rng>(&self, rng: &mut R) -> [u8; NONCE_LEN] {
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill(&mut nonce[..]);
        let printable = rng.gen_range(self.nonce.0..=self.nonce.1) as usize;
        for b in &mut nonce[..printable] {
            *b = rng.gen_range(PRINTABLE);
        }
        nonce
    }
}

fn random_bytes<R: Rng>(rng: &mut R, len: u16) -> Vec<u8> {
    let mut v = vec![0u8; len as usize];
    rng.fill(&mut v[..]);
    v
}

/// The sending side of one direction: the padding scheme over a sealer.
pub struct Encoder {
    sealer: Sealer,
    scheme: PaddingScheme,
    writes: u64,
    prefix: Vec<u8>,
    open: Option<Header>,
}

impl Encoder {
    pub fn new(
        sealer: Sealer,
        scheme: PaddingScheme,
        prefix: Vec<u8>,
        open: Option<Header>,
    ) -> Self {
        Self {
            sealer,
            scheme,
            writes: 0,
            prefix,
            open,
        }
    }

    /// The client side: returns the encoder and the client nonce.
    pub fn client<R: Rng>(
        key: &[u8; KEY_LEN],
        scheme: PaddingScheme,
        open: Header,
        rng: &mut R,
    ) -> (Self, [u8; NONCE_LEN]) {
        let nonce = scheme.make_nonce(rng);
        let sealer = Sealer::new(&key_c2s(key, &nonce), DIRECTION_C2S);
        (Self::new(sealer, scheme, nonce.to_vec(), Some(open)), nonce)
    }

    /// The server side: returns the encoder and the server nonce.
    pub fn server<R: Rng>(
        key: &[u8; KEY_LEN],
        client_nonce: &[u8; NONCE_LEN],
        scheme: PaddingScheme,
        rng: &mut R,
    ) -> (Self, [u8; NONCE_LEN]) {
        let nonce = scheme.make_nonce(rng);
        let sealer = Sealer::new(&key_s2c(key, client_nonce, &nonce), DIRECTION_S2C);
        (Self::new(sealer, scheme, nonce.to_vec(), None), nonce)
    }

    /// The wire bytes of one write of inner data.
    pub fn encode<R: Rng>(&mut self, data: &[u8], rng: &mut R) -> Result<Vec<u8>, Error> {
        let plans = self.scheme.plan(self.writes, data.len(), rng);
        self.writes = self.writes.saturating_add(1);
        let mut out = std::mem::take(&mut self.prefix);
        let mut offset = 0;
        for plan in plans {
            let header = self.open.take().unwrap_or_else(Header::data);
            let mid = random_bytes(rng, plan.mid);
            let end = random_bytes(rng, plan.end);
            self.sealer.frame_into(
                header,
                &data[offset..offset + plan.body],
                &mid,
                &end,
                &mut out,
            )?;
            offset += plan.body;
        }
        Ok(out)
    }
}

/// The time check of the admission.
pub fn timestamp_ok(now: u64, start: u64, timestamp: u64) -> bool {
    now.abs_diff(timestamp) <= TIMESTAMP_WINDOW_SECS && timestamp >= start
}

struct Window {
    top: u64,
    bits: [u64; (REPLAY_WINDOW / 64) as usize],
}

impl Window {
    fn new(counter: u64) -> Self {
        let mut w = Self {
            top: counter,
            bits: [0; (REPLAY_WINDOW / 64) as usize],
        };
        w.bits[0] = 1;
        w
    }

    /// Move the window up: bit `i` stands for the counter `top - i`.
    fn shift(&mut self, by: u64) {
        if by >= REPLAY_WINDOW {
            self.bits = [0; (REPLAY_WINDOW / 64) as usize];
            return;
        }
        let words = (by / 64) as usize;
        let bits = (by % 64) as u32;
        for j in (0..self.bits.len()).rev() {
            let mut v = 0;
            if j >= words {
                v = self.bits[j - words] << bits;
                if bits > 0 && j > words {
                    v |= self.bits[j - words - 1] >> (64 - bits);
                }
            }
            self.bits[j] = v;
        }
    }

    fn accept(&mut self, counter: u64) -> bool {
        if counter > self.top {
            self.shift(counter - self.top);
            self.top = counter;
            self.bits[0] |= 1;
            return true;
        }
        let age = self.top - counter;
        if age >= REPLAY_WINDOW {
            return false;
        }
        let (word, bit) = ((age / 64) as usize, age % 64);
        if self.bits[word] >> bit & 1 == 1 {
            return false;
        }
        self.bits[word] |= 1 << bit;
        true
    }
}

/// The replay window of the identities, FIFO when full.
pub struct ReplayRegistry {
    windows: HashMap<[u8; 8], Window>,
    order: VecDeque<[u8; 8]>,
    capacity: usize,
}

impl ReplayRegistry {
    pub fn new(capacity: usize) -> Self {
        Self {
            windows: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    /// Accept and record the pair, or report a replay.
    pub fn accept(&mut self, identity: [u8; 8], counter: u64) -> bool {
        if let Some(window) = self.windows.get_mut(&identity) {
            return window.accept(counter);
        }
        if self.windows.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.windows.remove(&oldest);
            }
        }
        self.windows.insert(identity, Window::new(counter));
        self.order.push_back(identity);
        true
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

/// Why the admission failed; only for the debug log, never on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Tag,
    Frame,
    Timestamp,
    Replay,
}

/// A connection that passed the admission.
pub struct Admitted {
    pub client_nonce: [u8; NONCE_LEN],
    pub opener: Opener,
    pub header: Header,
}

/// The admission of the endpoint: the key, the start time and the replay
/// window.
pub struct Gate {
    key: [u8; KEY_LEN],
    start: u64,
    replay: Mutex<ReplayRegistry>,
}

impl Gate {
    pub fn new(key: [u8; KEY_LEN], start: u64) -> Self {
        Self {
            key,
            start,
            replay: Mutex::new(ReplayRegistry::new(REPLAY_IDENTITIES)),
        }
    }

    /// Check the nonce and the sealed OPEN header, docs/cattail.md section 3.
    pub fn admit(&self, first: &[u8; FIRST_SEGMENT_LEN], now: u64) -> Result<Admitted, Reject> {
        let client_nonce: [u8; NONCE_LEN] = first[..NONCE_LEN].try_into().unwrap();
        let mut opener = Opener::new(&key_c2s(&self.key, &client_nonce), DIRECTION_C2S);
        let header = opener
            .open_header(first[NONCE_LEN..].try_into().unwrap())
            .map_err(|_| Reject::Tag)?;
        header.check(TYPE_OPEN).map_err(|_| Reject::Frame)?;
        if !timestamp_ok(now, self.start, header.timestamp) {
            return Err(Reject::Timestamp);
        }
        if !self
            .replay
            .lock()
            .unwrap()
            .accept(header.identity, header.counter)
        {
            return Err(Reject::Replay);
        }
        Ok(Admitted {
            client_nonce,
            opener,
            header,
        })
    }
}

/// The Unix time, seconds.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Pass the inner bytes read from `reader` to `writer` as frames.
async fn pump_encode<R, W>(
    reader: &mut R,
    writer: &mut W,
    mut encoder: Encoder,
    mut rng: StdRng,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; MAX_BODY_LEN];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return writer.shutdown().await;
        }
        let wire = encoder.encode(&buf[..n], &mut rng)?;
        writer.write_all(&wire).await?;
    }
}

/// Pass the bodies of the frames read from `reader` to `writer`.
async fn pump_decode<R, W>(reader: &mut R, writer: &mut W, opener: &mut Opener) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    while let Some(body) = read_frame(reader, opener).await? {
        writer.write_all(&body).await?;
    }
    writer.shutdown().await
}

/// What the client side needs for the OPEN frame.
#[derive(Clone)]
pub struct ClientParams {
    pub key: [u8; KEY_LEN],
    pub scheme: PaddingScheme,
    pub timestamp: u64,
    pub identity: [u8; 8],
    pub counter: u64,
}

/// Wrap a connection to a CatTail endpoint: the returned stream carries the
/// inner bytes (the TLS of the client). Used by the tests; the client core
/// has its own implementation of the same wire.
pub fn client_wrap<S>(stream: S, params: ClientParams) -> DuplexStream
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (inner, outer) = tokio::io::duplex(DUPLEX_LEN);
    tokio::spawn(async move {
        let (mut tcp_r, mut tcp_w) = tokio::io::split(stream);
        let (mut out_r, mut out_w) = tokio::io::split(outer);
        let mut rng = StdRng::from_entropy();
        let open = Header::open(params.timestamp, params.identity, params.counter);
        let (encoder, client_nonce) = Encoder::client(&params.key, params.scheme, open, &mut rng);
        let up = pump_encode(&mut out_r, &mut tcp_w, encoder, rng);
        let down = async {
            let mut server_nonce = [0u8; NONCE_LEN];
            tcp_r.read_exact(&mut server_nonce).await?;
            let mut opener = Opener::new(
                &key_s2c(&params.key, &client_nonce, &server_nonce),
                DIRECTION_S2C,
            );
            pump_decode(&mut tcp_r, &mut out_w, &mut opener).await
        };
        let _ = tokio::try_join!(up, down);
    });
    inner
}

/// The inner stream handed to the TLS acceptor: the pipe and the address of
/// the CatTail client.
pub(crate) struct InnerStream {
    io: DuplexStream,
    peer: SocketAddr,
}

impl net_utils::PeerAddr for InnerStream {
    fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.peer)
    }
}

impl AsyncRead for InnerStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for InnerStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

struct SilentGuard(Arc<AtomicUsize>);

impl SilentGuard {
    fn enter(count: &Arc<AtomicUsize>) -> Option<Self> {
        count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < MAX_SILENT_CONNECTIONS).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(count.clone()))
    }
}

impl Drop for SilentGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Never answer: discard the input until the deadline, then FIN.
async fn silence(mut stream: TcpStream, deadline: Instant, silent: &Arc<AtomicUsize>) {
    let guard = SilentGuard::enter(silent);
    let mut buf = [0u8; 2048];
    if guard.is_some() {
        let _ = tokio::time::timeout_at(deadline.into(), async {
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) => std::future::pending::<()>().await,
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        })
        .await;
    }
    let _ = stream.shutdown().await;
    let _ = tokio::time::timeout(FIN_DRAIN, async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await;
}

/// The listener state shared by the connections.
struct Listener {
    context: Arc<Context>,
    gate: Gate,
    scheme: PaddingScheme,
    silent: Arc<AtomicUsize>,
}

/// Run the CatTail listener if `[cattail]` is configured.
pub(crate) async fn listen(context: Arc<Context>) -> io::Result<()> {
    let Some(settings) = context.settings.cattail.clone() else {
        return Ok(());
    };
    let key = decode_key(&settings.key)
        .ok_or_else(|| io::Error::other("CatTail key is not base64 of 32 bytes"))?;
    let scheme = match settings.pattern.as_deref() {
        Some(p) => PaddingScheme::parse(p)?,
        None => PaddingScheme::default(),
    };

    let addr = settings.listen_address;
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::STREAM, Some(SockProtocol::TCP))?;
    if domain == Domain::IPV6 {
        socket.set_only_v6(false)?;
    }
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    let tcp_listener = TcpListener::from_std(socket.into())?;
    info!("Listening to CatTail TCP {}", addr);

    let state = Arc::new(Listener {
        context,
        gate: Gate::new(key, unix_now()),
        scheme,
        silent: Arc::new(AtomicUsize::new(0)),
    });
    loop {
        let (stream, peer) = match tcp_listener.accept().await.and_then(|(s, a)| {
            s.set_nodelay(true)?;
            SockRef::from(&s).set_keepalive(true)?;
            Ok((s, a))
        }) {
            Ok(x) => x,
            Err(e) => {
                debug!("CatTail TCP connection failed: {}", e);
                continue;
            }
        };
        tokio::spawn(handle(state.clone(), stream, peer));
    }
}

async fn handle(state: Arc<Listener>, mut stream: TcpStream, peer: SocketAddr) {
    let deadline = Instant::now() + rand::thread_rng().gen_range(SILENCE_MIN..=SILENCE_MAX);
    let context = &state.context;
    let client_ip = net_utils::unmap_ipv6(peer.ip());
    let client_id = log_utils::IdChain::from(log_utils::IdItem::new(
        log_utils::CLIENT_ID_FMT,
        context.next_client_id.fetch_add(1, Ordering::Relaxed),
    ));

    if context.is_decoy_only(Some(client_ip)) {
        log_id!(
            debug,
            client_id,
            "CatTail: decoy-only address {}, silence",
            client_ip
        );
        return silence(stream, deadline, &state.silent).await;
    }
    let Some(memory) = context.xhttp.reserve(
        CONNECTION_RESERVE,
        context.settings.xhttp_memory_limit,
        context.metrics.clone(),
    ) else {
        log_id!(
            debug,
            client_id,
            "CatTail: memory budget exhausted, silence"
        );
        return silence(stream, deadline, &state.silent).await;
    };

    let mut first = [0u8; FIRST_SEGMENT_LEN];
    let read = tokio::time::timeout_at(deadline.into(), stream.read_exact(&mut first)).await;
    if !matches!(read, Ok(Ok(_))) {
        drop(memory);
        log_id!(debug, client_id, "CatTail: no first segment, silence");
        return silence(stream, deadline, &state.silent).await;
    }
    let Admitted {
        client_nonce,
        mut opener,
        header,
    } = match state.gate.admit(&first, unix_now()) {
        Ok(x) => x,
        Err(reason) => {
            drop(memory);
            log_id!(
                debug,
                client_id,
                "CatTail: admission failed ({:?}), silence",
                reason
            );
            return silence(stream, deadline, &state.silent).await;
        }
    };
    log_id!(debug, client_id, "New CatTail client: {}", peer);

    let (inner, outer) = tokio::io::duplex(DUPLEX_LEN);
    tokio::spawn(Core::serve_stream(
        context.clone(),
        InnerStream { io: inner, peer },
        client_ip,
        client_id.clone(),
    ));

    let (mut tcp_r, mut tcp_w) = stream.into_split();
    let (mut out_r, mut out_w) = tokio::io::split(outer);
    let mut rng = StdRng::from_entropy();
    let (encoder, _) = Encoder::server(
        &state.gate.key,
        &client_nonce,
        state.scheme.clone(),
        &mut rng,
    );
    let up = async {
        let body = read_frame_rest(&mut tcp_r, &mut opener, &header).await?;
        out_w.write_all(&body).await?;
        pump_decode(&mut tcp_r, &mut out_w, &mut opener).await
    };
    let down = pump_encode(&mut out_r, &mut tcp_w, encoder, rng);
    if let Err(e) = tokio::try_join!(up, down) {
        log_id!(debug, client_id, "CatTail connection closed: {}", e);
    }
    drop(memory);
}

#[cfg(test)]
mod tests;
