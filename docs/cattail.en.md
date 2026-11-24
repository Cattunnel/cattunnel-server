# CatTail wire protocol, version 1

CatTail is an optional outer layer for the CatTunnel endpoint. The client wraps
the bytes of its ordinary TLS connection (the same TLS 1.3 + HTTP/2 CONNECT
session that it would send to port 443) into authenticated, padded frames, so
that from the first byte on the wire the TCP stream carries no TLS record
headers, no SNI and no fixed prefix. The endpoint removes the CatTail layer
and hands the inner bytes to the same TLS acceptor that serves its main TCP
listener. Everything inside (authentication, the CONNECT pool, rotation,
keepalive) is unchanged.

The diagrams in this document are informative; the text is normative. Russian text is the main one: [cattail.md](cattail.md). This English
translation follows it; where they differ, the Russian text is correct.

```text
 CLIENT                                                       ENDPOINT
 app bytes                                                    destination
    |                                                              ^
 +--v-----------+                                         +-------+------+
 | HTTP/2       |  CONNECT, pool, rotation, keepalive     | HTTP/2       |
 +--v-----------+                                         +-------^------+
 +--v-----------+                                         +-------+------+
 | TLS          |  the same TLS as on port 443            | TLS acceptor |
 +--v-----------+                                         +-------^------+
 +--v-----------+  N_c | frames             TCP           +-------+------+
 | CatTail      |=======================================>| CatTail      |
 | AEAD frames  |<=======================================| admission,   |
 | and padding  |  N_s | frames                          | silence      |
 +--------------+                                         +--------------+
```

Figure 1. Where CatTail sits. Only the outer layer is new; the TLS session
inside is the one a client would send to port 443.

This document is normative for both implementations: the endpoint
(Rust, `lib/src/cattail.rs`) and the client core (C++). Test vectors are in
[`cattail-vectors.json`](cattail-vectors.json); they are produced by an
independent Python implementation, `scripts/cattail_vectors.py`, and checked
by `cargo test`.

