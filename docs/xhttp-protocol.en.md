Русский: [xhttp-protocol.md](xhttp-protocol.md) (the Russian text is the main one; where they differ, the Russian text and the code are correct)

# NineLives, protocol 1 (internal name XHTTP)

This document describes the NineLives wire format: protocol 1, the only one implemented. The name NineLives is used in interfaces and documents. Code, configuration keys and file names keep the internal name `xhttp` (`xhttp_enable`, `transport_mode = "xhttp"`, `lib/src/http_xhttp.rs`). An overview with diagrams: [ninelives.en.md](ninelives.en.md). Versions 2 and 3, the CDN mode and Sudoku are reserved but not implemented. CONNECT uses its own earlier protocol and takes no part in the negotiation.

## Transport and version selection

One destination TCP session uses two HTTP/2 streams with the same path: GET `<xhttp_path><sid>` (default `xhttp_path = "/api/v2/"`) carries the downlink in the response body; POST with the same path carries the uplink in the request body. SID: 1 to 64 ASCII characters from `A-Z`, `a-z`, `0-9`, `-`, `_`. HTTP authentication is applied before pairing; both halves and a resume must belong to the same owner. A half waits for its pair up to 30 seconds; at most 1024 sessions may wait.

Both requests must carry exactly one `x-xhttp-version: 1` header. A missing header, a repeat, a list of values, a leading zero and an unknown value give HTTP 400. Successful GET and POST responses confirm `x-xhttp-version: 1`. Version selection has no fallback and no detection by body length. The `xhttp_supported_versions` table in the configuration of both sides is `[1]`. An empty C API table means the default `[1]`; an explicit empty TOML table is invalid.

On the server, `xhttp_enable = true` turns on the XHTTP listener and `transport_mode = "xhttp"` sets its mode. Other server transports are enabled by their own settings. In the client `[endpoint]` section `transport_mode` accepts `connect` or `xhttp`; when absent it follows the existing `xhttp_enable` (default CONNECT). In the C API, `transport_mode = nullptr` keeps that choice. `xhttp-cdn` and `sudoku` are rejected as unsupported modes. The old `xhttp_legacy_negotiation` key was removed; the client TOML parser rejects it explicitly.

## Frame format

All integers are unsigned big endian. Frames follow one another; the boundaries of HTTP DATA, TLS records and read calls do not set frame boundaries.

| Field | Size | Meaning |
| --- | --- | --- |
| type | 1 byte | A number from the registry below |
| offset | 8 bytes | Byte offset of the matching direction |
| short_length | 2 bytes | The length, if the high bit is 0 |
| long_length | 4 bytes, optional | Present if short_length has bit 0x8000 |
| payload | length bytes | Frame body |

For a payload up to 32767 bytes the header takes 11 bytes. For a longer payload the header takes 15 bytes: `short_length = 0x8000 | (length >> 16)`, then `long_length = length`. The decoder computes `((short_length & 0x7fff) << 16) | long_length`. The format maximum is `0x7fffffff`; the implementation limits the whole buffered frame to 2 MiB and rejects a declared length above that before it accumulates the payload. An encrypted record additionally limits the plaintext to 1 MiB. An unknown type is an error, including a type inside a successfully authenticated AEAD record; it must not be skipped as an extension.

| type | Name | Payload and offset |
| --- | --- | --- |
| 0x01 | OPEN | Destination address `host:port` as text; offset 0 |
| 0x02 | DATA | Application bytes; offset of the first byte |
| 0x03 | ACK | Empty body, offset of the confirmed bytes of the opposite direction; special FIN ACK below |
| 0x04 | FIN | Empty body; offset of the end of the direction |
| 0x05 | RST | Error or session reset |
| 0x06 | PAD | Ignored padding body, does not advance application offsets |
| 0x07 | RESUME | 48 bytes: offsets and HMAC; header offset 0 |
| 0x08 | PING | Health check nonce, instead of OPEN; offset 0 |
| 0x09 | PONG | Exact repeat of the PING nonce; offset 0 |
| 0x0a | HELLO | 54 bytes, start of the encrypted transport; offset 0 |
| 0x0b | HELLO_ACK | 102 bytes, signed answer; offset 0 |
| 0x0c | RESUME_TOKEN | 32 random bytes, once after OPEN; offset 0 |

