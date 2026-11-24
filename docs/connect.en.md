# CONNECT in CatTunnel: what the endpoint changes from TrustTunnel

Russian text is the main one: [connect.md](connect.md). This English translation follows it.

This document describes the CONNECT transport (HTTP/2 CONNECT over TLS) of the
CatTunnel endpoint as it stands in release 1.5.9.5. It lists what upstream
TrustTunnel does, what this fork does instead, and how the changed parts work.
It is not a history. Other protocols have their own documents:
[NineLives](ninelives.en.md) and [CatTail](cattail.en.md). All points where the fork
leaves upstream are collected in [upstream-divergence.en.md](upstream-divergence.en.md).

Base. The fork starts from upstream TrustTunnel `v1.3.0-beta.2` (merge-base
`48f424c`). The content of upstream `v1.3.1` is included (TRUST-773, TRUST-805,
TRUST-826). A trial merge of `v1.3.1` conflicts only in `CHANGELOG.md` and
`lib/tests/subscription.rs`. Client side of the same transport:
the `connect-client.md` document in the client core repository.

## 1. What CONNECT is here

A client opens TLS to the endpoint, negotiates `h2`, and sends one HTTP/2
`CONNECT host:port` request per tunneled TCP connection, with
`Proxy-Authorization: Basic`. The `200` answer turns the stream into a byte
pipe. Three authorities are special and do not open a TCP connection:
`_check` (health check), `_udp2` (UDP multiplexer), `_icmp` (ICMP multiplexer).

```text
 client                                         endpoint
   |                                               |
   |== TCP 443 ===================================>|  accept, nodelay, keepalive
   |== TLS ClientHello (SNI, ALPN h2) ===========>|  read hello, client_random
   |<=========================== ServerHello ... ==|  SNI picks the host type
   |== h2 SETTINGS ===============================>|
   |== HEADERS CONNECT example.org:443 ===========>|  stream 1, Basic auth
   |<========================== HEADERS 200 =======|  headers of section 3.1
   |== DATA ======================================>|  bytes to the destination
   |<========================================= DATA |  frames of 4096 bytes max
   |== HEADERS CONNECT _check ====================>|  keepalive (section 6)
   |<========================== HEADERS 200 =======|  stream ends at once
```

Figure 1. One CONNECT tunnel inside a TLS connection. Many such streams share
one TLS connection, and the client keeps several TLS connections (pool).

## 2. Path of a connection inside the endpoint

```text
 TCP accept (core.rs)                     CatTail listener (cattail.rs)
      |                                        |  admission, then inner bytes
      v                                        v
   +--------------------------------------------------+
   | Core::serve_stream: TlsListener::listen          |
   |  - join ClientHello split over TLS records       |
   |  - take client_random (prefix / auth key check)  |
   |  - count the JA4 fingerprint                     |
   +----------------------+---------------------------+
                          v
   +--------------------------------------------------+
   | TlsDemux::select(ALPN, SNI)                      |
   |  main host -> tunnel channel   h2 or h1 or h3    |
   |  reverse proxy host -> decoy   h1, h2 or h3      |
   |  ping / speedtest hosts -> own handlers          |
   +----------------------+---------------------------+
                          v
   +--------------------------------------------------+
   | HttpDemux::select (per request)                  |
   |  subscription, ping, speedtest, NineLives path,  |
   |  reverse proxy path, otherwise TUNNEL            |
   +----------------------+---------------------------+
                          v
   decoy-only check -> auth (Basic) -> rules -> TCP connect -> pipe
```

Figure 2. Layers between the socket and the tunnel. The same acceptor serves
the TCP listener and the inner stream of CatTail.

Code: the accept loop and `serve_stream` are in `lib/src/core.rs`, the
ClientHello reader in `lib/src/tls_listener.rs`, host selection in
`lib/src/tls_demultiplexer.rs`, request routing in
`lib/src/http_demultiplexer.rs`, the tunnel path in `lib/src/http_downstream.rs`.

## 3. Changes from upstream

| Topic | Upstream (v1.3.0-beta.2) | This fork (1.5.9.5) |
|---|---|---|
| Answer `200` to CONNECT | bare `200`, no headers | headers of a web server, random ids (3.1) |
| DATA frame size to client | whatever flow control allows | at most 4096 bytes on CONNECT streams (3.2) |
| Error answers | no `Content-Length` before v1.3.1 | `Content-Length: 0`, same change as upstream v1.3.1 |
| Clients that probed a closed port | not modelled | `decoy_only_ips_file`: VPN refused, decoy kept (3.4) |
| ClientHello over several TLS records | not joined | joined (3.5) |
| ALPN on decoy hosts | h1 and h3 only | h1, h2 and h3 |
| JA4 of clients | not collected | counted, served on the metrics listener |
| Outer layer | none | optional CatTail listener ([cattail.en.md](cattail.en.md)) |

### 3.1 Plausible 200

Upstream answers a CONNECT with an empty `200`. The headers travel inside TLS,
so a censor sees only their size, and an empty answer of the same size every
time is a mark of a tunnel. The fork answers with the set a web server sends:

```text
 HEADERS (stream N, END_HEADERS, no END_STREAM)
   :status          200
   date             <now, IMF-fixdate>
   server           nginx
   cache-control    no-cache, no-store, must-revalidate
   x-request-id     <hex of 8..40 random bytes = 16..80 characters>
   x-trace-id       <hex of 8..40 random bytes = 16..80 characters>
   (no content-length, no transfer-encoding)
```

