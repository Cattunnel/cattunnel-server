English: [README.en.md](README.en.md)

# Бенчмарк пропускной способности

Набор принадлежит апстриму ([TrustTunnel/bench](https://github.com/TrustTunnel/TrustTunnel/tree/master/bench)) и в форке не менялся. Он измеряет пропускную способность туннеля TrustTunnel и WireGuard. Образ сервера собирается из корня этого репозитория, то есть из дерева CatTunnel server. Клиент по умолчанию берётся из апстрима (`TrustTunnel/TrustTunnelClient`), а сервер запускается с настройками, которые создаёт мастер: NineLives выключен (`xhttp_enable = false` по умолчанию, `lib/src/settings.rs`). Значит, бенчмарк измеряет туннель CONNECT, а не NineLives и не CatTail.

## Устройство

Бенчмарк состоит из трёх изолированных частей:

- `remote-side`: HTTP-серверы, с которых качают и на которые отдают данные
- `middle-box`: хост VPN-сервера, WireGuard или TrustTunnel
- `local-side`: хост, который запускает измерения и поднимает туннель к удалённой стороне через VPN-сервер

```text
 local-side             middle-box              remote-side
 (клиент, измерения)    (VPN-сервер)            (HTTP :8080)
      |   туннель           |                        |
      |====================>|  обычный трафик        |
      |                     |----------------------->|
      |<====================|<-----------------------|
        WireGuard :51820 или TrustTunnel :4433
```

Рисунок 1. Три части бенчмарка. Данные идут от local-side через middle-box к remote-side.

## Как запустить

### Один хост

1) Соберите образы:

   ```shell
   cd ./bench
   ./single_host.sh build
   ```

   Репозиторий клиента по умолчанию: `https://github.com/TrustTunnel/TrustTunnelClient.git`. Другой клиент:

   ```shell
   ./single_host.sh build --client=<trusttunnel_client_repo_url>
   ```

   Все параметры:

   ```shell
   ./single_host.sh --help
   ```

2) Запустите бенчмарк:

   ```shell
   ./single_host.sh run
   ```

   Команда запускает все части на текущем хосте. Параметры `run`: `--remote_ip`, `--middle_ip` и `--tunnel_type=none|wg|ag|all`. Очистка: `./single_host.sh clean [all]`.

### Отдельные хосты

Пусть адреса `host_1`, `host_2` и `host_3` равны 203.0.113.1, 203.0.113.2 и 203.0.113.3.

1) `host_1` как remote-side:

   ```shell
   scp Dockerfile user@203.0.113.1:~
   scp -r remote-side user@203.0.113.1:~
   ssh user@203.0.113.1
   docker build -t bench-common .
   docker build -t bench-rs ./remote-side
   docker run -d -p 8080:8080 -p 5201:5201 -p 5201:5201/udp bench-rs
   ```

2) `host_2` как middle-box. Сервер собирается из корня репозитория, поэтому клонируйте проект на этот хост:

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

    - TrustTunnel (сервер CatTunnel из этого дерева)

       ```shell
       docker build \
         --build-arg ENDPOINT_HOSTNAME=endpoint.bench \
         -f bench/middle-box/trusttunnel-rust/Dockerfile \
         -t bench-mb-ag .
       docker run -d \
         -p 4433:4433 -p 4433:4433/udp \
         bench-mb-ag
       ```

3) Запустите измерения с `host_3`:

   ```shell
   scp Dockerfile user@203.0.113.3:~
   git clone <TrustTunnelClient.git> ./local-side/trusttunnel/trusttunnel-client
   scp -r local-side user@203.0.113.3:~
   ssh user@203.0.113.3
   docker build -t bench-common .
   docker build -t bench-ls ./local-side
   ```

   - Без VPN

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