OPEN, RESUME and PING are valid only in the matching start phase. A health check does not create a resumable TCP session and gets no RESUME_TOKEN. In a normal session the token is mandatory and precedes the server DATA.

## Suite 1: handshake and records

Body encryption is optional for a direct connection. A client with a configured Ed25519 public key starts with a plaintext HELLO frame. If the configured key is invalid, the connection fails. A KEX, signature or AEAD error never switches the session to a plaintext body. In plaintext mode the TLS terminator sees both the payload and the RESUME_TOKEN; the HMAC proof does not protect against such a terminator.

| HELLO payload | Size |
| --- | --- |
| suite = 1 | u16, 2 bytes |
| caps = 0 | u32, 4 bytes |
| client X25519 ephemeral public key | 32 bytes |
| client nonce | 16 bytes |

| HELLO_ACK payload | Size |
| --- | --- |
| suite = 1 | u16, 2 bytes |
| caps = 0 | u32, 4 bytes |
| server X25519 ephemeral public key | 32 bytes |
| Ed25519 signature | 64 bytes |

The negotiation prefix is `00 01 00 00 00 00`. An unknown suite or any non-zero caps is rejected before the KEX is accepted. The lengths must match exactly. A second HELLO_ACK after a completed handshake is invalid.

The signed transcript, without separators and without NUL:

```text
ASCII("cattunnel-xhttp-handshake-v1")
|| version:u16(1) || suite:u16(1) || caps:u32(0)
|| ASCII(sid) || client_public[32] || server_public[32] || client_nonce[16]
```

The static server Ed25519 seed signs the transcript; the client verifies the signature with the pinned public key. The shared secret is X25519 with fresh ephemeral keys of both sides. HKDF-SHA256 uses salt `SHA256(transcript)` and IKM equal to the shared secret; the 16-byte AES-128-GCM keys are derived separately with info `ASCII("cattunnel-xhttp-v1 c2s")` and `ASCII("cattunnel-xhttp-v1 s2c")`.

HELLO and HELLO_ACK travel as plaintext XHTTP frames. After the handshake every sent frame is encrypted into a record: `ciphertext_length:u32 || AES-GCM ciphertext || tag[16]`. The length includes the tag and excludes the 4-byte header; the valid range is 16..1048592 bytes. AAD: the exact ASCII bytes of the SID. Nonce: `direction:u8 || 00 00 00 || counter:u64`; direction 1 for c2s, 2 for s2c. The independent counters start at 0; the value `u64::MAX` is not used, and an overflow is an error. A lost or repeated record breaks the sequence of implicit counters: the old crypto state must not be continued.

```text
 client                                               server
   |== POST /<xhttp_path><sid>, request body open ====>|
   |== GET  /<xhttp_path><sid> ========================>|
   |   HELLO (in the POST body) ----------------------->|
   |<------------------------ HELLO_ACK (in the GET response body)
   |   from here every frame is inside an AEAD record   |
   |   OPEN host:port --------------------------------->|
   |<-------------------------------------- RESUME_TOKEN|
   |<------------------------------------------ DATA ...|
   |   DATA ... ---------------------------------------->|
```

Figure 1. The start of an encrypted session. The order in which GET and POST arrive does not matter: the server waits for the pair up to 30 seconds. Without a configured key, HELLO and HELLO_ACK are skipped and OPEN goes in plaintext inside TLS.

The Rust transport uses `RecordSealer: Send` and `RecordOpener: Send` through boxed states; suite 1 and the handshake factory are in `lib/src/xhttp_crypto.rs`. In the C++ client the only boundary is the `XhttpCrypto` class; the transport does not derive keys. The shared deterministic vector is `lib/tests/fixtures/xhttp_suite1.json` and the client `core/test/xhttp_suite1_vector.h` (in the client engine tree): X25519, transcript hash, signature, HKDF, both records.

## Resume

A session keeps the original RESUME_TOKEN, the offsets and the replay buffers after the streams break. The new pair uses the same SID and the same authenticated owner. A GET for an existing session returns a fresh `x-xhttp-challenge`: 32 random bytes in standard base64. The challenge belongs to this replacement pair and is consumed when it is processed; an old proof does not fit again.

RESUME payload:

```text
up_confirmed:u64 || down_received:u64 || proof[32]
proof = HMAC-SHA256(RESUME_TOKEN,
    ASCII("cattunnel-xhttp-resume-v1") || ASCII(sid)
    || up_confirmed:u64 || down_received:u64 || challenge[32])
```