Figure 3. Answer to a tunnel CONNECT.

The length of each id is drawn again for every answer, so the answer is not a
new constant size. RFC 9110 section 9.3.6 forbids `content-length` and
`transfer-encoding` in a 2xx answer to CONNECT; a client would count tunnel
data against `content-length`. The `_check` request gets the plain upstream
`200` with the stream ended, not this set.

### 3.2 DATA frames of 4096 bytes

A browser download over HTTP/2 arrives in small DATA frames. The upstream
write path fills the window the h2 stack allows (up to the 16384 byte frame
size). For a stream whose method is CONNECT the fork cuts each write to
`min(window, 4096)`; other methods (decoy, subscription) keep the full size.

```text
 upstream:  [ DATA 16384 ][ DATA 16384 ][ DATA 16384 ] ...
 fork:      [4096][4096][4096][4096][4096][4096][4096] ...   (CONNECT only)
```

Figure 4. Server to client DATA frame sizes for a CONNECT download.

### 3.3 Errors of zero length

Every refusal sent through `send_bad_response` carries `Content-Length: 0`
and an empty body (HTTP/1.1, HTTP/2). This is the change of upstream
TRUST-773; the fork carries the same code and adds tests for CONNECT, decoy-only
and wrong-password answers. The status of a failed authentication is
`auth_failure_status_code` (default 407, allowed 407, 405, 404, 403);
`non_connect_auth_failure_status_code` may set another for non-CONNECT requests.

### 3.4 Decoy site and decoy_only_ips_file

Connections to a reverse proxy host (chosen by SNI) and, on a main host, requests
whose path starts with `reverse_proxy.path_mask`, go to the reverse proxy of
the decoy site, so the endpoint answers like a normal site.
`decoy_only_ips_file` names a text file with one IP address or CIDR network per
line (`#` starts a comment). A client address from the list still gets TLS and
the decoy site, but every VPN request from it (CONNECT, NineLives, subscription,
SNI credentials) is answered as a failed authentication, even with valid
credentials. The file is re-read when its modification time changes (checked
at most every 5 seconds); a missing file means an empty list. CatTail treats such
an address with silence (see [cattail.en.md](cattail.en.md), section 3.2).

```text
 request from address A
        |
        +-- A in decoy list?  yes --> answer as failed auth (405/407...), stop
        |                      no
        v
   Basic credentials valid?   no --> failed auth answer
        | yes
        v
   rules, TCP connect, 200 + pipe
```

Figure 5. Decision for a tunnel request. The list is meant to be filled by the
host firewall after an address probed a closed port.

### 3.5 ClientHello over several records

RFC 8446 section 5.1 allows a client to fragment its ClientHello over several
TLS records. The endpoint joins handshake fragments until the whole message is
in, then reads `client_random` and computes the JA4 string, which is counted and
served at `/ja4` of the metrics listener. Upstream parsed only the first record,
so a hello fragmented over records left it waiting for bytes the client never
sends. The 1.5.9.5 client core splits the first flight at the TCP level and
rejects a TLS record split (`-r`) in its desync plans, so this joining serves
other clients and older builds; the comment in the code names CatTunnel
anti-DPI as a source of such hellos.

## 4. Padding from the client

The client may add random padding (the PADDED flag of HEADERS and DATA, RFC
9113 section 6.1) to the first frames of each stream. The endpoint has no code
for this: it relies on the `h2` crate to accept padded frames. This repository
has no test that sends padded frames; compatibility rests on the RFC.
Details are in `connect-client.md`.

## 5. Connection pool of the client

The endpoint does not coordinate the pool. The client keeps 2 to 4 TLS
connections and 24 to 48 streams per connection before it opens the next one
(client document). The endpoint sees them as ordinary h2 connections from one
address. The limit on h2 connections per client is the upstream setting
`default_max_http2_conns_per_client` (unchanged, unset by default).

## 6. Idle tunnels, keepalive and lifetime

After the last request of an h2 connection the endpoint waits for the next
request at most `client_listener_timeout_secs` (default 600 s). A request
restarts the wait, so the client sends a tiny CONNECT to `_check` at random
intervals; an HTTP/2 PING alone is not a request. Tunneled TCP connections have
`tcp_connections_timeout_secs` (default one week), UDP 300 s. The lifetime of a
TLS connection and its rotation are decided by the client; the endpoint has no
code for them. The accepted socket has TCP keepalive on.

```text
  client                                endpoint
    |-- CONNECT _check ----------------->|  wait for next request restarts
    |<--------------- 200 (END_STREAM) --|  (client_listener_timeout_secs)
    ...random pause...
    |-- CONNECT _check ----------------->|
```

Figure 6. Keepalive by a request.

## 7. What is not on the server

- Rotation of the key between servers and SNI names (client 9a): no server
  code. Each rotation server is an ordinary endpoint.
- Session lifetime, pool size, handshake spacing: client only.
- TLS profile choice (Chrome 157, Firefox 160): client only. The endpoint
  only counts the JA4 string it sees.

## 8. Compatibility

- The extra answer headers are ordinary HTTP/2 response headers and 4096 byte
  DATA frames are within the h2 limits. This repository has no test with an
  official TrustTunnel client; this point is not tested here.
- A client from an address in `decoy_only_ips_file` is refused whatever
  its credentials are.
- Config keys added by the fork have defaults that keep upstream behaviour
  (`decoy_only_ips_file` unset, `cattail` unset, `xhttp_enable = false`).
