Русский: [DEVELOPMENT.md](DEVELOPMENT.md) (the Russian text is the main one; where they differ, the Russian text is correct)

# Development

This document covers building, running and checking the CatTunnel server from source. The build and the commands are the same as in TrustTunnel ([original](https://github.com/TrustTunnel/TrustTunnel/blob/master/DEVELOPMENT.md)). What differs in the fork: the Rust version, the repository, the names. Crates and binaries keep the upstream names: `trusttunnel_endpoint`, `trusttunnel` (see the [README](README.en.md)).

## Table of contents

- [Getting started](#getting-started)
    - [Prerequisites](#prerequisites)
    - [Building](#building)
    - [Cross-compiling for Linux](#cross-compiling-for-linux)
- [Running](#running)
    - [First setup](#first-setup)
    - [Docker](#docker)
    - [Ready-made settings (CI/CD)](#ready-made-settings-cicd)
    - [Custom settings](#custom-settings)
- [Tests and linters](#tests-and-linters)
- [Benchmarks](#benchmarks)
- [See also](#see-also)

## Getting started

### Prerequisites

Linux and macOS are supported. `make init` enables the git hooks from `scripts/hooks` (checks before a commit).

- Rust 1.95. The version is pinned in `rust-toolchain.toml`, and rustup installs it on the first build inside the repository. Manually: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --default-toolchain 1.95 -y`.
- CMake. Debian/Ubuntu: `apt install cmake`, macOS: `brew install cmake`.
- [libclang](https://rust-lang.github.io/rust-bindgen/requirements.html#installing-clang). Debian/Ubuntu: `apt install libclang-dev`.
- A C++ compiler. Debian/Ubuntu: `apt install build-essential`, macOS: `xcode-select --install`.

The linters need `markdownlint-cli` (`npm install -g markdownlint-cli`), plus `rustfmt` and `clippy` (`rustup component add rustfmt clippy`).

### Building

```shell
cargo build --bins --release
```

Debug build:

```shell
cargo build --bins
```

The binaries appear in `target/release` or `target/debug`. Through the Makefile: `make endpoint/build`.

### Cross-compiling for Linux

To build a static x86_64-unknown-linux-musl binary from macOS or another platform, use the `adguard/core-libs` image. The upstream release build uses tag `2.10` ([`.github/workflows/publish-release.yml`](.github/workflows/publish-release.yml)):

```shell
docker run --rm --platform linux/amd64 -v "$(pwd)":/work -w /work adguard/core-libs:2.10 sh -c '\
    CC=x86_64-linux-musl-gcc \
    CXX=x86_64-linux-musl-g++ \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
    BINDGEN_EXTRA_CLANG_ARGS="--sysroot=/opt/cross/x86_64-linux-musl" \
    cargo build --release --target x86_64-unknown-linux-musl'
```

The result is in `target/x86_64-unknown-linux-musl/release/`. For aarch64, replace `x86_64` with `aarch64` in the compiler names, the sysroot and the target.

Prebuilt archives are in the [CatTunnel server releases](https://github.com/Cattunnel/cattunnel-server/releases). How to verify them: [VERIFY_RELEASES.en.md](VERIFY_RELEASES.en.md).

## Running

### First setup

Quick start:

```shell
make ENDPOINT_HOSTNAME="example.org" endpoint/setup  # skip if the settings already exist
make endpoint/run
```

The Makefile variables (`LISTEN_ADDRESS`, `LISTEN_PORT`, `CONFIG_FILE`, `HOSTS_CONFIG_FILE`, `LOG_LEVEL` and others) are at the top of `Makefile`.

```text
 make endpoint/setup                       make endpoint/run
   |                                          |
   +- builds the binary with the wizard       +- builds the binary (endpoint/build)
   +- runs --setup-wizard                     +- runs it with vpn.toml hosts.toml
   |    asks about the certificate
   |    and the users                         default port 443
   +- writes vpn.toml, hosts.toml,
      credentials.toml, certs/ (if a certificate was created)
```

Figure 1. The order of the first-setup commands. The wizard is built into the main binary.

The wizard is interactive unless `--mode non-interactive` is given.

### Docker

```shell
docker build -t trusttunnel-endpoint:latest .   # build the image
docker run -it --name trusttunnel-endpoint trusttunnel-endpoint:latest   # create the container and start it interactively
docker start -i trusttunnel-endpoint            # start it again
```

If the settings files are missing, the container runs the wizard: with questions when a terminal is attached, otherwise without questions from `TT_*` environment variables (`TT_HOSTNAME` and `TT_CREDENTIALS` are required; see `docker-entrypoint.sh`).

`Dockerfile.prebuilt` does not build the server from this tree: it downloads an archive from the **TrustTunnel** releases (`TT_RELEASE_BASE_URL`). Use `Dockerfile` for the CatTunnel server.

### Ready-made settings (CI/CD)

If your pipeline creates the settings files in advance, mount them into the working volume and the wizard is not needed.

Files under `/trusttunnel_endpoint`:

- `vpn.toml`
- `hosts.toml`
- `credentials.toml`
- optional `rules.toml`
- optional `certs/` (if `hosts.toml` points to certificate files)

`docker-compose.yml` maps host port 443 to container port 8443 (TCP and UDP), so `vpn.toml` must listen on `0.0.0.0:8443`.

```shell
docker compose up -d --build
```

- No wizard environment variables are needed in this mode.
- Port `80:80` is only for Let's Encrypt issuance and renewal (HTTP-01) inside the container. Remove it if the certificates are external.

### Custom settings

```shell
cargo run --bin trusttunnel_endpoint -- --setup-wizard  # a dialogue to tweak the settings
cargo run --bin trusttunnel_endpoint -- <lib-settings> <hosts-settings>  # file names depend on the previous step
```

More about the binary: [endpoint/README.en.md](./endpoint/README.en.md).

## Tests and linters

```shell
make test        # cargo test --workspace
make lint        # lint-md and lint-rust
make lint-fix    # auto-fix
```

`make lint-rust` runs `cargo fmt --all -- --check` and `cargo clippy -- -D warnings`. `make lint-md` runs `markdownlint .` with the rules from `.markdownlint.json`. The `scripts/hooks/pre-commit` hook (enabled by `make init`) runs these checks for the changed files.

The files in `.github/workflows` and `bamboo-specs` describe the upstream CI. Some jobs there are limited by a repository condition to the upstream repositories and do not run in the fork.

## Benchmarks

A suite for measuring throughput: [bench/README.en.md](bench/README.en.md).

## See also

- [README.en.md](README.en.md): quick start
- [PROTOCOL.en.md](PROTOCOL.en.md): protocol specification
- [CONFIGURATION.en.md](CONFIGURATION.en.md): configuration
- [CHANGELOG.md](CHANGELOG.md): changelog