`up_confirmed`: the last upload ACK known to the client. `down_received`: the download offset the client has received. The proof is checked in constant time before the old pumps are cancelled and the offsets change; a wrong proof gives HTTP 403 and leaves the old transport running. The offsets are checked before the cancel and once more after the old socket guards return. The server rejects an upload offset above the written one and a download offset outside the stored replay range. Replay and ACK repeat with the original offsets; duplicate DATA is dropped by offset, and a gap in the data is rejected.

An encrypted resume always starts a fresh HELLO/HELLO_ACK, gets new keys and zero counters; RESUME travels inside the new AEAD. The original token is kept. A plaintext resume of an encrypted session is forbidden. `xhttp_resume_timeout = 0` disables resume. The timeouts for waiting for the replacement GET and guards and for the final ACK are limited to 10 seconds.

```text
 client                                               server
   |  the old GET/POST pair broke, the session lives until xhttp_resume_timeout
   |== GET  same SID ===================================>|
   |<== 200, x-xhttp-challenge: base64(32 bytes) =========|
   |== POST same SID ===================================>|
   |   HELLO -> HELLO_ACK (encrypted sessions only)       |
   |   RESUME: up_confirmed, down_received, proof ------->|  proof check
   |                                                      |  wrong: HTTP 403
   |<== data replayed from the named offsets ============>|
```

Figure 2. Resuming a session with a new pair of requests.

## FIN, flow control and memory

An upload FIN closes writing to the destination and keeps the ability to receive the answer. The server sends the download FIN with the final offset. The client must confirm it with an ACK with the same offset and a payload of exactly `[0x04]`. A normal ACK has an empty body. A FIN ACK is valid only after the downlink EOF and at the exact final offset. Until then the server keeps the data for repetition; a lost FIN is repeated on the replacement transport. After the FIN ACK the session becomes terminal and is removed without waiting for the full resume timeout. The general half-close interface of the application is a separate task; FIN frames do not mean it is finished.

An upload is acknowledged only after the destination socket sink accepts the bytes. A download is stored before a cancellable send and held until the ACK. With a full window the downlink waits for acknowledgements. HTTP/2 wire credit is counted separately from application offsets, including the XHTTP headers and the AEAD overhead.

The memory budget limits XHTTP-owned buffers with the global `xhttp_memory_limit`: the default is a quarter of min(physical RAM, cgroup limit), clamped to 64..512 MiB, with a fallback of 64 MiB. For 2 GiB the default is 512 MiB. A new session reserves a base of 256 KiB; a refused reservation gives HTTP 503 and does not evict existing sessions. Replay blocks and decoder growth get an extra reservation before copying, held until the last owner of the bytes. The downlink waits for the budget and keeps the already read peer chunk across a cancel. When the decoder reserve is refused, the uplink closes the transport without an ACK for the unaccepted bytes. The counter does not include TLS, h2 and kernel buffers or the exact allocator RSS. Metrics: `xhttp_reserved_bytes`, `xhttp_memory_rejections_total`.

## Versions and extensions

| Identifier | Purpose | Current behavior |
| --- | --- | --- |
| version 1 | Current XHTTP | The only supported version |
| version 2 | Stronger own encryption and CDN | Reserved, rejected |
| version 3 | Sudoku | Reserved, rejected |
| suite 1 | X25519 + Ed25519 + HKDF-SHA256 + AES-128-GCM | Supported |
| suite 2 | A future cipher suite | Reserved, rejected |
| caps | Future capabilities, including CDN, Sudoku, server PAD | Only 0 now; bit numbers are not assigned yet |
| types 0x0d..0x0f | Unassigned | Rejected |
| types 0x10..0x1f | Version 2 extensions | Reserved, rejected |
| types 0x20..0x2f | Sudoku / version 3 | Reserved, rejected |
| transport_mode xhttp-cdn / sudoku | Future modes | Rejected in the configuration |

Adding a version requires an explicit update of the tables on both sides and of the specification, a check of the HTTP version before OPEN is sent, exact suite and caps, and their binding to the transcript. A new suite is implemented behind the crypto interface with independent states; cryptography is not added to the transport. A new type or bit gets a definition, rules of validity by phase and version, negative parser tests and a shared cross-language vector. There is no fallback by length or on error.

Which NineLives fields the client gets in a `tt://` link: [ninelives.en.md](ninelives.en.md#8-client-side).
