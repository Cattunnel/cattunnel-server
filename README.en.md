# CatTunnel server

[Русская версия](README.md)

CatTunnel server is the VPN endpoint used by the CatTunnel apps. It is a fork of the
TrustTunnel endpoint (version 1.3.0-beta.2) and speaks the same protocol: TLS, then
HTTP/2 (or HTTP/3 over QUIC), with every TCP connection of the user carried as a
`CONNECT` stream. On top of that it adds NineLives, a second transport made of paired
HTTP requests, plus several changes to the TLS listener, the decoy site and the
`CONNECT` answer.

This tree is `1.3.0-beta.2-32-ga12e73a`.

## CatTunnel repositories

| Repository | What it is |
| --- | --- |
| [Cattunnel/cattunnel](https://github.com/Cattunnel/cattunnel) | Client app for Android, Windows and Linux |
| [Cattunnel/cattunnel-router](https://github.com/Cattunnel/cattunnel-router) | Router edition for OpenWrt, Keenetic and ASUS Merlin |
| [Cattunnel/cattunnel-server](https://github.com/Cattunnel/cattunnel-server) | Server endpoint (this repository) |

## Contents

- [Names kept from upstream](#names-kept-from-upstream)
- [Features](#features)
- [Build](#build)
- [Install and run](#install-and-run)
- [Configuration](#configuration)
- [Export a client configuration](#export-a-client-configuration)
- [Docker](#docker)
- [Documentation](#documentation)
- [License](#license)

## Names kept from upstream

The binaries, crates, config files and the systemd unit keep their upstream names so that
existing configurations, scripts and clients keep working: the endpoint binary is
`trusttunnel_endpoint`, the library crate is `trusttunnel`, the deep-link crate is
`trusttunnel-deeplink`, the default install directory in the examples is
`/opt/trusttunnel` and the unit is `trusttunnel.service`. Some help texts printed by the
binaries also still say "TrustTunnel". Wherever this documentation shows such a name, it is
a literal identifier, not a separate product.

## Features

Short list. The server side of each feature, with the config keys, is in
[docs/features.en.md](docs/features.en.md).

**From upstream**

- TCP, UDP and ICMP tunneling over HTTP/1.1, HTTP/2 and HTTP/3 (QUIC).
- Authentication by login and password (`credentials.toml`), connection rules by client IP
  and by the TLS `client_random` (`rules.toml`), including `client_random_auth_key`: a
  per-connection value derived from a shared key, so the field stays random for an observer.
- Decoy site for anyone without valid credentials (reverse proxy to a local web server).
- Several names per TLS host (`allowed_sni`), served with that host's certificate.
- Export of client configurations as `tt://` deep links or TOML, per-user JSON subscription
  (`[subscription]`), Prometheus metrics, hot reload of TLS hosts on `SIGHUP`.

**Added in this fork**

- **NineLives transport (XHTTP protocol 1).** A TCP session of the client is carried by two
  HTTP/2 requests on the same path: a `GET` whose response body carries the download and a
  `POST` whose request body carries the upload. Frames have offsets and acknowledgements. An
  optional signed handshake (X25519 key exchange, Ed25519 signature by the server,
  HKDF-SHA256, AES-128-GCM per direction) encrypts the frame stream inside TLS. A session can
  be resumed after the connection drops (60 s by default). Buffers are limited by a global
  memory budget. NineLives is off unless `xhttp_enable = true`. It is **not** compatible
  with XHTTP from Xray: the name XHTTP in config keys is historical. See
  [docs/ninelives.en.md](docs/ninelives.en.md).
- **`CONNECT` answer.** The `200` response to `CONNECT` carries `date`, `server`,
  `cache-control` and two random identifiers of random length, instead of a bare status line.
- **TLS listener.** A ClientHello split across several TLS records is joined before the
  listener parses it, so the client may split it. Input that is not TLS is rejected at once.
- **Decoy site.** HTTP/1.1 keep-alive, chunked bodies and trailers pass through correctly,
  chunk size lines and trailers are limited, HTTP/2 is allowed on reverse proxy hosts.
- **Metrics.** JA4 fingerprints of incoming ClientHellos are counted, and the NineLives
  memory budget is exported (`xhttp_reserved_bytes`, `xhttp_memory_rejections_total`).

**Client-side features that need nothing on the server**

HTTP/2 padding, the client connection pool, browser TLS profiles and the anti-DPI modes of
the CatTunnel client work with this server and with an unmodified upstream endpoint. The
server only has to accept standard HTTP/2 padding and several TLS connections per client,
which it does. Details: [docs/features.en.md](docs/features.en.md#client-side-features).

## Build

There are no prebuilt binaries in this README. Build from source.

Requirements (see [DEVELOPMENT.md](DEVELOPMENT.md)): Rust 1.95 (pinned in
`rust-toolchain.toml`), CMake, libclang and a C++ compiler. Linux and macOS are supported.

```shell
git clone https://github.com/Cattunnel/cattunnel-server.git
cd cattunnel-server
cargo build --bins --release
```

The binaries are `target/release/trusttunnel_endpoint` and `target/release/setup_wizard`.
The same through the Makefile: `make endpoint/build` and `make endpoint/build-wizard`.

Tests and linters: `make test`, `make lint` (needs `markdownlint-cli`, rustfmt and clippy).

## Install and run

Copy the two binaries and the service template to one directory, for example
`/opt/trusttunnel`:

```shell
sudo mkdir -p /opt/trusttunnel
sudo cp target/release/trusttunnel_endpoint target/release/setup_wizard /opt/trusttunnel/
sudo cp scripts/trusttunnel.service.template /opt/trusttunnel/
```

Generate the configuration with the wizard. It asks for the listen address, users, rules,
and the certificate (Let's Encrypt, self-signed, or existing files):

```shell
cd /opt/trusttunnel
sudo ./setup_wizard
```

`sudo` is needed to manage TLS certificates. `./setup_wizard -h` lists the
non-interactive options. Certificate renewal with Certbot: [CERT_RENEWAL.md](CERT_RENEWAL.md).

Run it as a systemd service. The template assumes the binary and the config files are in
`/opt/trusttunnel` with the default names; edit it if your paths differ:

```shell
sudo cp /opt/trusttunnel/trusttunnel.service.template /etc/systemd/system/trusttunnel.service
sudo systemctl daemon-reload
sudo systemctl enable --now trusttunnel
```

To update, build the new revision, stop the service, replace the binaries and start it again.

`scripts/install.sh` from upstream downloaded upstream release archives. In this fork it only
prints a message and exits with an error, so that nobody installs upstream binaries by
accident.

## Configuration

The endpoint reads TOML files: `vpn.toml` (main settings), `hosts.toml` (TLS hosts),
`credentials.toml` (users) and optionally `rules.toml` (connection rules). Full reference:
[CONFIGURATION.md](CONFIGURATION.md).

Keys added or used by this fork:

| File | Key | Default | Meaning |
| --- | --- | --- | --- |
| `vpn.toml` | `xhttp_enable` | `false` | Turn NineLives on |
| `vpn.toml` | `xhttp_path` | `"/api/v2/"` | Path prefix of NineLives requests |
| `vpn.toml` | `xhttp_secret_key` | none | Base64 Ed25519 seed (32 bytes) that signs the NineLives handshake |
| `vpn.toml` | `xhttp_resume_timeout` | `60` | Seconds a session is kept for resume; `0` turns resume off |
| `vpn.toml` | `xhttp_memory_limit` | 1/4 of RAM, 64..512 MiB | Global budget for NineLives buffers, bytes |
| `vpn.toml` | `xhttp_unacked_limit` | `1048576` | Unacknowledged bytes per session |
| `vpn.toml` | `xhttp_supported_versions` | `[1]` | Only protocol 1 exists |
| `vpn.toml` | `transport_mode` | `"xhttp"` | NineLives listener mode; other values are reserved |
| `hosts.toml` | `allowed_sni` | `[]` | Extra names accepted for a main host |
| `rules.toml` | `client_random_auth_key` | none | Hex key for `client_random` checks |

## Export a client configuration

The endpoint prints a configuration for a user from `credentials.toml`:

```shell
cd /opt/trusttunnel
./trusttunnel_endpoint vpn.toml hosts.toml -c <client_name> -a <address>
```

`<address>` is `ip`, `ip:port`, `domain` or `domain:port`. The default output is a
`tt://?` deep link ([DEEP_LINK.md](DEEP_LINK.md)) that the CatTunnel app imports from text
or a QR code. `--format toml` prints a TOML file for a command-line client. Other options:
`--name`, `--dns-upstream`, `--client-random-auth-key`, `--subscription-url`,
`--subscription-only`; see `./trusttunnel_endpoint -h` and
[CONFIGURATION.md](CONFIGURATION.md#command-line-arguments).

The exported link does not contain the NineLives settings. A client that should use
NineLives needs `xhttp_enable`, the path and the server public key in its link; the CatTunnel
app reads them from `tt://` tags `0x4F`, `0x50` and `0x51` (see
[docs/ninelives.en.md](docs/ninelives.en.md#client-side)).

A deep link that carries a subscription URL contains HTTP Basic Auth credentials. Treat such
links as secrets.

## Docker

`Dockerfile` builds the endpoint from this source tree. `docker-compose.yml` maps host
port 443 to container port 8443 and keeps the configuration in a volume:

```shell
docker compose up -d --build
```

Without config files the container runs `setup_wizard` (interactive with a TTY, otherwise
non-interactive from `TT_*` environment variables, see `docker-entrypoint.sh`). Details:
[DEVELOPMENT.md](DEVELOPMENT.md#run-with-external-configuration-cicd).

## Documentation

| File | Content |
| --- | --- |
| [docs/features.en.md](docs/features.en.md) | Server side of CatTunnel features and their keys |
| [docs/ninelives.en.md](docs/ninelives.en.md) | NineLives overview, keys, key generation |
| [docs/xhttp-protocol.md](docs/xhttp-protocol.md) | NineLives wire format (in Russian) |
| [CONFIGURATION.md](CONFIGURATION.md) | All settings and command line options |
| [PROTOCOL.md](PROTOCOL.md) | Base tunnel protocol (`CONNECT` transport) |
| [DEEP_LINK.md](DEEP_LINK.md) | `tt://` deep link format |
| [CERT_RENEWAL.md](CERT_RENEWAL.md) | Certificate renewal with Certbot |
| [METRICS.md](METRICS.md) | Prometheus metrics |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Building, cross-compiling, Docker, benchmarks |
| [CHANGELOG.md](CHANGELOG.md) | Changes of this fork, then upstream history |

## License

Apache License 2.0, see [LICENSE](LICENSE).

CatTunnel server is based on TrustTunnel by AdGuard, Apache License 2.0; modified. Copyright
notices in the source files are kept. CatTunnel is an independent project: it is not
affiliated with or endorsed by AdGuard. "TrustTunnel" and "AdGuard" are trademarks of their
owners.