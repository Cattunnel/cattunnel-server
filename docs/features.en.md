# CatTunnel features: the server side

[Русская версия](features.md)

This page lists what each CatTunnel feature needs from the server and which keys control it.
Key names are literal identifiers in `vpn.toml`, `hosts.toml` and `rules.toml`.

- [NineLives transport](#ninelives-transport)
- [CONNECT answer](#connect-answer)
- [ClientHello split across TLS records](#clienthello-split-across-tls-records)
- [Several server names (SNI rotation)](#several-server-names-sni-rotation)
- [TLS authorization key in client\_random](#tls-authorization-key-in-client_random)
- [Decoy site](#decoy-site)
- [Subscriptions](#subscriptions)
- [Metrics](#metrics)
- [Client-side features](#client-side-features)

```text
 incoming TCP :443
   |
   v
 TLS listener (a ClientHello split into records is joined)
   |-- SNI of the main host or allowed_sni ----+
   |-- client_random by rules.toml ------------+
   v                                            v
 HTTP/2: login from credentials.toml?    no login / a "site only"
   |-- CONNECT -> stream to the target          address: decoy site
   |     (200 with headers, data in
   |      4096-byte DATA frames)
   |-- NineLives (xhttp_enable)
   '-- subscription, metrics
```

## NineLives transport

A second transport next to `CONNECT`, enabled with `xhttp_enable = true`. Overview, keys and
key generation: [ninelives.en.md](ninelives.en.md). Wire format: [xhttp-protocol.en.md](xhttp-protocol.en.md) (in Russian).

## CONNECT answer

Upstream answered a successful `CONNECT` with a bare `200` status. This fork adds the headers
a web server would send: `date`, `server`, `cache-control: no-cache, no-store,
must-revalidate`, and `x-request-id` and `x-trace-id` with random hex values of random
length, so the answer does not have a constant size. As RFC 9110 (9.3.6) requires, the answer
has neither `content-length` nor `transfer-encoding`. There is no setting; the change is in
`lib/src/http_codec.rs`.

## ClientHello split across TLS records

Before the TLS library sees a connection, the listener reads the first record itself to get
the SNI and the `client_random` for the rules. Upstream refused a connection whose ClientHello
did not fit into the first record. This fork joins a ClientHello split across several TLS
records before parsing it, so a client may split the ClientHello into records as an anti-DPI
measure. TLS allows such a split, and the TLS library accepts it. Input that is not TLS at
all is rejected at once instead of waiting for the handshake timeout. There is no setting.

The CatTunnel client does not use the record split today; its anti-DPI modes split the
ClientHello into TCP segments and keep it in one TLS record, which any server accepts.

## Several server names (SNI rotation)

The server name in the TLS ClientHello is visible to an observer. A subscription service can
give different users different names of the same server, or change a name over time, so that
the users of one server do not all share one name. The server only has to accept every name:

```toml
# hosts.toml
[[main_hosts]]
hostname = "vpn.example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"
allowed_sni = ["cdn.example.com", "static.example.com"]
```

A connection with one of the `allowed_sni` names is served by this main host with its
certificate, so the certificate must cover all names (a wildcard or several SANs). Otherwise a
client that checks the certificate, as the CatTunnel client does, refuses the connection. The
decoy site keeps working on every name. A new name needs no rebuild; `hosts.toml` is
reloaded on `SIGHUP`.

Which user gets which name is decided by the service that writes the `tt://` links (tag
`0x03`, custom SNI). This repository does not contain such a service. A second name does not
help against a block of the server address and port; it only spreads a visible field.

## TLS authorization key in client\_random

`rules.toml` can allow connections by a fixed prefix of the TLS `client_random`. A fixed
prefix makes the start of a random field constant in every connection of a user, so an
observer can link these connections. The upstream `client_random_auth_key` replaces it: the
client derives part of `client_random` from the key, the SNI and fresh random bytes
(HKDF-SHA256 and AES-128), and the server checks it. For an observer the field stays random.

```toml
# rules.toml
[[rule]]
client_random_auth_key = "<32 hex characters>"
action = "allow"
```

Rules that only allow, with no deny rule at the end, keep the decoy site for clients without
a matching value (scanners, browsers); the tunnel itself is still protected by login and
password. A final deny rule would cut such clients off at the TLS level and make the server
easier to spot. Reference: [CONFIGURATION.md](../CONFIGURATION.md#rules-reference).

## Decoy site

Connections without valid credentials, and requests whose path matches `path_mask`, go to the
reverse proxy (`[reverse_proxy]` in `vpn.toml`, `reverse_proxy_hosts` in `hosts.toml`).
This fork fixes how HTTP/1.1 traffic passes through it: keep-alive connections serve the next
request, chunked bodies keep their framing and trailers, the request body is sent while the
response is awaited, and chunk size lines and trailers have size limits. HTTP/2 is allowed on
reverse proxy hosts.

## Subscriptions

There are two different things called a subscription.

- **Endpoint subscription (upstream).** With `[subscription]` in `vpn.toml` the endpoint
  serves each user a JSON configuration over HTTPS with HTTP Basic Auth. See
  [CONFIGURATION.md](../CONFIGURATION.md#subscription-settings).
- **CatTunnel subscription.** The CatTunnel app and router read a list of `tt://` links, one per
  line, optionally encrypted with age for the device key, with an optional `#mirrors` line.
  This format is served by a separate HTTPS service, not by this endpoint. The format is
  described in the app repository:
  [docs/subscriptions.md](https://github.com/Cattunnel/cattunnel/blob/main/docs/subscriptions.md).

## Metrics

With `[metrics]` in `vpn.toml` the endpoint serves Prometheus metrics ([METRICS.md](../METRICS.md)).
This fork adds:

- `xhttp_reserved_bytes`: bytes reserved by NineLives buffers;
- `xhttp_memory_rejections_total`: NineLives sessions or buffers refused by the memory budget;
- `GET /ja4` on the metrics listener: a JSON list of `[fingerprint, count]` pairs, the JA4
  fingerprints of incoming ClientHellos counted since the endpoint started. It shows which TLS
  profiles the clients really send. The fingerprint of each ClientHello is also logged at
  debug level.

## Client-side features

These CatTunnel client features work with any endpoint that follows the protocol. The server
needs no settings for them.

- **HTTP/2 padding.** The client sets the PADDED flag (RFC 9113, 6.1) on the first frames of
  each stream and adds random bytes, within a byte budget per stream and per session. Any
  HTTP/2 server must accept padding. Details in the app repository:
  [docs/padding.md](https://github.com/Cattunnel/cattunnel/blob/main/docs/padding.md).
- **Connection pool.** The client spreads its `CONNECT` streams over two to four TLS
  connections and opens them with pauses, not in a burst. The server sees several sessions
  of one user, which it allows by default.
- **Anti-DPI.** The client splits the ClientHello into TCP segments, may send one byte as TCP
  urgent data, or sends a segment with a small TTL that the network drops and the client OS
  resends. A lone urgent byte stalls this server: the tokio reader stops at the urgent mark and
  waits for an event that does not come. The CatTunnel client therefore always follows an
  urgent byte with a short-TTL segment, whose retransmission wakes the reader.
- **Browser TLS profiles.** The ClientHello imitates Chrome, Firefox or Safari. The TLS
  library of the server accepts all of them.