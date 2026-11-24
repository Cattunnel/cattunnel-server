# Divergence from upstream TrustTunnel (endpoint, release 1.5.9.5)

Russian text is the main one: [upstream-divergence.md](upstream-divergence.md). This English translation follows it.

This document lists where the CatTunnel endpoint (this repository) differs
from upstream TrustTunnel in release 1.5.9.5. It describes the present state,
not the history.

Compared versions. The fork's merge-base with upstream is `v1.3.0-beta.2`
(`48f424c`). The content of upstream `v1.3.1` (TRUST-773, TRUST-805, TRUST-826)
is included, so the comparison below is against `v1.3.1`. Measured with
`git diff --numstat v1.3.1 HEAD`: 45 files, 13351 insertions, 240 deletions.
Measured with `git merge-tree --write-tree --name-only HEAD v1.3.1`: conflicts
only in `CHANGELOG.md` and `lib/tests/subscription.rs`.

Classes: A, the fork added something upstream does not have; B, upstream does it
otherwise and the fork's code is incompatible with it or hinders a merge; C, the
same problem is solved in another way. The client core has its own list in its
repository.

```text
 TCP :443 ----------------------+        TCP :cattail port
                                 |                |
                                 |        [A1 CatTail admission,
                                 |         silence, frames]
                                 v                v
                        +---------------------------------+
                        | TlsListener                     |
                        |  [A6 join split ClientHello,    |
                        |      count JA4]                 |
                        +----------------+----------------+
                                         v
                        +---------------------------------+
                        | TlsDemux: SNI + ALPN            |
                        |  [B1 h2 on decoy hosts]         |
                        +----------------+----------------+
                                         v
                        +---------------------------------+
                        | HttpDemux: per request          |
                        |  [A2 NineLives channel]         |
                        +-----+-----------+----------+----+
                              v           v          v
                          tunnel      decoy site   subscription
                          [A5 decoy-  [A7 h1 keep-  (upstream)
                           only check] alive, bodies]
                          [A3 200 headers]
                          [A4 DATA 4096]
```

Figure 1. Where the fork's changes sit in the path of a connection (A1..A7,
B1 are the items below).


| # | Class | Item | Compatibility |
|---|---|---|---|
| A1 | Departures | CatTail outer layer | separate port, optional |
| A2 | Departures | NineLives (XHTTP) transport | off by default, own wire format |
| A3 | Departures | Plausible 200 answer to CONNECT | extra response headers only |
| A4 | Departures | DATA frames of 4096 bytes on CONNECT | legal frame size, no contract change |
| A5 | Departures | decoy_only_ips_file | unset by default |
| A6 | Departures | Split ClientHello joined, JA4 counted | accepts what upstream may stall on |
| A7 | Departures | Decoy site behaves like a real site | decoy hosts only |
| B1 | Conflicts | h2 on decoy (reverse proxy) hosts | upstream test asserts the opposite |
| B2 | Conflicts | Heavily modified upstream files | any upstream change there conflicts |
| B3 | Conflicts | Same change ported twice (CHANGELOG, subscription tests) | 2 files conflict on merge |
| B4 | Conflicts | Deep-link tags | no clash today; watch the tag space |
| C1 | Same problem, other way | Probing endpoints | both stay compatible |
| C2 | Same problem, other way | Keeping an idle tunnel alive | no server change |

## A. Departures: added by the fork, absent in upstream

### A1. CatTail outer layer

- Upstream: No outer layer: every connection starts with a TLS ClientHello on the listener.
- Fork: Second TCP listener configured by `[cattail]` (`lib/src/settings.rs:94`), started with the other listeners (`lib/src/core.rs:215`). Admission, silence and framing are in `lib/src/cattail.rs:1052`; inner bytes go to `Core::serve_stream`.
- Why: The wire carries no TLS record headers, no SNI and no fixed prefix, and a prober gets silence instead of a web site.
- Compatibility: Own port, off unless `[cattail]` is set. Official clients cannot use it; the official client path (port 443) is unchanged. Specification: [cattail.en.md](cattail.en.md).

### A2. NineLives (XHTTP) transport

