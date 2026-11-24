Русский: [METRICS.md](METRICS.md). The Russian text is the main one; where the two differ, the Russian text is correct.

# Server metrics

The CatTunnel server (`trusttunnel_endpoint`) exposes Prometheus metrics over plain HTTP/1, without TLS. The metrics listener is off until `vpn.toml` has a `[metrics]` section. The metrics inherited from TrustTunnel work as in [upstream](https://github.com/TrustTunnel/TrustTunnel/blob/v1.3.1/METRICS.md). The fork adds two memory budget metrics for NineLives and CatTail (`xhttp_reserved_bytes`, `xhttp_memory_rejections_total`) and a `/ja4` path with a counter of ClientHello fingerprints.

## Contents

- [Enabling](#enabling)
- [Listener paths](#listener-paths)
- [Metrics](#metrics)
- [NineLives and CatTail memory budget](#ninelives-and-cattail-memory-budget)
- [Per-user metrics and /clients](#per-user-metrics-and-clients)
- [ClientHello fingerprints: /ja4](#clienthello-fingerprints-ja4)
- [Using the library](#using-the-library)
- [When metrics are missing or odd](#when-metrics-are-missing-or-odd)
- [See also](#see-also)

## Enabling

Add a section to `vpn.toml`:

```toml
[metrics]
address = "127.0.0.1:1987"
request_timeout_secs = 3
per_client_metrics = false
```

The values in the example are the defaults. If the section exists but a key is missing, the same defaults apply.

| Key | Default | Meaning |
| --- | --- | --- |
| `address` | `127.0.0.1:1987` | Address and port of the metrics listener |
| `request_timeout_secs` | `3` | How many seconds the listener waits for a request after a connection |
| `per_client_metrics` | `false` | Turns on the `*_per_user` series and the `/clients` path |

The listener checks neither a login nor the request method: anyone who can reach the port sees all metrics. Keep `address` on `127.0.0.1` or close the port in the firewall. With `per_client_metrics = true` the metrics carry user names, and `/clients` also shows client IP addresses, so protect the listener with particular care.

If the port is taken or the address cannot be opened, the server exits with a `Metrics listener failure` error instead of running without metrics.

## Listener paths

| Path | Response |
| --- | --- |
| `/metrics` | Metrics in the Prometheus text format |
| `/clients` | JSON per user; only with `per_client_metrics = true`, otherwise `404` |
| `/ja4` | JSON with the number of ClientHello messages per JA4 fingerprint (added by the fork) |
| `/health-check` | `200 OK` while the server runs |

Any other path gets `400 Bad Request`.

```text
 client connection (CONNECT)             metrics listener, 127.0.0.1:1987
   session opened ----> client_sessions       GET /metrics ------> Prometheus text
   bytes from client -> inbound_traffic_bytes GET /clients ------> JSON per user
   bytes to client ---> outbound_traffic_bytes GET /ja4 ---------> JSON per fingerprint
   exit to target ----> outbound_tcp_sockets  GET /health-check -> 200 OK
                        outbound_udp_sockets
 TLS connection over TCP
   ClientHello -------> JA4 counter
 NineLives and CatTail
   buffer reserved ---> xhttp_reserved_bytes
   reservation refused > xhttp_memory_rejections_total
```

Figure 1. Where the values come from and which paths the listener serves.

## Metrics

| Name | Type | Labels | What it counts |
| --- | --- | --- | --- |
| `client_sessions` | gauge | `protocol_type` | Open client sessions |
| `inbound_traffic_bytes` | counter | `protocol_type` | Bytes uploaded by clients (client, server, target) |
| `outbound_traffic_bytes` | counter | `protocol_type` | Bytes downloaded by clients (target, server, client) |
| `outbound_tcp_sockets` | gauge | none | Outbound TCP connections from the server to targets |
| `outbound_udp_sockets` | gauge | none | Outbound UDP sockets of the server |
| `xhttp_reserved_bytes` | gauge | none | Used part of the NineLives and CatTail memory budget, in bytes (fork) |
| `xhttp_memory_rejections_total` | counter | none | Refused reservations from this budget (fork) |
| `client_sessions_per_user` | gauge | `username`, `protocol_type` | Sessions per user, only with `per_client_metrics` |
| `inbound_traffic_bytes_per_user` | counter | `username` | Upload per user, only with `per_client_metrics` |
| `outbound_traffic_bytes_per_user` | counter | `username` | Download per user, only with `per_client_metrics` |

The `protocol_type` label takes the values `HTTP1`, `HTTP2` and `HTTP3` (uppercase).

An example `/metrics` response without `per_client_metrics` (the numbers are made up):

```console
# HELP client_sessions Number of active client sessions
# TYPE client_sessions gauge
client_sessions{protocol_type="HTTP1"} 5
client_sessions{protocol_type="HTTP2"} 3

# HELP inbound_traffic_bytes Total number of bytes uploaded by clients
# TYPE inbound_traffic_bytes counter
inbound_traffic_bytes{protocol_type="HTTP2"} 1234567

# HELP outbound_traffic_bytes Total number of bytes downloaded by clients
# TYPE outbound_traffic_bytes counter
outbound_traffic_bytes{protocol_type="HTTP2"} 7654321

# HELP outbound_tcp_sockets Number of active outbound TCP connections
# TYPE outbound_tcp_sockets gauge
outbound_tcp_sockets 12

# HELP outbound_udp_sockets Number of active outbound UDP sockets
# TYPE outbound_udp_sockets gauge
outbound_udp_sockets 8

# HELP xhttp_reserved_bytes Reserved XHTTP session buffer budget
# TYPE xhttp_reserved_bytes gauge
xhttp_reserved_bytes 786432

# HELP xhttp_memory_rejections_total XHTTP sessions refused by the memory budget
# TYPE xhttp_memory_rejections_total counter
xhttp_memory_rejections_total 0
```

Series with labels appear after the first event: until a session has been opened, `client_sessions` is absent from the response. The sockets and the two memory metrics are always there because they have no labels.

How the values are counted:

- `client_sessions`, `inbound_traffic_bytes` and `outbound_traffic_bytes` belong to the CONNECT tunnel. A session is counted from the opening of the tunnel connection to its close. NineLives bytes do not reach these two counters: `lib/src/http_xhttp.rs` has no calls that count them.
- `outbound_tcp_sockets` goes up when the attempt to reach the target starts, before the connection is established, and goes down when it closes or fails. In SOCKS5 mode it goes up when the proxy has answered a CONNECT with success.
- `outbound_udp_sockets` equals the number of source and destination entries with direct forwarding, and the number of UDP associations (one per source) with SOCKS5.

## NineLives and CatTail memory budget

NineLives session buffers and CatTail connection buffers come from one shared budget. Its size is the `xhttp_memory_limit` key in `vpn.toml`. Without the key the server takes a quarter of the smaller of physical memory and the cgroup limit, but not less than 64 MiB and not more than 512 MiB; when the memory size cannot be determined, the budget is 64 MiB.

Every NineLives session reserves 256 KiB at once and asks for more as its read buffer grows. A CatTail connection reserves a fixed amount for its lifetime. Details: [docs/ninelives.en.md](docs/ninelives.en.md) and [docs/cattail.en.md](docs/cattail.en.md).

```text
 request to reserve N bytes (NineLives session, read buffer growth, CatTail connection)
   |
   v
 already used + N <= xhttp_memory_limit ?
   |-- yes --> xhttp_reserved_bytes += N
   |           on release: xhttp_reserved_bytes -= N
   '-- no  --> xhttp_memory_rejections_total += 1
                new NineLives session: answer 503
                CatTail connection: silence, no bytes sent back
                read buffer growth: error for that session
```

Figure 2. One reservation from the memory budget and the two metrics it changes.

`xhttp_reserved_bytes` shows how much of the budget is used now. If it stays near `xhttp_memory_limit` while `xhttp_memory_rejections_total` grows, the budget is too small for the load: raise `xhttp_memory_limit`. The counter goes up on every failed attempt, so one overloaded session can add several units.

## Per-user metrics and /clients

The `per_client_metrics` option (default `false`) adds three `*_per_user` series with the `username` label and the `/clients` path. While the user name is not known yet (for example, a tunnel with a `Proxy-Authorization` header before its first request), the `username` label is empty.

With `per_client_metrics = true` the `/metrics` response also contains:

```console
# HELP client_sessions_per_user Number of active client sessions per user
# TYPE client_sessions_per_user gauge
client_sessions_per_user{protocol_type="HTTP2",username="alice"} 1

# HELP inbound_traffic_bytes_per_user Total number of bytes uploaded per user
# TYPE inbound_traffic_bytes_per_user counter
inbound_traffic_bytes_per_user{username="alice"} 1234567

# HELP outbound_traffic_bytes_per_user Total number of bytes downloaded per user
# TYPE outbound_traffic_bytes_per_user counter
outbound_traffic_bytes_per_user{username="alice"} 7654321
```

`/clients` returns a JSON list:

```json
[{"username":"alice","ip":"203.0.113.7","sessions":3,"inbound":123456,"outbound":789012},
 {"username":"bob","ip":null,"sessions":0,"inbound":0,"outbound":0}]
```

- The list contains every user from `credentials.toml` and everyone who has a connection now. A user without sessions stays in the list with zero counters, so monitoring can show unused accounts.
- `inbound` and `outbound` are totals since the server started, `sessions` is the current number of sessions.
- `ip` is one of the addresses the user is connected from; with several addresses the choice is arbitrary. `null` means the address is unknown: this happens for a configured user without connections and for HTTP/3 when the client address could not be determined.

## ClientHello fingerprints: /ja4

The server computes a JA4 fingerprint for every ClientHello that arrives over TCP and keeps a count per fingerprint. It shows which TLS profiles clients really send: a browser, an old phone, the CatTunnel app. Each fingerprint is also written to the log at `debug` level. Only TCP connections are counted: QUIC is not part of this count.

`/ja4` returns a JSON list of `[fingerprint, count]` pairs sorted by fingerprint. The count starts at server start and is not kept across restarts:

```json
[["t13d1516h2_000000000000_000000000000",41],["t13d1517h2_111111111111_222222222222",7]]
```

A fingerprint has three parts. The prefix `t13d1516h2` reads as follows: `t` (TCP), TLS version `13`, `d` when the ClientHello has an SNI (otherwise `i`), number of cipher suites `15`, number of extensions `16`, first and last letter of the first ALPN `h2`. Two SHA-256 hashes cut to 12 characters follow. Format description: [JA4 by FoxIO](https://github.com/FoxIO-LLC/ja4). The values in the example are made up.

## Using the library

If you embed the `trusttunnel` crate (the `lib` directory), the listener is enabled by the `Settings::metrics` setting. The fields of `MetricsSettings` are private, so the settings are built with the builder:

```rust
use std::time::Duration;
use trusttunnel::settings::{MetricsSettings, Settings};

let metrics = MetricsSettings::builder()
    .listen_address("127.0.0.1:1987")
    .unwrap()
    .request_timeout(Duration::from_secs(3))
    .per_client_metrics(false)
    .build()
    .unwrap();

let settings = Settings::builder()
    // the other server settings
    .metrics(metrics)
    .build()
    .unwrap();
```

## When metrics are missing or odd

- The port does not answer: check that `vpn.toml` has a `[metrics]` section and that you connect to the same `address`. A busy port gives a `Metrics listener failure` error at start, so read the server log.
- Answer `400`: the path is not in the table above.
- Answer `404` on `/clients`: `per_client_metrics` is off.
- No `client_sessions` and no traffic counters: no CONNECT session has happened yet, and NineLives does not change these counters.
- No `*_per_user` series: turn on `per_client_metrics` and wait for the first session.
- `xhttp_memory_rejections_total` grows: the memory budget is exhausted, see the section above.

## See also

- [CONFIGURATION.en.md](CONFIGURATION.en.md): server settings reference
- [PROTOCOL.en.md](PROTOCOL.en.md): protocols
- [DEVELOPMENT.en.md](DEVELOPMENT.en.md): build and tests
- [docs/features.en.md](docs/features.en.md): fork features and keys
- [docs/upstream-divergence.en.md](docs/upstream-divergence.en.md): how the fork differs from TrustTunnel