Attribution. CatTail takes ideas, not code or text, from three designs:
the Identity + Counter replay window and the silent admission come from the
Nowhere-Protocol specification by NodePassProject
(https://github.com/NodePassProject/Nowhere, specification licensed
CC BY 4.0; the wire format here is new); AEAD frames with random padding
around them and the timestamp window come from mieru
(https://github.com/enfein/mieru); the padding scheme string comes from
AnyTLS (https://github.com/anytls/anytls-go). The implementations of Nowhere
and mieru are GPL-3.0; no code from them, or from AnyTLS, is used.

The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.
All integers are unsigned and big-endian.

## 1. Primitives

| Name | Definition |
|---|---|
| `K` | the endpoint key, 32 bytes, shared by the endpoint and its clients (configuration `[cattail] key`, subscription tag 0x58), base64 (standard alphabet, with padding) in text form |
| AEAD | AES-128-GCM (RFC 5116), 16-byte key, 12-byte nonce, 16-byte tag, empty associated data. The same primitive and nonce layout as NineLives suite 1 (`lib/src/xhttp_crypto.rs`) |
| HKDF | HKDF-SHA256 (RFC 5869) |
| `N_c` | the client nonce, 24 bytes |
| `N_s` | the server nonce, 24 bytes |

### 1.1 Direction keys

    key_c2s = HKDF-Expand(HKDF-Extract(salt = N_c,       IKM = K), info = "cattail-v1 c2s", L = 16)
    key_s2c = HKDF-Expand(HKDF-Extract(salt = N_c || N_s, IKM = K), info = "cattail-v1 s2c", L = 16)

The info strings are ASCII without a terminating zero. `N_s` makes the
keys of the server direction fresh even if a client segment were accepted
twice (for example after a restart of the endpoint).

```text
 K (32 bytes, endpoint key)
   |
   +-- HKDF-Extract(salt = N_c)          --> HKDF-Expand("cattail-v1 c2s", 16)
   |                                           = key_c2s   (client to server)
   +-- HKDF-Extract(salt = N_c || N_s)   --> HKDF-Expand("cattail-v1 s2c", 16)
                                               = key_s2c   (server to client)

 AEAD nonce (12 bytes) = direction (1) | 00 00 00 | n (8, big-endian)
 direction: 01 client to server, 02 server to client; n counts every
 header seal and every body seal of that direction from 0.
```

Figure 2. Direction keys and nonces.

### 1.2 AEAD nonces

Each direction keeps a 64-bit seal counter `n`, starting at 0. Every AEAD
operation of that direction (a header or a body, see 2.2) uses the nonce

    nonce = direction (1 byte) || 00 00 00 || n (8 bytes)

with `direction = 0x01` for client to server and `0x02` for server to
client, and then increments `n`. A side MUST close the connection instead of
reusing a counter value.

## 2. Framing

### 2.1 Stream layout

Client to server:

    N_c (24) | frame | frame | ...

Server to client, only after a successful admission (section 3):

    N_s (24) | frame | frame | ...

The server sends `N_s` immediately before its first frame, in the same
write. The first frame of the client direction has the type OPEN, every other
frame (in both directions) has the type DATA.

```text
 client to server:  N_c (24) | OPEN frame | DATA frame | DATA frame | ...
 server to client:  N_s (24) | DATA frame | DATA frame | ...
                    (only after a successful admission; N_s and the first
                     frame are sent in the same write)
```

Figure 3. Stream layout of both directions.

### 2.2 Frame

    E(header) (48) | mid padding (mid_len) | E(body) (body_len + 16) | end padding (end_len)

* `E(header)` is the 32-byte header (2.3) sealed with the AEAD: 32 bytes of
  ciphertext and 16 bytes of tag.
* `E(body)` is the body sealed with the AEAD, with the next counter value
  after the header. When `body_len = 0` the body is absent: no ciphertext, no
  tag, and the counter is not incremented. Such a frame carries only padding.
* The padding bytes are not authenticated and carry no meaning. The receiver
  MUST skip them without looking at them. The sender chooses their content
  (random bytes by default).

```text
 +-------------------+----------------+----------------------+--------------+
 | E(header)         | mid padding    | E(body)              | end padding  |
 | 32 B ciphertext   | mid_len bytes  | body_len B ciphertext| end_len bytes|
 | + 16 B tag = 48 B | not sealed,    | + 16 B tag           | not sealed,  |
 |                   | skipped by the | (absent when         | skipped by   |
 |                   | receiver       | body_len = 0)        | the receiver |
 +-------------------+----------------+----------------------+--------------+
  seal counter n      (no counter)     seal counter n + 1     (no counter)
```

Figure 4. One frame on the wire. Padding is random by default.

### 2.3 Header (32 bytes, plaintext)

| Offset | Length | Field | OPEN | DATA |
|---|---|---|---|---|
| 0 | 1 | type | 0x01 | 0x02 |
| 1 | 1 | version | 0x01 | 0x01 |
| 2 | 8 | timestamp: Unix time of the client, seconds | set | 0 |
| 10 | 8 | identity | set | 0 |
| 18 | 6 | counter (48 bits) | set | 0 |
| 24 | 2 | body_len, 0..16384 | | |
| 26 | 2 | mid_len, 0..65535 | | |
| 28 | 2 | end_len, 0..65535 | | |
| 30 | 2 | reserved, sent as 0, ignored | | |

Receivers MUST treat as an error: a type other than OPEN for the first client
frame, a type other than DATA for any other frame, a version other than 1, a
`body_len` above 16384. Fields marked 0 are sent as zero and ignored.

The body bytes, concatenated in frame order, are the inner byte stream (the
TLS connection). Frame boundaries carry no meaning for the inner stream.

```text
 offset  len  field      OPEN (first client frame)       DATA (all others)
 ------  ---  ---------  ------------------------------  -----------------
      0    1  type       01                              02
      1    1  version    01                              01
      2    8  timestamp  Unix seconds of the client       0
     10    8  identity   random per client start         0
     18    6  counter    per identity, +1 per TCP        0
                         connection
     24    2  body_len   0..16384
     26    2  mid_len    0..65535
     28    2  end_len    0..65535
     30    2  reserved   sent as 0, ignored
 total 32 bytes, big-endian; sealed to 48 bytes (32 + 16 tag)
```

Figure 5. Header of 32 bytes by offset.

### 2.4 End of stream

A TCP FIN received at a frame boundary ends the inner stream of that
direction (half-close). A FIN inside a frame is an error. The inner TLS has
its own close_notify, so CatTail does not authenticate the end of stream.

### 2.5 Identity and counter

`identity` is 8 random bytes that the client draws again at every start of
its process (or every new configuration). `counter` starts at 0 for a new
identity and grows by 1 for every new TCP connection the client opens with
this identity. A client that cannot keep the counter MUST draw a new identity.

## 3. Admission (server)

The server reads `N_c` and the first 48 bytes of `E(header)`, derives
`key_c2s`, and checks, in this order:

1. the client address is not in `decoy_only_ips_file` (the decoy trap);
2. the endpoint has memory left in its buffer budget (the same budget as
   NineLives, `xhttp_memory_limit`);
3. the header opens with `key_c2s` and seal counter 0;
4. type is OPEN, version is 1, `body_len <= 16384`;
5. `|now - timestamp| <= 120` seconds, and `timestamp >= T_start`, the
   Unix time the CatTail listener started (an OPEN recorded before a restart
   cannot be replayed after it, when the replay window is empty);
6. the replay window (3.1) accepts `(identity, counter)`; only now the pair
   is recorded.

If all checks pass, the connection is admitted: the server reads the rest of
the frame and passes the body bytes to the TLS acceptor of its main TCP
listener, as if they had arrived on that listener from the same client
address.

```text
 TCP accept on the CatTail port; deadline D = accept time + random 10..60 s
   |
   v
 client address in decoy_only_ips_file? ------ yes ---+
   | no                                               |
 buffer budget (xhttp_memory_limit) left? ----- no ---+
   | yes                                              |
 72 bytes (N_c + E(header)) read before D? ---- no ---+
   | yes                                              |
 header opens with key_c2s at counter 0,              |
 type OPEN, version 1, body_len <= 16384? ----- no ---+
   | yes                                              |
 |now - timestamp| <= 120 s and                       |
 timestamp >= T_start? ------------------------ no ---+
   | yes                                              |
 replay window accepts (identity, counter)? --- no ---+
   | yes                                              v
   v                                               SILENCE
 ADMIT: read the rest of the frame, pass the       (section 3.2)
 body bytes to the TLS acceptor
```

Figure 6. Admission checks in order. Every failure leads to the same silence.

### 3.1 Replay window

Per identity the server keeps `top`, the highest accepted counter, and a
1024-bit map of the counters `top - 0 ... top - 1023` that were accepted.
A counter `c` is accepted when:

* the identity is new: `top = c`, the map holds only `c`;
* `c > top`: the map is shifted by `c - top`, `top = c`;
* `top - c < 1024` and `c` is not in the map: `c` is added.

Otherwise (`c` is in the map, or `top - c >= 1024`) it is a replay. The
registry holds at most 16384 identities; adding one more removes the identity
that was added first (FIFO).

```text
 counters:  top-1023 ................................ top-1   top
 map:       1024 bits, a set bit = counter accepted

 new identity:           top = c, map holds only c
 c > top:                shift the map by (c - top), top = c, set bit c
 top-1023 <= c < top
   and bit c clear:      set bit c (accepted)
 bit c set, or c < top-1023:  REPLAY, rejected
 registry: at most 16384 identities, the oldest added is removed first
```

Figure 7. Replay window of one identity.

### 3.2 Silence

When any admission check fails, or the first 72 bytes do not arrive in time,
the server:

* never writes a byte to the connection;
* keeps reading and discarding whatever arrives;
* at a deadline drawn uniformly from 10 to 60 seconds after the TCP accept,
  sends FIN (shutdown of the write side), discards input for up to 5 more
  seconds or until the peer's FIN, then closes the socket.

The behaviour is the same for every failed check, so a prober learns nothing
from timing or from the way the connection ends.

```text
 t = 0 (accept)            t = D (10..60 s)         t = D + 5 s at most
   |---- read and discard ------|-- FIN, discard ------|-- close the socket
   |  the endpoint never writes a byte to the peer, whatever the failed check
```

Figure 8. Timeline of silence. D is drawn once per connection.

### 3.3 Errors after admission

After admission, a frame that fails to open, a bad type or version, a
`body_len` above 16384 or a FIN inside a frame closes the connection at once
(both directions).

## 4. Padding scheme

The sender shapes its first writes with a scheme string, the idea of the
AnyTLS padding scheme. The receiver does not need the sender's scheme: any
sequence of valid frames is accepted. The server uses the scheme from
`[cattail] pattern`, the client the one from subscription tag 0x59.

### 4.1 Syntax

Lines separated by `\n` (a `;` is accepted as a separator too, and `\r` is
ignored). Each line is `key=value`. Spaces around keys and values are
ignored. Empty lines are ignored.

| Key | Value | Meaning | Default |
|---|---|---|---|
| `stop` | 0..16 | number of shaped writes | 0 |
| `0` .. `15` | items, comma separated | how write `i` is cut, `i < stop` | none (write sent as steady frames) |
| `mid` | `lo-hi` | mid padding of steady frames, 0..4096 | `0-0` |
| `end` | `lo-hi` | end padding of steady frames, 0..4096 | `0-0` |
| `nonce` | `lo-hi` | printable prefix of the sender's own nonce, 0..24 bytes | `0-0` |

An item is either `lo-hi` or a single number `n` (meaning `n-n`): the
total wire size of one frame, 48..16448 (16448 = 48 + 16384 + 16), or `c`.
At most 16 items per line. Values must satisfy `lo <= hi`. Unknown keys are
ignored (newer schemes may add keys). Any malformed known key makes the whole
string invalid: the endpoint refuses to start, the client uses its built-in
default.

The endpoint's default scheme (used when `pattern` is not set):

    stop=3
    0=400-1400,c,200-900
    1=300-1200,c,100-700
    2=100-600,c
    mid=0-32
    end=0-96

### 4.2 Sender algorithm

Writes are counted per direction from 0: a write is one chunk of inner bytes
handed to the CatTail layer (for the server: one read from the TLS side). For
write `i < stop` with a line `i`, the data `D` is cut by the items in order:

* `c`: if `D` is empty, stop processing the items of this write;
* a size item: draw `L` uniformly in `lo..hi`. If `D` is not empty and
  `L >= 65`, the frame carries `b = min(|D|, L - 64, 16384)` bytes of `D`
  and `L - 64 - b` bytes of padding. Otherwise the frame carries padding
  only, `L - 48` bytes. The padding is split at a uniform point into
  `mid_len` and `end_len`.

Bytes of `D` left after the items, and every write with `i >= stop` or
without a line, are sent as steady frames: bodies of up to 16384 bytes,
`mid_len` and `end_len` drawn from `mid` and `end`.

The client's OPEN frame is the first frame it sends, whatever its body (it may
carry padding only). The client SHOULD send `N_c` and all frames of write 0
in one write to the socket.

The nonce policy `nonce=lo-hi` replaces the first `P` bytes of the
sender's own nonce, `P` uniform in `lo..hi`, with bytes drawn uniformly from
the printable ASCII range 0x21..0x7E. The rest stays random.

```text
 default scheme, write 0 carrying data D:   0=400-1400,c,200-900

   item 400-1400 : L = random 400..1400. Frame carries
                   b = min(|D|, L - 64, 16384) bytes of D and L - 64 - b
                   bytes of padding (split at a random point into mid/end)
   item c        : if D is now empty, stop; else go on
   item 200-900  : next frame in the same way
   rest of D     : steady frames, body <= 16384,
                   mid_len in 0-32, end_len in 0-96
 writes 3 and later (stop=3): steady frames only
```

Figure 9. How the default padding scheme cuts the first write.

## 5. Configuration of the endpoint

    [cattail]
    listen_address = "0.0.0.0:20443"
    key = "<base64 of 32 random bytes>"
    # optional, see section 4
    pattern = """
    stop=3
    0=400-1400,c,200-900
    1=300-1200,c,100-700
    2=100-600,c
    mid=0-32
    end=0-96
    """

The endpoint listens on one TCP port. A range of ports is mapped to it by a
DNAT or redirect rule of the host firewall, outside the endpoint (the same
way as the 8443/995 redirect of the `alt_port` role); the client picks a
random port of the range for each new TCP connection.

```text
 clients ---- TCP to a random port P of the range [a..b] ----+
 (new port for every new TCP connection)                     |
                                                             v
                                         host firewall: DNAT / redirect
                                                             |
                                                             v
                              endpoint  [cattail] listen_address (one port)
 main TCP listener (port 443)  ------------------ unchanged, separate port
```

Figure 10. Port range mapped to one listener, outside the endpoint.

## 6. Security notes

* `K` authenticates the CatTail layer only. Users are authenticated inside,
  by the CONNECT credentials, as without CatTail. Anyone holding `K` (every
  subscriber of the endpoint) can open the outer layer and reach the inner TLS;
  this is the same exposure as port 443.
* An OPEN recorded on the wire can be replayed to another endpoint that uses
  the same `K` within the timestamp window. Each endpoint SHOULD have its own
  key.
* The registry of 16384 identities bounds memory. An identity evicted from it
  could be replayed within 120 s; that needs more than 16384 distinct client
  starts in two minutes.
* CatTail hides the TLS record lengths of the first writes only as far as the
  padding scheme reaches. Steady frames show the sizes of the inner writes,
  plus 64 bytes and the steady padding.
* Low-entropy shaping of the ciphertext (as in mieru) is not part of version 1.
