# NineLives: protocol 1 (internal name XHTTP)

NineLives carries each tunneled TCP connection as a pair of HTTP/2 streams
inside one TLS connection, instead of one CONNECT stream. The name NineLives
is used in user interfaces and documents; the code, the configuration keys and
the files keep the internal name `xhttp` (`xhttp_enable`, `transport_mode =
"xhttp"`, `http_xhttp.rs`). The shape of the requests resembles XHTTP of Xray;
the protocol here is our own, incompatible with it, and carries no brand on the
wire: the path comes from configuration.

This is an overview with diagrams. The normative text is
[xhttp-protocol.en.md](xhttp-protocol.en.md) (frame tables, handshake
transcript, resume proof, limits; the main text is Russian, [xhttp-protocol.md](xhttp-protocol.md)). Where the two disagree, `xhttp-protocol.md`
and the code win. Related: [connect.en.md](connect.en.md), [cattail.en.md](cattail.en.md),
[upstream-divergence.en.md](upstream-divergence.en.md).

Versions in 1.5.9.5. Only protocol version 1 is implemented: `xhttp_supported_versions`
must equal `[1]` and any other value fails settings validation. Version 2
(NineLivesSec, a stronger own cipher and a CDN mode) and version 3 are
reserved; the endpoint rejects them, and `transport_mode` other than `xhttp` is
rejected as well. Cipher suite 1 is the only suite.

Russian text is the main one: [ninelives.md](ninelives.md). This English translation follows it.

## 1. Two streams, one connection

```text
 client                          one TLS connection (h2)               endpoint
   |                                                                       |
   |== stream A: GET  /api/v2/<sid>   (downlink) ========================>|
   |<=============== response 200, body = frames of the downlink ==========|
   |                                                                       |
   |== stream B: POST /api/v2/<sid>   (uplink, request body is open) =====>|
   |   body = frames of the uplink                                          |
   |                                                                       |
   |   both carry   x-xhttp-version: 1   and the same credentials          |
```

Figure 1. A session is a GET (server to client bytes) and a POST (client to
server bytes) with the same path. Many sessions share one TLS connection.

The session id (SID) is 1 to 64 characters of `A-Z a-z 0-9 - _`. The endpoint
authenticates each half before pairing, and both halves must belong to one
user. A half waits for its pair up to 30 seconds; at most 1024 sessions wait.
A request without exactly one `x-xhttp-version: 1` gets HTTP 400.

## 2. Layers

```text
 destination TCP bytes
        |
 +------v-------------------------------------------+
 | NineLives frames: OPEN DATA ACK FIN RST PAD ...   |  offsets per direction
 +------+-------------------------------------------+
        |   suite 1: every frame becomes one record
 +------v-------------------------------------------+
 | record: u32 length | AES-128-GCM ciphertext | tag |  AAD = SID
 +------+-------------------------------------------+
        |
 +------v-------------------------------------------+
 | HTTP/2 DATA of the POST (up) / of the GET (down)  |
 +------+-------------------------------------------+
        |
 +------v-------------------------------------------+
 | TLS (and, optionally, CatTail outside it)         |
 +--------------------------------------------------+
```

Figure 2. Layers of a NineLives session. Body encryption is used when the client
has the Ed25519 public key of the endpoint; without it frames travel unencrypted
inside TLS.

## 3. Start of a session

The downlink answer is sent before the destination is connected, because the key
exchange is answered in its body.

```text
 client (POST body)                         endpoint (GET response body)
   |                                              |
   |<---------- 200 + x-xhttp-version: 1 ---------|  headers of the GET
   |-- HELLO  (suite 1, caps 0, X25519 pub, nonce)|  54 bytes, plain frame
   |<--- HELLO_ACK (X25519 pub, Ed25519 sig) -----|  102 bytes, plain frame
   |     both sides derive c2s / s2c AES keys     |
   |-- [OPEN host:port]   (sealed record) ------->|
   |<-- [RESUME_TOKEN 32 random bytes] (sealed) --|  then the destination is
   |                                              |  connected
   |-- [DATA ...] ------------------------------->|
   |<-------------------------- [DATA ...] -------|
   |-- [ACK offset] ----------------------------->|  uplink ACK, see section 5
   |<----------------------- [ACK offset] --------|
```

Figure 3. Handshake with body encryption. Without the endpoint key HELLO and
HELLO_ACK are skipped and OPEN is the first frame in the clear. A health check
sends PING instead of OPEN and gets PONG with the same nonce; it creates no
resumable session and no RESUME_TOKEN.

Key schedule (suite 1):

