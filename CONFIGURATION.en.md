# CatTunnel Endpoint Configuration

Русский: [CONFIGURATION.md](CONFIGURATION.md) (the main text; if the two differ, the Russian one is correct)

This document describes the configuration files and command line arguments of the CatTunnel server (the `trusttunnel_endpoint` binary). The server is a fork of the TrustTunnel endpoint. Anything not marked "added in the fork" works as in [TrustTunnel](https://github.com/TrustTunnel/TrustTunnel/blob/master/CONFIGURATION.md). The keys of the fork are collected in [Fork keys](#fork-keys). How the fork differs from upstream in general: [docs/upstream-divergence.en.md](docs/upstream-divergence.en.md).

CatTunnel repositories: [cattunnel](https://github.com/Cattunnel/cattunnel) (app), [cattunnel-router](https://github.com/Cattunnel/cattunnel-router) (router), [cattunnel-server](https://github.com/Cattunnel/cattunnel-server) (this server).

## Table of Contents

- [Overview](#overview)
- [Command Line Arguments](#command-line-arguments)
- [Configuration Files](#configuration-files)
- [Settings Reference](#settings-reference)
- [Fork keys](#fork-keys)
- [TLS Hosts Reference](#tls-hosts-reference)
- [Rules Reference](#rules-reference)
- [Runtime](#runtime)

---

## Overview

The configuration is split into TOML files:

1. **Main settings file** (`vpn.toml`): timeouts, protocols, transports.
2. **TLS hosts file** (`hosts.toml`): certificates and hostnames.
3. **Credentials file** (`credentials.toml`): logins and passwords.
4. **Rules file** (`rules.toml`): connection filter.

The main file and the TLS hosts file are passed as arguments. The credentials and rules are read from the paths `credentials_file` and `rules_file` in the main file.

The setup wizard (`trusttunnel_endpoint --setup-wizard`) generates these files. It does not write the keys of the fork (`xhttp_*`, `[cattail]`, `decoy_only_ips_file`): add them to `vpn.toml` by hand after the wizard. In non-interactive mode the wizard overwrites existing files without asking.

Installing the server from a Cattunnel release or building it: [README.en.md](README.en.md).

---

## Command Line Arguments

| Argument | Short | Description | Default |
| --- | --- | --- | --- |
| `--version` | `-v` | Print version and exit | none |
| `--setup-wizard` | none | Run the setup wizard; must be the first argument | none |
| `--loglvl` | `-l` | Log level: `info`, `debug`, `trace` | `info` |
| `--logfile` | none | Log file; without it logs go to stdout | stdout |
| `--sentry_dsn` | none | Sentry DSN for error reports | none |
| `--jobs` | none | Number of worker threads | CPU count |
| `<settings>` | none | **Required.** Path to the main settings file | none |
| `<tls_hosts_settings>` | none | **Required.** Path to the TLS hosts file; `SIGHUP` reloads it | none |
| `--client_config` | `-c` | Print the config of the named client and exit | none |
| `--address` | `-a` | Server address for the client config (requires `-c`): `ip`, `ip:port`, `domain` or `domain:port`. May be repeated. Optional when `[subscription]` is enabled: the subscription address is used | none |
| `--custom-sni` | `-s` | Override the client SNI; must match a main host `hostname` or `allowed_sni` | none |
| `--client-random-prefix` | `-r` | Explicit `client_random_prefix` for the client. `rules.toml` must contain a matching `allow` rule | none |
| `--generate-client-random-prefix` | none | Generate a new `client_random_prefix`, append an `allow` rule to `rules.toml` and use it. Conflicts with `-r`. Needs `rules_file` in the settings | none |
| `--prefix-length` | none | Length of the generated prefix in bytes (with `--generate-client-random-prefix`) | `4` |
| `--prefix-percent` | none | Share of one bits in the mask (with `--generate-client-random-prefix`) | `70` |
| `--prefix-mask` | none | Explicit hex mask; conflicts with `--prefix-length` and `--prefix-percent` | none |
| `--client-random-auth-key` | none | Hex key for the SNI-derived `client_random`. Conflicts with `-r` and `--generate-client-random-prefix`. `rules.toml` must contain a rule with this key and action `allow` | none |
| `--format` | `-f` | Output format: `deeplink` (a `tt://?` link) or `toml` | `deeplink` |
| `--name` | `-n` | Server name for the client | none |
| `--dns-upstream` | `-d` | DNS upstream for the client; may be repeated | none |
| `--subscription-url` | none | Override the subscription URL (host and path only); credentials come from `credentials.toml` | none |
| `--subscription-only` | none | Print a minimal link with the subscription URL only. Needs `--format deeplink`, an enabled `[subscription]` and a subscription host certificate verifiable by the system CAs | none |

The exported client config and link do not set `tls_profile`: the client uses `chrome` unless the profile is set separately. Link format: [DEEP_LINK.en.md](DEEP_LINK.en.md).

After the link the server prints the address of a QR code page on `trusttunnel.org` (an upstream page). The CatTunnel app also reads the link from text, so the page is optional.

The link carries no NineLives or CatTail settings. They reach the client through the subscription or link tags: [docs/ninelives.en.md](docs/ninelives.en.md), [docs/cattail.en.md](docs/cattail.en.md).

### Examples

```bash
# Start the endpoint
./trusttunnel_endpoint vpn.toml hosts.toml

# Debug logging
./trusttunnel_endpoint vpn.toml hosts.toml -l debug

# Log to a file
./trusttunnel_endpoint vpn.toml hosts.toml --logfile /var/log/trusttunnel.log

# Client config, address by IP
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a 203.0.113.1

# Address with a port
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a 203.0.113.1:443

# Address by name
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com

# A TOML file instead of a link
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com --format toml

# Explicit client_random_prefix
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --client-random-prefix a0b0/f0f0

# New client_random_prefix with a rule in rules.toml
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --generate-client-random-prefix

# New prefix with a given mask
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --generate-client-random-prefix --prefix-mask aaaa7777

# Key for the SNI-derived client random
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --client-random-auth-key 00112233445566778899aabbccddeeff
```

### Setup wizard

`--setup-wizard` is the first argument. Without options it asks questions. Non-interactive mode:

```bash
sudo ./trusttunnel_endpoint --setup-wizard -m non-interactive \
    -a 0.0.0.0:443 -c admin:secretpass -n vpn.example.com \
    --lib-settings vpn.toml --hosts-settings hosts.toml
```

| Option | Short | Description |
| --- | --- | --- |
| `--mode` | `-m` | `interactive` (default) or `non-interactive` |
| `--address` | `-a` | Listen address; required in non-interactive mode |
| `--creds` | `-c` | `username:password`; required in non-interactive mode |
| `--hostname` | `-n` | Certificate hostname; required in non-interactive mode |
| `--lib-settings` | none | Where to write the main file; required in non-interactive mode |
| `--hosts-settings` | none | Where to write the TLS hosts file; required in non-interactive mode |
| `--cert-type` | none | `self-signed`, `letsencrypt` or `provided` |
| `--cert-chain-path`, `--cert-key-path` | none | Existing certificate and key; required for `provided` |
| `--acme-email` | none | Let's Encrypt account email; required for `letsencrypt` |
| `--acme-challenge` | none | ACME challenge; only `http-01` (default) outside interactive mode |
| `--acme-staging` | none | Let's Encrypt staging environment |

The wizard help (`-h`) points to the upstream documentation, not to CatTunnel.

---

## Configuration Files

### Main Settings File (vpn.toml)

> If the server listens on port 443 directly, use `listen_address = "0.0.0.0:443"`. If Docker maps host port 443 to container port 8443, use `listen_address = "0.0.0.0:8443"` inside `vpn.toml`.

```toml
# The address to listen on
listen_address = "0.0.0.0:443"

# Whether IPv6 connections can be routed
ipv6_available = true

# Whether clients may reach the private network of the endpoint.
# Applies to client traffic only; the reverse proxy server_address is always reachable.
allow_private_network_connections = false

# Timeout of an incoming TLS handshake (seconds)
tls_handshake_timeout_secs = 10

# Timeout of a client listener (seconds)
client_listener_timeout_secs = 600

# Timeout of outgoing connection establishment (seconds)
connection_establishment_timeout_secs = 30

# Idle timeout of tunneled TCP connections (seconds)
tcp_connections_timeout_secs = 604800

# Timeout of tunneled UDP "connections" (seconds)
udp_connections_timeout_secs = 300

# Credentials file
credentials_file = "credentials.toml"

# Rules file (optional)
rules_file = "rules.toml"

# Added in the fork (optional)
# decoy_only_ips_file = "decoy-only.txt"

[listen_protocols]

[listen_protocols.http1]
upload_buffer_size = 32768

[listen_protocols.http2]
initial_connection_window_size = 8388608
initial_stream_window_size = 131072
max_concurrent_streams = 1000
max_frame_size = 16384
header_table_size = 65536

[listen_protocols.quic]
recv_udp_payload_size = 1350
send_udp_payload_size = 1350
initial_max_data = 104857600
initial_max_stream_data_bidi_local = 1048576
initial_max_stream_data_bidi_remote = 1048576
initial_max_stream_data_uni = 1048576
initial_max_streams_bidi = 4096
initial_max_streams_uni = 4096
max_connection_window = 25165824
max_stream_window = 16777216
disable_active_migration = true
enable_early_data = true
message_queue_capacity = 4096

# Forward protocol (optional, defaults to direct)
[forward_protocol]
direct = {}

# Reverse proxy (optional)
# [reverse_proxy]
# server_address = "127.0.0.1:8080"
# path_mask = "/api"
# h3_backward_compatibility = false

# ICMP (optional, some systems need superuser)
# [icmp]
# interface_name = "eth0"
# request_timeout_secs = 3
# recv_message_queue_capacity = 256

# Metrics (optional)
# [metrics]
# address = "127.0.0.1:1987"
# request_timeout_secs = 3
# per_client_metrics = false
```

### TLS Hosts Settings File (hosts.toml)

```toml
# Main hosts: tunneling and service requests
[[main_hosts]]
hostname = "vpn.example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"
# Extra names served with this host certificate (optional)
allowed_sni = ["alt.example.com"]

# Ping hosts for HTTPS health checks (optional)
[[ping_hosts]]
hostname = "ping.vpn.example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"

# Speed test hosts (optional)
[[speedtest_hosts]]
hostname = "speed.vpn.example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"

# Reverse proxy hosts (needs [reverse_proxy] in the main file)
# [[reverse_proxy_hosts]]
# hostname = "api.example.com"
# cert_chain_path = "certs/cert.pem"
# private_key_path = "certs/key.pem"
```

### Credentials File (credentials.toml)

```toml
[[client]]
username = "user1"
password = "secure_password_1"

[[client]]
username = "user2"
password = "secure_password_2"
# Optional: connection limits of this user
# max_http2_conns = 8
# max_http3_conns = 1
```

### Rules File (rules.toml)

```toml
# Rules are evaluated in order, the first matching rule applies.
# If no rule matches, the connection is allowed.

# Deny an IP range
[[rule]]
cidr = "192.168.1.0/24"
action = "deny"

# Allow connections with a TLS client random prefix
[[rule]]
client_random_prefix = "aabbcc"
action = "allow"

# Deny when both the address and the client random (with mask) match
[[rule]]
cidr = "10.0.0.0/8"
client_random_prefix = "a0b0/f0f0"
action = "deny"
```

---

## Settings Reference

### Core Settings

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `listen_address` | String | `0.0.0.0:443` | Address and port to listen on |
| `ipv6_available` | Boolean | `true` | Whether IPv6 connections can be routed |
| `allow_private_network_connections` | Boolean | `false` | Let clients reach the private network of the endpoint |
| `tls_handshake_timeout_secs` | Integer | `10` | TLS handshake timeout, seconds |
| `client_listener_timeout_secs` | Integer | `600` | Client listener timeout, seconds |
| `connection_establishment_timeout_secs` | Integer | `30` | Outgoing connection timeout, seconds |
| `tcp_connections_timeout_secs` | Integer | `604800` | Idle TCP timeout (1 week) |
| `udp_connections_timeout_secs` | Integer | `300` | UDP timeout (5 minutes) |
| `credentials_file` | String | none | Path to the credentials file |
| `rules_file` | String | none | Path to the rules file. Missing or unreadable file: everything is allowed |
| `speedtest_enable` | Boolean | `false` | Speed test handler on main hosts |
| `speedtest_path` | String | `/speedtest` | Path prefix of the speed test |
| `ping_enable` | Boolean | `false` | Ping handler on main hosts |
| `ping_path` | String | `/ping` | Path prefix of ping |
| `auth_failure_status_code` | Integer | `407` | Status code on an authentication failure for `CONNECT` |
| `non_connect_auth_failure_status_code` | Integer | none | The same for non-`CONNECT` requests; if not set, `auth_failure_status_code` is used |
| `default_max_http2_conns_per_client` | Integer | none | Limit of simultaneous HTTP/1 and HTTP/2 connections per user. A client opens 8 HTTP/2 connections by default. Not set: unlimited. Overridden by `max_http2_conns` in `credentials.toml` |
| `default_max_http3_conns_per_client` | Integer | none | Limit of HTTP/3 connections per user. Not set: unlimited. Overridden by `max_http3_conns` |

Paths start with `/`. The server refuses to start if one path equals another or is its prefix in these pairs: `ping_path` and `speedtest_path`, `ping_path` and `xhttp_path`, `speedtest_path` and `xhttp_path`, the subscription path and each of `ping_path`, `speedtest_path`, `path_mask`.

`auth_failure_status_code` and `non_connect_auth_failure_status_code` accept `407`, `405`, `404` or `403`. A value other than `407` breaks proxy authentication in Chrome.

Without users the server does not start on a public address; on a loopback address it starts with a warning in the log.

### Listen Protocol Settings

At least one protocol must be enabled.

#### HTTP/1.1 (`[listen_protocols.http1]`)

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `upload_buffer_size` | Integer | `32768` | Buffer size for outgoing traffic, bytes |

#### HTTP/2 (`[listen_protocols.http2]`)

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `initial_connection_window_size` | Integer | `8388608` | Connection flow control window (8 MB) |
| `initial_stream_window_size` | Integer | `131072` | Stream flow control window (128 KB) |
| `max_concurrent_streams` | Integer | `1000` | Concurrent streams |
| `max_frame_size` | Integer | `16384` | Largest frame payload |
| `header_table_size` | Integer | `65536` | Header table size |

#### QUIC and HTTP/3 (`[listen_protocols.quic]`)

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `recv_udp_payload_size` | Integer | `1350` | Largest received UDP payload |
| `send_udp_payload_size` | Integer | `1350` | Largest sent UDP payload |
| `initial_max_data` | Integer | `104857600` | Initial connection data limit (100 MB) |
| `initial_max_stream_data_bidi_local` | Integer | `1048576` | Local bidirectional stream limit (1 MB) |
| `initial_max_stream_data_bidi_remote` | Integer | `1048576` | Remote bidirectional stream limit (1 MB) |
| `initial_max_stream_data_uni` | Integer | `1048576` | Unidirectional stream limit (1 MB) |
| `initial_max_streams_bidi` | Integer | `4096` | Bidirectional streams |
| `initial_max_streams_uni` | Integer | `4096` | Unidirectional streams |
| `max_connection_window` | Integer | `25165824` | Largest connection window (24 MB) |
| `max_stream_window` | Integer | `16777216` | Largest stream window (16 MB) |
| `disable_active_migration` | Boolean | `true` | Disable active connection migration |
| `enable_early_data` | Boolean | `true` | Enable 0-RTT |
| `message_queue_capacity` | Integer | `4096` | QUIC multiplexer queue |

### Forward Protocol Settings

Connections go directly by default:

```toml
[forward_protocol]
direct = {}
```

Through a SOCKS5 proxy:

```toml
[forward_protocol.socks5]
address = "127.0.0.1:1080"
extended_auth = false
```

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `address` | String | none | **Required.** SOCKS5 proxy address |
| `extended_auth` | Boolean | `false` | Extended authentication |

### Reverse Proxy Settings

Optional. The server terminates TLS on `reverse_proxy_hosts` and on main hosts for paths under `path_mask`, and passes the request to `server_address` over HTTP/1.1. In the fork the reverse proxy serves the decoy site.

```toml
[reverse_proxy]
server_address = "127.0.0.1:8080"
path_mask = "/api"
h3_backward_compatibility = false
```

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `server_address` | String | none | **Required.** Origin server address |
| `path_mask` | String | none | **Required.** Path prefix, must start with `/` |
| `h3_backward_compatibility` | Boolean | `false` | Override the HTTP method when translating HTTP/3 to HTTP/1 |

Translated requests carry the `X-Original-Protocol` header (`HTTP1` or `HTTP3`). `server_address` is set by the operator, so `allow_private_network_connections` does not apply to it.

### ICMP Settings

Optional. Some systems need superuser privileges.

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `interface_name` | String | `eth0` (Linux), `en0` (others) | Network interface of the ICMP socket |
| `request_timeout_secs` | Integer | `3` | ICMP request timeout, seconds |
| `recv_message_queue_capacity` | Integer | `256` | Queue per client |

### Metrics Settings

Optional. Prometheus metrics; details: [METRICS.en.md](METRICS.en.md).

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `address` | String | `127.0.0.1:1987` | Metrics address |
| `request_timeout_secs` | Integer | `3` | Request timeout, seconds |
| `per_client_metrics` | Boolean | `false` | Per-user series and the `/clients` path. Exposes usernames and client addresses |

The metrics handler serves `/health-check`, `/metrics`, `/clients` and `/ja4` (added in the fork). `/ja4` returns JSON counts of the JA4 fingerprints of incoming ClientHello messages since the server started. The fork also adds the metrics `xhttp_reserved_bytes` and `xhttp_memory_rejections_total` (the NineLives memory budget).

### Subscription Settings

Optional. The server serves per-user JSON settings over HTTPS on a main host.

```toml
[subscription]
enabled = true
hostname = "vpn.example.com"
path = "/subscription"
address = "203.0.113.1:443"
name = "Example VPN"
dns_upstreams = ["tls://1.1.1.1", "tls://8.8.8.8"]
custom_sni = "example.org"
client_random_prefix = "a0b0/f0f0"
```

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | Boolean | `false` | Master switch |
| `hostname` | String | none | Must match a `main_hosts` hostname |
| `path` | String | `/subscription` | Path (exact match); starts with `/`, cannot be just `/` |
| `address` | String | none | **Required.** `host:port` or `ip:port` placed in the JSON |
| `name` | String | none | Name hint for the client |
| `dns_upstreams` | Array | `[]` | DNS upstream hint |
| `custom_sni` | String | none | Custom SNI for the client |
| `client_random_prefix` | String | none | `hex[/hex]` |

The fields are validated on every start, including `--client_config`, whenever the section exists, even with `enabled = false`. `address` is also the fallback address for `--client_config`.

Answers for a request to the subscription path:

| Condition | Answer |
| --- | --- |
| Subscription not configured, or the SNI is not the subscription host | `404` |
| Missing or wrong login (HTTP Basic), or the client address is on the decoy-only list | `401` |
| Subscription disabled | `403` |
| Method is not `GET` | `405` |
| Otherwise | `200` with the JSON of this user only |

---

## Fork keys

Added in the fork; upstream has none of them. All are optional and off or neutral by default.

### NineLives (XHTTP, protocol 1)

Details: [docs/ninelives.en.md](docs/ninelives.en.md). The keys live in the main file. NineLives is not compatible with XHTTP from Xray or with the TrustTunnel client; the word XHTTP stayed in key names for historical reasons.

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `xhttp_enable` | Boolean | `false` | Enable NineLives on main hosts |
| `xhttp_path` | String | `/api/v2/` | Request path prefix; the session id is appended, for example `/api/v2/<sid>` |
| `xhttp_secret_key` | String | none | Seed of the handshake signature: base64 of 32 bytes. Without a valid key a client that starts the key exchange gets `400` |
| `xhttp_resume_timeout` | Integer, seconds | `60` | How long the server keeps a session after its connection died. `0`: no resume, the client gets `400` |
| `xhttp_unacked_limit` | Integer, bytes | `1048576` | Unconfirmed bytes per session; at least `65536` while NineLives is enabled |
| `xhttp_memory_limit` | Integer, bytes | a quarter of memory, 64 to 512 MiB | Shared budget of NineLives and CatTail buffers. The smaller of physical memory and the cgroup limit is used; if both are unknown it is computed from 256 MiB, that is 64 MiB. Must be above zero |
| `xhttp_supported_versions` | Array | `[1]` | Only `[1]`: any other value fails validation |
| `transport_mode` | String | `"xhttp"` | Only `"xhttp"`: any other value fails validation |

`xhttp_secret_key` is not checked at start; a bad value shows up as a refusal during the key exchange.

### CatTail

An optional outer layer on its own TCP port: [docs/cattail.en.md](docs/cattail.en.md). Without the `[cattail]` section the port is not opened.

```toml
[cattail]
listen_address = "0.0.0.0:20443"
key = "<base64 of 32 random bytes>"
# optional
pattern = """
stop=3
0=400-1400,c,200-900
1=300-1200,c,100-700
2=100-600,c
mid=0-32
end=0-96
"""
```

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `listen_address` | String | none | **Required.** TCP address of the CatTail listener; must differ from the main `listen_address`. A port range is mapped to it by the host firewall |
| `key` | String | none | **Required.** Endpoint key: base64 of 32 bytes |
| `pattern` | String | the scheme shown above | Padding scheme of the server direction |

An invalid key or scheme stops the server from starting.

### Decoy-only addresses

| Setting | Type | Default | Description |
| --- | --- | --- | --- |
| `decoy_only_ips_file` | String | none | File with a list of addresses and networks |

The file has one IP address or CIDR network per line; `#` starts a comment, lines that do not parse are skipped. The server checks the modification time at most every 5 seconds and re-reads the file when it changed. A missing or unreadable file means an empty list. No `SIGHUP` is needed.

From a listed address the decoy site is shown as usual, while every VPN request is answered as a wrong password, even with valid credentials: `CONNECT`, NineLives, the subscription (`401`) and credentials in the SNI. A listed address gets silence on the CatTail port.

```text
 request from an address in decoy_only_ips_file
        |
        v
 TLS handshake and decoy site: same as for everyone
        |
        +-- CONNECT, NineLives, credentials in SNI --> answered as a wrong password
        +-- subscription ----------------------------> 401
        +-- CatTail port ----------------------------> silence
```

Figure 1. What a decoy-only address receives.

### Other keys of the fork's tree

| Where | Key | Default | Description |
| --- | --- | --- | --- |
| `hosts.toml` | `allowed_sni` | `[]` | Extra host names (also in upstream) |
| `rules.toml` | `client_random_auth_key` | none | See [Rules Reference](#rules-reference) (also in upstream) |

---

## TLS Hosts Reference

Each host has three required fields and one optional:

| Field | Type | Description |
| --- | --- | --- |
| `hostname` | String | **Required.** Name for SNI matching; unique across all hosts |
| `cert_chain_path` | String | **Required.** PEM certificate chain file |
| `private_key_path` | String | **Required.** PEM private key file |
| `allowed_sni` | Array | Extra names served with this host certificate |

The certificate and key are read at start; an unreadable file stops the server from starting. At least one main host is required.

### Host Types

- **`main_hosts`**: primary hosts, tunneling and service requests.
- **`ping_hosts`**: answer `200 OK` to HTTPS `GET` (health checks).
- **`speedtest_hosts`**: speed tests:
    - `GET /Nmb.bin` (N from 1 to 100): download N megabytes;
    - `POST /upload.html`: upload up to 120 MB.
- **`reverse_proxy_hosts`**: forward to the reverse proxy; ignored without `[reverse_proxy]`.

On main hosts a request is routed by path in this order:

```text
 request to a main host
        |
        v
 subscription path (exact match)          --> subscription
 ping_path (if ping_enable)               --> ping
 speedtest_path (if speedtest_enable)     --> speed test
 xhttp_path (if xhttp_enable)             --> NineLives
 reverse proxy path_mask                  --> reverse proxy
 everything else                          --> tunnel (CONNECT)
```

Figure 2. Handler choice on a main host. Paths match by prefix, except the subscription.

---

## Rules Reference

Rules filter incoming connections by client address and by the TLS `client_random` field.

### Rule Structure

```toml
[[rule]]
cidr = "192.168.0.0/16"           # optional: IP range
client_random_prefix = "aabbcc"   # optional: hex prefix or prefix/mask
client_random_auth_key = "0011..." # optional: hex key for the SNI-derived client random
action = "allow"                  # required: "allow" or "deny"
```

### Evaluation

1. Rules are evaluated in order.
2. The first matching rule applies.
3. If no rule matches, the connection is **allowed**.
4. If both `cidr` and a client random condition are set, both must match.
5. `client_random_prefix` and `client_random_auth_key` are mutually exclusive: if both are set, only the key is enforced.
6. If the file has a rule with a prefix or a key, a connection without `client_random` is denied at once.

### Client Random Matching

**Simple prefix:**

```toml
client_random_prefix = "aabbcc"
```

Matches if the client random starts with `0xaabbcc`.

**Prefix with mask:**

```toml
client_random_prefix = "a0b0/f0f0"
```

Compares `client_random & mask` with `prefix & mask` byte by byte, within the length of the mask and the prefix.

**Key (client_random_auth_key):**

```toml
client_random_auth_key = "00112233445566778899aabbccddeeff"
```

Matches if the last 16 bytes of the 32-byte `client_random` equal

```text
AES-128-ECB(SHA256(SNI)[0:16], HKDF-SHA256(secret = client_random_auth_key, salt = client_random[0:16], info = "tls13 encryption context"))
```

The first 16 bytes are random per connection, so `client_random` differs every time while the check stays deterministic.

Constraints:

- The key is a non-empty even-length hex string. 16 bytes (32 hex chars) or more are recommended; the setup wizard refuses to create a rule with a shorter key.
- The check depends on the SNI. If the file has a key rule, connections without an SNI or without `client_random` are denied.
- The SNI from the ClientHello is used. If the client sets `custom_sni`, that value is checked.
- The key is a credential: whoever knows it passes the check. It goes into exported configs and links, so keep them secret.
- The client must support the `client_random_auth_key` field.

```text
 client                                        server
   |  ClientHello: client_random =                |
   |    [16 random bytes | AES-128-ECB(SHA256(SNI)[0:16], HKDF(key, these 16 bytes))]
   |--------------------------------------------->|
   |                     server derives the same 16 bytes from the rule
   |                     key and the SNI; equal: the rule matches, action applies
```

Figure 3. The `client_random_auth_key` check.

### Examples

```toml
# Deny an IP range
[[rule]]
cidr = "192.168.1.0/24"
action = "deny"

# Allow a client random prefix
[[rule]]
client_random_prefix = "deadbeef"
action = "allow"

# Deny internal networks with a given client signature
[[rule]]
cidr = "10.0.0.0/8"
client_random_prefix = "bad0/ff00"
action = "deny"

# Allow only clients that prove a key bound to the SNI
[[rule]]
client_random_auth_key = "00112233445566778899aabbccddeeff"
action = "allow"

# Catch-all deny (place last)
[[rule]]
action = "deny"
```

---

## Runtime

### Reloading

Send `SIGHUP` to reload the TLS hosts file and the `[subscription]` section of the main file without a restart:

```bash
kill -HUP $(pidof trusttunnel_endpoint)
```

If the new `[subscription]` fails to parse or validate, the previous one is kept and a warning is logged. Other settings apply after a restart. The `decoy_only_ips_file` list reloads by itself.

### Systemd Service

The service template is [scripts/trusttunnel.service.template](scripts/trusttunnel.service.template). It assumes the directory `/opt/trusttunnel` and the files `vpn.toml`, `hosts.toml`:

```bash
sudo cp /opt/trusttunnel/trusttunnel.service.template /etc/systemd/system/trusttunnel.service
sudo systemctl daemon-reload
sudo systemctl enable --now trusttunnel

# Reload TLS hosts and the subscription (the template has no ExecReload, so "reload" does not work)
sudo systemctl kill -s HUP trusttunnel

# Logs
sudo journalctl -u trusttunnel -f
```

Installing and updating: [README.en.md](README.en.md). The `scripts/install.sh` in this tree comes from upstream and downloads upstream archives; do not use it.

---

## See Also

- [README.en.md](README.en.md): install and run
- [docs/features.en.md](docs/features.en.md): what CatTunnel features need from the server
- [docs/upstream-divergence.en.md](docs/upstream-divergence.en.md): differences from upstream
- [DEVELOPMENT.en.md](DEVELOPMENT.en.md): development
- [PROTOCOL.en.md](PROTOCOL.en.md): protocol
- [CHANGELOG.md](CHANGELOG.md): changelog
