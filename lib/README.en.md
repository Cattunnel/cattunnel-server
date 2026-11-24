Русский: [README.md](README.md). The Russian text is the main one; where the two differ, the Russian text is correct.

# The trusttunnel library

The `trusttunnel` crate (the `lib` directory) holds everything the CatTunnel server does: accepting TLS, parsing HTTP/1, HTTP/2 and HTTP/3, tunnels, forwarding to targets, the reverse proxy, credential checks, metrics, and in the fork also NineLives, CatTail and the "site only" list. The `trusttunnel_endpoint` binary in the `endpoint` directory is a thin wrapper over this library. The crate name comes from TrustTunnel, see the [root README](../README.en.md).

This document describes what the code does. Where it says "as in TrustTunnel", the behavior did not change; see [upstream](https://github.com/TrustTunnel/TrustTunnel) for details.

## Contents

- [Building](#building)
- [Traffic forwarding](#traffic-forwarding)
- [How a connection reaches a channel](#how-a-connection-reaches-a-channel)
- [Reverse proxy](#reverse-proxy)
- [Authentication](#authentication)
- [Metrics](#metrics)
- [What the fork adds](#what-the-fork-adds)
- [License](#license)

## Building

You need:

- Rust 1.95: the version is pinned in `rust-toolchain.toml`, and rustup picks it up on its own;
- CMake 3.31.6 or newer;
- libclang 9.0 or newer;
- a C++ compiler.

Commands from the repository root:

```shell
cargo build            # debug build
cargo build --release  # release build
```

More about the environment and tests: [DEVELOPMENT.en.md](../DEVELOPMENT.en.md).

## Traffic forwarding

The library accepts client connections wrapped in HTTP/1, HTTP/2 or HTTP/3 (over QUIC). `Settings.listen_protocols` sets which codecs are on (`http1`, `http2`, `quic`). `Settings.forward_protocol` (`ForwardProtocolSettings`) sets where the unwrapped connection goes:

- `Direct` (default): straight to the target host;
- `Socks5`: through a SOCKS5 proxy.

### ICMP forwarding

An optional feature: the server forwards ICMP packets of a client. It is enabled by the `Settings.icmp` section (`IcmpSettings`):

| Key | Default | Meaning |
| --- | --- | --- |
| `interface_name` | `eth0` on Linux, `en0` on other systems | The interface the ICMP socket binds to |
| `request_timeout_secs` | `3` | ICMP request timeout |
| `recv_message_queue_capacity` | `256` | Size of the received message queue; each client has its own |

A smaller queue may drop packets if the multiplexer cannot keep up, a larger one uses more memory. Some systems need superuser rights.

## How a connection reaches a channel

A channel is chosen in two steps. First by the SNI name of the TLS connection, then, for the main host, by the path of the request.

```text
 TLS connection, SNI name
   |
   +-- main host (main_hosts) ----------------------> tunnel channel
   +-- host from reverse_proxy_hosts (if there is
   |   [reverse_proxy]) ----------------------------> reverse proxy
   +-- host from ping_hosts ------------------------> ping
   +-- host from speedtest_hosts -------------------> speed test
   +-- <data>.<main host> --------------------------> tunnel channel, SNI login
   +-- a name from allowed_sni of the main host ----> tunnel channel
   '-- any other name ------------------------------> refused

 request on the tunnel channel, checks in order
   subscription path (exact match) -----------------> subscription
   ping_path, if ping_enable -----------------------> ping
   speedtest_path, if speedtest_enable -------------> speed test
   xhttp_path, if xhttp_enable ---------------------> NineLives
   path_mask, if there is [reverse_proxy] ----------> reverse proxy
   everything else ---------------------------------> tunnel
```

Figure 1. Channel choice: first by SNI, then by path.

`ping_path`, `speedtest_path` and `xhttp_path` are compared by the start of the path, the subscription path by exact match. Defaults: `ping_path = "/ping"`, `speedtest_path = "/speedtest"`, `xhttp_path = "/api/v2/"`, `ping_enable = false`, `speedtest_enable = false`, `xhttp_enable = false`.

## Reverse proxy

A request goes to the reverse proxy in two cases:

1. The SNI equals one of the hosts of `TlsHostsSettings.reverse_proxy_hosts`.
2. The request came to the main host, its path starts with `ReverseProxySettings.path_mask`, and it did not match ping, speed test, NineLives or subscription (the order is in figure 1).

The server ends TLS itself and translates the HTTP/x request into HTTP/1.1 towards the origin server (`ReverseProxySettings.server_address`), and the response back into the original HTTP/x:

```text
 (client) TLS, HTTP/x <---> (CatTunnel server) <---> (origin server) HTTP/1.1
```

Figure 2. The reverse proxy translates HTTP/x into HTTP/1.1 and back.

Translation rules:

- The request gets an `X-Original-Protocol` header with the value `HTTP1`, `HTTP2` or `HTTP3`.
- For every request the server opens a new connection to the origin server and sets `Connection: close`.
- The request body is sent while the server waits for the response. The socket to the origin server is not closed for writing after the body: the origin knows the end of the body by the length or by the last chunk, and a half-closed connection would make it drop the response.
- With `h3_backward_compatibility` on, an HTTP/3 request with the `GET` method and the path `/` goes to the origin server as `CONNECT`.
- Keys of the `[reverse_proxy]` section: `server_address`, `path_mask` (starts with `/`), `h3_backward_compatibility`.

## Authentication

The list of users comes from the `credentials.toml` file (the `credentials_file` key in `vpn.toml`):

```toml
[[client]]
username = "a"
password = "b"
```

A client may have the optional `max_http2_conns` and `max_http3_conns`: the limit of simultaneous HTTP/1 and HTTP/2 connections and of HTTP/3 connections for this client. The server builds a `RegistryBasedAuthenticator` from this list and passes it to `Core::new`. With no users there is no credential check; the binary warns about this at start when it listens on a local address.

You can plug in your own check by implementing the `authentication::Authenticator` trait (methods `authenticate` and `username`) and passing it to `Core::new`. The source of the credentials is called `authentication::Source`: `Sni` or `ProxyBasic`.

### How a client logs in

**By SNI.** The client connects with the name `data.main_host_name`. The first label (up to the first dot) must equal the base64 of `username:password`. The server compares the label with a list of such values built from `credentials.toml`.

**By proxy header.** The client sends `Proxy-Authorization: Basic base64(username:password)`.

### SOCKS5 authentication

When forwarding goes through SOCKS5 (`forward_protocol.socks5`), the server does not check the credentials itself and passes them to the proxy.

**Standard.** With `extended_auth = false` (the default) the standard procedure of [RFC 1929](https://datatracker.ietf.org/doc/html/rfc1929) runs:

- SNI login: the same first SNI label is both the user name and the password;
- header login: `username` and `password` are taken from the base64 value, split at the first colon.

**Extended.** With `extended_auth = true` the method `0x80` is used. After the proxy selects it, the server sends a request in this form:

```text
+-----+-----------+-----+--------+
| VER |   EXT(0)  |     | EXT(n) |
+-----+-----------+ ... +--------+
|  1  | see below |     |        |
+-----+-----------+-----+--------+
```

Figure 3. The SOCKS5 extended authentication request.

`VER` is `0x01`. Each extension looks like this:

```text
+------+--------+----------+
| TYPE | LENGTH |  VALUE   |
+------+--------+----------+
|  1   |   2    | variable |
+------+--------+----------+
```

Figure 4. One extension (the length is in network byte order).

Extension types:

| Type | Name | Value |
| --- | --- | --- |
| `0x00` | `TERM` | Length 0; ends the message, required |
| `0x01` | `DOMAIN` | UTF-8 string: the SNI name the client used for TLS |
| `0x02` | `CLIENT_ADDRESS` | 4 or 16 bytes: the public IP address of the client |
| `0x03` | `USER_AGENT` | UTF-8 string: the client `User-Agent` (only when present) |
| `0x04` | `PROXY_AUTH` | A base64 string: the `Proxy-Authorization` value without the word `Basic` |
| `0x05` | `SNI_AUTH` | Length 0: the client logs in by SNI |

`PROXY_AUTH` and `SNI_AUTH` never come in the same message. The proxy answers in the standard way, as in [RFC 1929](https://datatracker.ietf.org/doc/html/rfc1929#section-2).

## Metrics

To collect metrics, enable the listener with the `Settings.metrics` section (`MetricsSettings`). It listens on `MetricsSettings.address` over plain HTTP/1 and serves these paths:

- `/health-check`: a check that the server is alive, answer `200 OK`;
- `/metrics`: values in the [Prometheus](https://prometheus.io/) format;
- `/clients`: JSON per user, only with `per_client_metrics`;
- `/ja4`: a counter of ClientHello fingerprints (added by the fork).

The list of metrics and examples: [METRICS.md](../METRICS.en.md).

## What the fork adds

The fork added the following to the library. Each item has its own document.

- NineLives, the second transport made of a pair of HTTP requests: [docs/ninelives.en.md](../docs/ninelives.en.md).
- CatTail, an optional outer layer on a separate port: [docs/cattail.en.md](../docs/cattail.en.md).
- A `CONNECT` response with the headers of an ordinary web server and data frames of 4096 bytes: [docs/connect.en.md](../docs/connect.en.md).
- Joining a ClientHello from several TLS records, JA4 fingerprint counting, a list of "site only" addresses (`decoy_only_ips_file`), decoy site fixes: [docs/features.en.md](../docs/features.en.md).
- The full list of differences from TrustTunnel: [docs/upstream-divergence.en.md](../docs/upstream-divergence.en.md).

## License

Apache 2.0.
