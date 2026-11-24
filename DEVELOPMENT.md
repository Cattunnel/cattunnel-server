English: [DEVELOPMENT.en.md](DEVELOPMENT.en.md)

# Разработка

Документ о сборке, запуске и проверке сервера CatTunnel из исходников. Сборка и команды те же, что в TrustTunnel ([оригинал](https://github.com/TrustTunnel/TrustTunnel/blob/master/DEVELOPMENT.md)). Отличия форка: версия Rust, репозиторий, имена. Крейты и бинарники называются как в апстриме: `trusttunnel_endpoint`, `trusttunnel` (см. [README](README.md)).

## Содержание

- [Начало работы](#начало-работы)
    - [Что нужно](#что-нужно)
    - [Сборка](#сборка)
    - [Кросс-сборка для Linux](#кросс-сборка-для-linux)
- [Запуск](#запуск)
    - [Первая настройка](#первая-настройка)
    - [Docker](#docker)
    - [Готовые настройки (CI/CD)](#готовые-настройки-cicd)
    - [Свои настройки](#свои-настройки)
- [Тесты и линтеры](#тесты-и-линтеры)
- [Бенчмарки](#бенчмарки)
- [Смотрите также](#смотрите-также)

## Начало работы

### Что нужно

Поддерживаются Linux и macOS. Команда `make init` подключает хуки git из `scripts/hooks` (проверка перед коммитом).

- Rust 1.95. Версия записана в `rust-toolchain.toml`, rustup ставит её сам при первой сборке в каталоге репозитория. Вручную: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --default-toolchain 1.95 -y`.
- CMake. Linux (Debian/Ubuntu): `apt install cmake`, macOS: `brew install cmake`.
- [libclang](https://rust-lang.github.io/rust-bindgen/requirements.html#installing-clang). Debian/Ubuntu: `apt install libclang-dev`.
- Компилятор C++. Debian/Ubuntu: `apt install build-essential`, macOS: `xcode-select --install`.

Для линтеров нужен `markdownlint-cli` (`npm install -g markdownlint-cli`), а также `rustfmt` и `clippy` (`rustup component add rustfmt clippy`).

### Сборка

```shell
cargo build --bins --release
```

Отладочная сборка:

```shell
cargo build --bins
```

Бинарники появятся в `target/release` или `target/debug`. Через Makefile: `make endpoint/build`.

### Кросс-сборка для Linux

Чтобы собрать статический бинарник для x86_64-unknown-linux-musl с macOS или другой платформы, используйте образ `adguard/core-libs`. Релизная сборка апстрима использует тег `2.10` ([`.github/workflows/publish-release.yml`](.github/workflows/publish-release.yml)):

```shell
docker run --rm --platform linux/amd64 -v "$(pwd)":/work -w /work adguard/core-libs:2.10 sh -c '\
    CC=x86_64-linux-musl-gcc \
    CXX=x86_64-linux-musl-g++ \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
    BINDGEN_EXTRA_CLANG_ARGS="--sysroot=/opt/cross/x86_64-linux-musl" \
    cargo build --release --target x86_64-unknown-linux-musl'
```

Результат: `target/x86_64-unknown-linux-musl/release/`. Для aarch64 замените `x86_64` на `aarch64` в именах компиляторов, sysroot и цели.

Готовые архивы лежат в [релизах CatTunnel server](https://github.com/Cattunnel/cattunnel-server/releases). Как их проверить: [VERIFY_RELEASES.md](VERIFY_RELEASES.md).

## Запуск

### Первая настройка

Быстрый запуск:

```shell
make ENDPOINT_HOSTNAME="example.org" endpoint/setup  # пропустите, если настройки уже есть
make endpoint/run
```

Переменные Makefile (`LISTEN_ADDRESS`, `LISTEN_PORT`, `CONFIG_FILE`, `HOSTS_CONFIG_FILE`, `LOG_LEVEL` и другие) смотрите в начале `Makefile`.

```text
 make endpoint/setup                       make endpoint/run
   |                                          |
   +- собирает бинарник с мастером            +- собирает бинарник (endpoint/build)
   +- запускает --setup-wizard                +- запускает его с vpn.toml hosts.toml
   |    спрашивает про сертификат
   |    и пользователей                       порт по умолчанию 443
   +- пишет vpn.toml, hosts.toml,
      credentials.toml, certs/ (если сертификат создан)
```

Рисунок 1. Порядок команд первой настройки. Мастер встроен в основной бинарник.

Мастер работает в интерактивном режиме, пока не указан `--mode non-interactive`.

### Docker

```shell
docker build -t trusttunnel-endpoint:latest .   # собрать образ
docker run -it --name trusttunnel-endpoint trusttunnel-endpoint:latest   # создать контейнер и запустить в интерактивном режиме
docker start -i trusttunnel-endpoint            # запустить снова
```

Если файлов настроек нет, контейнер запускает мастер: с вопросами при подключённом терминале, иначе без вопросов из переменных окружения `TT_*` (`TT_HOSTNAME`, `TT_CREDENTIALS` обязательны; см. `docker-entrypoint.sh`).

`Dockerfile.prebuilt` не собирает сервер из этого дерева: он скачивает архив из релизов **TrustTunnel** (`TT_RELEASE_BASE_URL`). Для сервера CatTunnel используйте `Dockerfile`.

### Готовые настройки (CI/CD)

Если конвейер создаёт файлы настроек заранее, смонтируйте их в рабочий том, и мастер не понадобится.

Файлы в `/trusttunnel_endpoint`:

- `vpn.toml`
- `hosts.toml`
- `credentials.toml`
- необязательно `rules.toml`
- необязательно `certs/` (если сертификаты указаны в `hosts.toml`)

`docker-compose.yml` пробрасывает порт 443 хоста на порт 8443 контейнера (TCP и UDP), поэтому в `vpn.toml` должен быть адрес `0.0.0.0:8443`.

```shell
docker compose up -d --build
```

- Переменные окружения для мастера в этом режиме не нужны.
- Порт `80:80` нужен только для выпуска и продления Let's Encrypt (HTTP-01) внутри контейнера. Если сертификаты внешние, уберите его.

### Свои настройки

```shell
cargo run --bin trusttunnel_endpoint -- --setup-wizard  # диалог с настройкой параметров
cargo run --bin trusttunnel_endpoint -- <lib-settings> <hosts-settings>  # имена файлов зависят от предыдущего шага
```

Подробнее о бинарнике: [endpoint/README.md](./endpoint/README.md).

## Тесты и линтеры

```shell
make test        # cargo test --workspace
make lint        # lint-md и lint-rust
make lint-fix    # автоисправление
```

`make lint-rust` запускает `cargo fmt --all -- --check` и `cargo clippy -- -D warnings`. `make lint-md` запускает `markdownlint .` с правилами из `.markdownlint.json`. Хук `scripts/hooks/pre-commit` (включается через `make init`) гоняет эти проверки для изменённых файлов.

Файлы в `.github/workflows` и `bamboo-specs` описывают CI апстрима. Часть заданий там ограничена условием на репозиторий апстрима и в форке не запускается.

## Бенчмарки

Набор для измерения пропускной способности: [bench/README.md](bench/README.md).

## Смотрите также

- [README.md](README.md): быстрый старт
- [PROTOCOL.md](PROTOCOL.md): спецификация протокола (на английском)
- [CONFIGURATION.md](CONFIGURATION.md): настройки (на английском)
- [CHANGELOG.md](CHANGELOG.md): журнал изменений