- Upstream: CONNECT is the only way to tunnel TCP; there is no GET/POST session transport.
- Fork: A routing channel `lib/src/http_demultiplexer.rs:63` and the handler `lib/src/http_xhttp.rs:1058`; cipher suite 1 in `lib/src/xhttp_crypto.rs:48`; settings `xhttp_*` (`lib/src/settings.rs:275`).
- Why: A session survives the death of one TLS connection (resume) and has its own body encryption.
- Compatibility: `xhttp_enable = false` by default. Incompatible with Xray XHTTP and with official clients. See [ninelives.en.md](ninelives.en.md).

### A3. Plausible 200 answer to CONNECT

- Upstream: Bare `200` (`upstream v1.3.0-beta.2:lib/src/http_downstream.rs:226`).
- Fork: `date`, `server: nginx`, `cache-control`, two random-length hex ids (`lib/src/http_codec.rs:38`, used at `lib/src/http_downstream.rs:256`).
- Why: An empty answer of one size every time marks a tunnel; headers travel inside TLS and show only as size.
- Compatibility: Plain HTTP/2 headers, no contract change. Not tested against an official client in this repository.

### A4. DATA frames of 4096 bytes on CONNECT

- Upstream: Each write fills the window: `to_send = self.tx.capacity()` (`upstream v1.3.0-beta.2:lib/src/http2_codec.rs:338`).
- Fork: For CONNECT streams `capacity().min(4096)` (`lib/src/http2_codec.rs:352`); other methods keep the full window.
- Why: The size of the server DATA frames is part of the traffic shape; the fork uses a browser-sized frame (the name of the test in `http2_codec.rs`).
- Compatibility: Any h2 client accepts smaller frames.

### A5. decoy_only_ips_file

- Upstream: No list: an address is refused only for wrong credentials.
- Fork: `DecoyList` re-read by modification time (`lib/src/decoy_list.rs:19`); checked for CONNECT (`lib/src/http_downstream.rs:130`), NineLives, subscription, SNI credentials and CatTail.
- Why: An address that probed a closed port may keep seeing the site but never the VPN, even with valid credentials.
- Compatibility: A listed legitimate client is refused; the list is filled by the host firewall.

### A6. Split ClientHello joined, JA4 counted

- Upstream: One TLS record is parsed (`upstream v1.3.0-beta.2:lib/src/tls_listener.rs:72`).
- Fork: Handshake fragments are joined until the message is whole (`lib/src/tls_listener.rs:81`); the JA4 string is counted (`lib/src/tls_listener.rs:133`) and served at `lib/src/metrics.rs:19`.
- Why: RFC 8446 section 5.1 lets a client fragment its ClientHello over records and the code comment names CatTunnel anti-DPI as a source; the JA4 counter shows which client fingerprints reach the endpoint.
- Compatibility: Superset of upstream parsing; a normal one-record hello behaves as before.

### A7. Decoy site behaves like a real site

- Upstream: HTTP/1.1 connection ends after the first response (`upstream v1.3.1:lib/src/http1_codec.rs:233`); request bodies of the decoy are sent after the response arrives.
- Fork: The connection serves the next request (`lib/src/http1_codec.rs:204`); the request body is forwarded while the response is awaited (`lib/src/reverse_proxy.rs:662`); chunk size lines and trailers are limited (`lib/src/http_forwarded_stream.rs:16`); h2 is allowed on decoy hosts (B1).
- Why: A browser that opens a page must get the same behaviour a normal web server gives, otherwise the decoy is a mark.
- Compatibility: Touches only the decoy path. Files `http_forwarded_stream.rs` and `reverse_proxy.rs` are rewritten heavily (see B2).

## B. Conflicts: upstream does otherwise, or merging needs a decision

### B1. h2 on decoy (reverse proxy) hosts

- Upstream: ALPN `h2` on a reverse proxy host is refused: only `Http1 | Http3` pass (`upstream v1.3.1:lib/src/tls_demultiplexer.rs:286`) and a test expects an error for `h2` (`upstream v1.3.1:lib/src/tls_demultiplexer.rs:571`).
- Fork: `Http1 | Http2 | Http3` are accepted and the highest the client offers is chosen (`lib/src/tls_demultiplexer.rs:286`); the test is rewritten to expect `Http2`.
- Why: A browser-shaped client offers `h2`; a site that refuses it looks unlike a normal one.
- Compatibility: Real conflict in meaning: a merge of upstream tests would fail until this test is kept as ours. Official clients on the tunnel hosts are not affected.