```text
 X25519(client_eph, server_eph) --> shared secret --+
                                                     v
 transcript = "cattunnel-xhttp-handshake-v1" | version | suite | caps
              | SID | client_pub | server_pub | client_nonce
 Ed25519 (endpoint static key) signs transcript --> client checks signature
                                                     |
 HKDF-SHA256(salt = SHA256(transcript), IKM = shared secret)
        +--> key c2s  info "cattunnel-xhttp-v1 c2s"   (16 bytes)
        +--> key s2c  info "cattunnel-xhttp-v1 s2c"   (16 bytes)
 nonce = direction (1 c2s, 2 s2c) | 00 00 00 | counter u64
```

Figure 4. Keys of suite 1: X25519, Ed25519, HKDF-SHA256, AES-128-GCM. A failed
signature, key exchange or AEAD check never falls back to the clear.

## 4. Frame

```text
 short form (payload < 32768):
 +------+-----------------+--------------+-----------------+
 | type | offset (u64 BE) | length (u16) | payload         |
 | 1 B  | 8 B             | 2 B          | length bytes    |
 +------+-----------------+--------------+-----------------+
 long form (length >= 32768), 15-byte header:
 +------+-----------------+------------------+------------+---------+
 | type | offset (u64 BE) | 0x8000 | len>>16 | length u32 | payload |
 |      |                 | (u16)            | (full len) |         |
 +------+-----------------+------------------+------------+---------+
```

Figure 5. Frame header, 11 or 15 bytes. Types: 01 OPEN, 02 DATA, 03 ACK, 04 FIN,
05 RST, 06 PAD (ignored), 07 RESUME, 08 PING, 09 PONG, 0a HELLO, 0b HELLO_ACK,
0c RESUME_TOKEN. Any other type is an error. The endpoint buffers at most 2 MiB
per frame; a sealed record holds at most 1 MiB of plaintext. PAD is accepted and
dropped by the endpoint; the endpoint does not send it in 1.5.9.5.

## 5. Offsets, ACK and FIN

Each direction counts bytes from 0; DATA carries the offset of its first byte,
so a duplicate is recognised and a gap is an error.

- Uplink ACK: the endpoint confirms client bytes only after the destination
  socket has accepted them. A confirmation is triggered by 64 KiB of unconfirmed
  bytes. It travels in the downlink.
- Downlink ACK: the client confirms received bytes; the endpoint keeps data
  until confirmed (replay buffer) and stops sending when the window
  (`xhttp_unacked_limit`, default 1 MiB) is full.
- FIN carries the final offset of a direction. The client answers a server FIN
  with an ACK whose payload is exactly `[0x04]` and the same offset.

## 6. Resume

If a TLS connection dies, the session is kept for `xhttp_resume_timeout`
(default 60 s; 0 turns resume off). The client opens a new GET and POST with the
same SID and the same user.

```text
 client                                         endpoint (session kept)
   |-- GET  /api/v2/<sid> ----------------------->|
   |<-- 200  x-xhttp-challenge: <32 random bytes> -|  fresh per replacement pair
   |-- POST /api/v2/<sid>                          |
   |-- [HELLO] (encrypted sessions: new keys)      |
   |<-- [HELLO_ACK] --------------------------------|
   |-- [RESUME up_confirmed | down_received | proof] ->|
   |        proof = HMAC-SHA256(RESUME_TOKEN, "cattunnel-xhttp-resume-v1"
   |                 | sid | up_confirmed | down_received | challenge)
   |<-- downlink bytes from offset down_received  ----|  repeated from the
   |-- uplink bytes from offset up_confirmed -------->|  replay buffers
```

Figure 6. Resume. The proof is checked in constant time before the old pumps are
cancelled; a wrong proof gives HTTP 403 and leaves the old transport running.
An encrypted session always repeats HELLO and gets new keys and zero counters;
resuming it in the clear is refused.

## 7. Memory budget

All buffers owned by NineLives (and by CatTail connections) count against
`xhttp_memory_limit`: one quarter of the smaller of physical memory and the
cgroup limit, clamped to 64..512 MiB, 64 MiB if unknown. A new session reserves
256 KiB; when the reserve is refused the endpoint answers HTTP 503 and does not
evict existing sessions. Metrics: `xhttp_reserved_bytes`,
`xhttp_memory_rejections_total`.

## 8. Client side

The client core (`core/src/xhttp_*.cpp`, see its documents) opens 2 to 4 TLS
connections for NineLives, uses the same handshake limiter as CONNECT (3 per
minute, 2 to 6 s apart) and replaces a connection when its rotation time is due
or the pool is degraded. The key rotation across servers (9a) and the
fingerprint profile apply to CONNECT only; NineLives does not take part in 9a.

## 9. Compatibility

- Not compatible with Xray XHTTP or with the official TrustTunnel client: the
  frames, the cipher and the resume are our own.
- Off by default (`xhttp_enable = false`). A request on the NineLives path of a
  decoy-only address is refused as a failed authentication.
- A client without the endpoint key can still use unencrypted frames inside
  TLS; the endpoint does not force encryption.
