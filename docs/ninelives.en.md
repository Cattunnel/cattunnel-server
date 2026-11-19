# NineLives (XHTTP protocol 1)

[Русская версия](ninelives.md)

NineLives is the second transport of CatTunnel, next to `CONNECT`. This page is an overview
for server operators. The exact wire format is in [xhttp-protocol.md](xhttp-protocol.md) (in
Russian).

The config keys say `xhttp` for historical reasons. NineLives is **not** compatible with XHTTP
from Xray: it has its own framing, its own body encryption and its own resume, and it does not
accept Xray links or configurations.

## How it works

- **Paired requests.** One TCP session of the client uses two HTTP/2 streams on the same path
  `<xhttp_path><sid>`: a `GET` whose response body carries the download and a `POST` whose
  request body carries the upload. The session id (`sid`) is 1 to 64 characters from
  `A-Z a-z 0-9 - _`. Both requests carry `x-xhttp-version: 1`. HTTP authentication is
  checked before the two halves are paired, and both halves must belong to the same user. A
  half waits up to 30 s for its pair; at most 1024 sessions may wait.
- **Frames.** The bodies carry frames: `OPEN` (destination `host:port`), `DATA`, `ACK`,
  `FIN`, `RST`, `PAD`, `RESUME`, `PING`/`PONG`, `HELLO`/`HELLO_ACK` and
  `RESUME_TOKEN`. Each frame has a type, a byte offset and a length. Upload bytes are
  acknowledged only after the destination socket has accepted them; download bytes are kept
  until the client acknowledges them.
- **Signed handshake and body encryption (suite 1).** Optional for direct connections. The
  client sends `HELLO` with an ephemeral X25519 public key and a nonce. The server answers
  with its own ephemeral X25519 key and an Ed25519 signature over the transcript (protocol
  version, suite, capabilities, `sid`, both public keys, client nonce), made with the static
  key `xhttp_secret_key`. The client checks the signature with the server public key it got
  in its link. HKDF-SHA256 (salt = SHA-256 of the transcript) derives a separate AES-128-GCM
  key for each direction; the nonce has a direction byte and a per-direction counter, and the
  `sid` is the associated data. A failed key exchange, signature or decryption never falls
  back to an unencrypted body. Without encryption the stream is protected by TLS alone.
- **Resume.** The server gives each TCP session a 32-byte resume token. After the connection
  drops, the client opens a new `GET`/`POST` pair with the same `sid`; the server sends a
  fresh challenge, and the client answers with `RESUME`: the offsets it has and an
  HMAC-SHA256 proof over the token, the `sid`, the offsets and the challenge. The old
  transport is cancelled only after the proof is checked. An encrypted session always runs a
  new handshake with new keys before resuming. The server replays unacknowledged download
  data from the client's offset. `xhttp_resume_timeout` says how long a broken session is
  kept (60 s by default).
- **FIN.** The server sends `FIN` with the final download offset and keeps the data until the
  client acknowledges that offset, then frees the session at once.
- **Memory budget.** All NineLives buffers are charged to one global budget,
  `xhttp_memory_limit`. Each new session reserves 256 KiB first; if that does not fit, the
  request gets HTTP 503 and existing sessions are not evicted. Decoder and replay buffers are
  reserved before data is copied. A download waits for budget; an upload that cannot reserve
  space closes its transport without acknowledging the unaccepted bytes, and the session can
  resume later. TLS, HTTP/2 and kernel buffers are not counted.

Protocol versions 2 and 3, crypto suite 2, the CDN mode and non-zero capability bits are
reserved and rejected. There is no fallback to another version.

## Server configuration

```toml
# vpn.toml
xhttp_enable = true
xhttp_path = "/api/v2/"
xhttp_secret_key = "<base64 of a 32-byte Ed25519 seed>"
xhttp_resume_timeout = 60
# xhttp_memory_limit = 268435456
# xhttp_unacked_limit = 1048576
```

| Key | Default | Meaning |
| --- | --- | --- |
| `xhttp_enable` | `false` | Accept NineLives requests on the main hosts |
| `xhttp_path` | `"/api/v2/"` | Path prefix; the `sid` is appended to it. Must not overlap `ping_path` or `speedtest_path` |
| `xhttp_secret_key` | none | Standard base64 of the 32-byte Ed25519 seed that signs `HELLO_ACK`. Without it the server cannot answer `HELLO` |
| `xhttp_resume_timeout` | `60` | Seconds a broken session is kept for resume; `0` turns resume off |
| `xhttp_memory_limit` | 1/4 of the smaller of RAM and cgroup limit, clamped to 64..512 MiB (64 MiB if unknown) | Global budget for NineLives buffers, bytes |
| `xhttp_unacked_limit` | `1048576` | Unacknowledged bytes per session |
| `xhttp_supported_versions` | `[1]` | Only `[1]` is valid |
| `transport_mode` | `"xhttp"` | Only `"xhttp"` is valid; `xhttp-cdn` and `sudoku` are reserved |

`CONNECT` keeps working next to NineLives on the same port.

## Key generation

`xhttp_secret_key` is the raw 32-byte Ed25519 seed in standard base64. The client needs the
matching 32-byte public key, also in standard base64 (44 characters). With OpenSSL:

```shell
openssl genpkey -algorithm ed25519 -out xhttp-key.pem
# xhttp_secret_key for vpn.toml
openssl pkey -in xhttp-key.pem -outform DER | tail -c 32 | base64
# public key for the clients
openssl pkey -in xhttp-key.pem -pubout -outform DER | tail -c 32 | base64
```

The seed is a secret: whoever has it can impersonate the server to NineLives clients. Keep
`vpn.toml` readable only by the service.

## Client side

The endpoint's `-c` export does not include NineLives settings. The CatTunnel client reads
them from its `tt://` link (usually written by a subscription service):

| Tag | Value |
| --- | --- |
| `0x4F` | NineLives on (boolean) |
| `0x50` | Path prefix, if it differs from `/api/v2/` |
| `0x51` | Server Ed25519 public key, base64 (44 characters) |

Without `0x51`, or with the encryption switch off in the app, the client sends no `HELLO` and
the body is protected by TLS only. In the CatTunnel app NineLives is in the expert settings of
a server.

## Status

NineLives is released with CatTunnel 1.5.9. Session resume of up to 60 s is part of this
release. Traffic shape (frame sizes and timing compared with a browser) is still being
tuned, so the transport may change in a later protocol version.