### B2. Heavily modified upstream files

- Upstream: Files as in `v1.3.1`.
- Fork: Against `v1.3.1` the fork changes 15 existing files and adds 30 new ones (`git diff --numstat v1.3.1 HEAD`: 45 files, +13351, -240). Largest edits of existing files: `http_forwarded_stream.rs` (+441/-44), `reverse_proxy.rs` (+345/-31), `tls_listener.rs` (+166/-41), `http1_codec.rs` (+153/-14), `core.rs` (+85/-39), `http2_codec.rs` (+101/-3). Entry points: `lib/src/core.rs:364`, `lib/src/http_downstream.rs:179`.
- Why: The decoy fixes and the new listener need hooks inside upstream code.
- Compatibility: `v1.3.1` does not touch these files, so the trial merge is clean in `lib/src`. Later upstream work on the reverse proxy or the TLS listener will conflict.

### B3. Same change ported twice (CHANGELOG, subscription tests)

- Upstream: TRUST-773 `Content-Length: 0` with tests in `lib/tests/subscription.rs` and a CHANGELOG line (upstream commit `3826388`).
- Fork: The same code change in `lib/src/http_codec.rs:191` plus own tests for CONNECT and decoy-only answers (`lib/tests/authentication.rs:445`).
- Why: The change was taken when `v1.3.1` was a candidate; the tests were extended, so the files differ.
- Compatibility: `git merge-tree HEAD v1.3.1` reports conflicts only in `CHANGELOG.md` and `lib/tests/subscription.rs`; code merges cleanly.

### B4. Deep-link tags

- Upstream: The `tt://` format defines tags `0x00` to `0x10`, `0x10` being the numeric `tls_profile` (TRUST-826) (`deeplink/src/types.rs:25`).
- Fork: The crate is upstream's, unchanged in meaning. NineLives and CatTail parameters are not carried by this crate; the subscription outside this repository carries them.
- Why: The client enumeration of TLS profiles differs from the numbers of tag `0x10` (see the client document).
- Compatibility: Parsers ignore unknown tags, so extra tags do not break upstream clients. A future upstream tag could take a number the subscription already uses.

## C. Same problem solved differently

### C1. Probing endpoints

- Upstream: Every connection gets a TLS handshake and, without valid credentials, the decoy site or an authentication error (`lib/src/http_downstream.rs:144`).
- Fork: Keeps that, adds the decoy-only list (A5) and, on the CatTail port, silence: not a single byte is written and the connection ends at a random moment (`lib/src/cattail.rs:63`).
- Why: A site answer proves that something listens; silence on a port with no service looks like a closed one.
- Compatibility: The two approaches do not touch each other: port 443 behaves as upstream plus the decoy-only list.

### C2. Keeping an idle tunnel alive

- Upstream: The endpoint has a health-check authority `_check` and waits for requests at most `client_listener_timeout` (`lib/src/http_downstream.rs:21`, `upstream v1.3.0-beta.2:lib/src/settings.rs:106`).
- Fork: The server is unchanged. The client sends `_check` at random intervals as a keepalive, because an HTTP/2 PING is not a request (`lib/src/tunnel.rs:131`).
- Why: Reuses an upstream mechanism instead of adding a server one.
- Compatibility: Works with an unmodified upstream endpoint.

## Taken from upstream, no divergence

- TRUST-773, `Content-Length: 0` on error answers: the same code change as upstream
  (`lib/src/http_codec.rs:191`).
- TRUST-826, `tls_profile` as tag `0x10` of the deep link: upstream's crate.
- TRUST-805, setup wizard in the main binaries: upstream's change.
- `client_random` prefix and auth key checks of the TLS listener: upstream
  (`lib/src/client_random_prefix.rs:5`).

## Not in 1.5.9.5

Planned for 1.5.9.10 and absent here: the full move of the client core to
upstream `v1.3.0`, and the reverse exit.
