Русский: [README.md](README.md) (the Russian text is the main one; where they differ, the Russian text is correct)

# Throughput benchmark

The suite belongs to upstream ([TrustTunnel/bench](https://github.com/TrustTunnel/TrustTunnel/tree/master/bench)) and is unchanged in the fork. It measures the throughput of a TrustTunnel tunnel and of WireGuard. The server image is built from the root of this repository, that is, from the CatTunnel server tree. The client comes from upstream by default (`TrustTunnel/TrustTunnelClient`), and the server runs with the settings the wizard creates: NineLives is off (`xhttp_enable = false` by default, `lib/src/settings.rs`). So the benchmark measures the CONNECT tunnel, not NineLives and not CatTail.

## Architecture

The benchmark has three isolated parts:

- `remote-side`: HTTP servers that serve downloads and accept uploads
- `middle-box`: the VPN server host, WireGuard or TrustTunnel
- `local-side`: the host that runs the measurements and opens a tunnel to the remote side through the VPN server

```text
 local-side             middle-box              remote-side
 (client, measurements) (VPN server)            (HTTP :8080)
      |   tunnel            |                        |
      |====================>|  plain traffic         |
      |                     |----------------------->|
      |<====================|<-----------------------|
        WireGuard :51820 or TrustTunnel :4433
```

Figure 1. The three parts of the benchmark. Data goes from local-side through middle-box to remote-side.

## How to run

### Single host

1) Build the images:

   ```shell
   cd ./bench
   ./single_host.sh build
   ```

   The default client repository is `https://github.com/TrustTunnel/TrustTunnelClient.git`. Another client:

   ```shell
   ./single_host.sh build --client=<trusttunnel_client_repo_url>
   ```

   All options:

   ```shell
   ./single_host.sh --help
   ```

2) Run the benchmark:

   ```shell
   ./single_host.sh run
   ```

   This runs all the parts on the current host. `run` options: `--remote_ip`, `--middle_ip` and `--tunnel_type=none|wg|ag|all`. Cleanup: `./single_host.sh clean [all]`.

### Separate hosts

Let the addresses of `host_1`, `host_2` and `host_3` be 203.0.113.1, 203.0.113.2 and 203.0.113.3.

1) `host_1` as the remote side:

   ```shell
   scp Dockerfile user@203.0.113.1:~
   scp -r remote-side user@203.0.113.1:~
   ssh user@203.0.113.1
   docker build -t bench-common .
   docker build -t bench-rs ./remote-side
   docker run -d -p 8080:8080 -p 5201:5201 -p 5201:5201/udp bench-rs
   ```

2) `host_2` as the middle box. The server is built from the repository root, so clone the project on this host:

   ```shell
   ssh user@203.0.113.2
   git clone https://github.com/Cattunnel/cattunnel-server.git ~/trusttunnel-endpoint
   cd ~/trusttunnel-endpoint
   docker build -t bench-common bench/
   ```

    - WireGuard

       ```shell
       docker build -t bench-mb-wg bench/middle-box/wireguard
       docker run -d \
         --cap-add=NET_ADMIN --cap-add=SYS_MODULE --device=/dev/net/tun \
         -p 51820:51820/udp \
         bench-mb-wg
       ```

    - TrustTunnel (the CatTunnel server from this tree)

       ```shell
       docker build \
         --build-arg ENDPOINT_HOSTNAME=endpoint.bench \
         -f bench/middle-box/trusttunnel-rust/Dockerfile \
         -t bench-mb-ag .
       docker run -d \
         -p 4433:4433 -p 4433:4433/udp \
         bench-mb-ag
       ```

3) Run the measurements from `host_3`:

   ```shell
   scp Dockerfile user@203.0.113.3:~
   git clone <TrustTunnelClient.git> ./local-side/trusttunnel/trusttunnel-client
   scp -r local-side user@203.0.113.3:~
   ssh user@203.0.113.3
   docker build -t bench-common .
   docker build -t bench-ls ./local-side
   ```

   - No VPN

      ```shell
      ./local-side/bench.sh no-vpn bridge 203.0.113.1 results/no-vpn
      ```

   - WireGuard

      ```shell
      docker build -t bench-ls-wg ./local-side/wireguard
      ./local-side/bench.sh wg bridge 203.0.113.1 results/wg 203.0.113.2
      ```

   - TrustTunnel

      ```shell
      docker build \
        --build-arg CLIENT_DIR=trusttunnel-client \
        -t bench-ls-ag ./local-side/trusttunnel
      ./local-side/bench.sh ag bridge 203.0.113.1 results/ag 203.0.113.2 endpoint.bench
      ```
