# Настройки endpoint CatTunnel

English: [CONFIGURATION.en.md](CONFIGURATION.en.md)

Документ описывает файлы настроек и параметры командной строки сервера CatTunnel (бинарник `trusttunnel_endpoint`). Сервер это форк TrustTunnel endpoint. Всё, что здесь не помечено как «добавлено в форке», работает как в [TrustTunnel](https://github.com/TrustTunnel/TrustTunnel/blob/master/CONFIGURATION.md). Ключи форка собраны в разделе [Ключи форка](#ключи-форка). Чем форк отличается от апстрима в целом: [docs/upstream-divergence.md](docs/upstream-divergence.md).

Репозитории CatTunnel: [cattunnel](https://github.com/Cattunnel/cattunnel) (приложение), [cattunnel-router](https://github.com/Cattunnel/cattunnel-router) (роутер), [cattunnel-server](https://github.com/Cattunnel/cattunnel-server) (этот сервер).

## Содержание

- [Обзор](#обзор)
- [Параметры командной строки](#параметры-командной-строки)
- [Файлы настроек](#файлы-настроек)
- [Справочник настроек](#справочник-настроек)
- [Ключи форка](#ключи-форка)
- [TLS-хосты](#tls-хосты)
- [Правила](#правила)
- [Работа сервера](#работа-сервера)

---

## Обзор

Настройки лежат в файлах TOML:

1. **Основной файл** (`vpn.toml`): сроки, протоколы, транспорты.
2. **Файл TLS-хостов** (`hosts.toml`): сертификаты и имена.
3. **Файл пользователей** (`credentials.toml`): логины и пароли.
4. **Файл правил** (`rules.toml`): фильтр подключений.

Основной файл и файл TLS-хостов сервер получает в аргументах запуска. Пользователей и правила он читает по путям `credentials_file` и `rules_file` из основного файла.

Мастер настройки (`trusttunnel_endpoint --setup-wizard`) создаёт эти файлы. Он не пишет ключи форка (`xhttp_*`, `[cattail]`, `decoy_only_ips_file`): их добавляют в `vpn.toml` вручную после мастера. В неинтерактивном режиме мастер перезаписывает существующие файлы без вопросов.

Как установить сервер из релиза Cattunnel или собрать его: [README.md](README.md#готовые-сборки).

---

## Параметры командной строки

| Параметр | Кратко | Что делает | По умолчанию |
| --- | --- | --- | --- |
| `--version` | `-v` | Печатает версию и выходит | нет |
| `--setup-wizard` | нет | Запускает мастер настройки; должен идти первым аргументом | нет |
| `--loglvl` | `-l` | Уровень журнала: `info`, `debug`, `trace` | `info` |
| `--logfile` | нет | Файл журнала; без него журнал идёт в stdout | stdout |
| `--sentry_dsn` | нет | DSN Sentry для отчётов об ошибках | нет |
| `--jobs` | нет | Число рабочих потоков | число процессоров |
| `<settings>` | нет | **Обязателен.** Путь к основному файлу | нет |
| `<tls_hosts_settings>` | нет | **Обязателен.** Путь к файлу TLS-хостов; `SIGHUP` перечитывает его | нет |
| `--client_config` | `-c` | Печатает настройки клиента с этим именем и выходит | нет |
| `--address` | `-a` | Адрес сервера для настроек клиента (нужен `-c`): `ip`, `ip:port`, `domain` или `domain:port`. Можно повторять. Если включена `[subscription]`, можно не указывать: берётся адрес подписки | нет |
| `--custom-sni` | `-s` | Подменяет SNI клиента; значение должно совпасть с `hostname` или `allowed_sni` основного хоста | нет |
| `--client-random-prefix` | `-r` | Явный `client_random_prefix` для клиента. В `rules.toml` должно быть подходящее правило с действием `allow` | нет |
| `--generate-client-random-prefix` | нет | Создаёт новый `client_random_prefix`, дописывает правило `allow` в `rules.toml` и использует его. Несовместим с `-r`. Нужен `rules_file` в настройках | нет |
| `--prefix-length` | нет | Длина созданного префикса в байтах (с `--generate-client-random-prefix`) | `4` |
| `--prefix-percent` | нет | Доля единичных бит в маске (с `--generate-client-random-prefix`) | `70` |
| `--prefix-mask` | нет | Явная маска в hex; несовместима с `--prefix-length` и `--prefix-percent` | нет |
| `--client-random-auth-key` | нет | Ключ (hex) для `client_random`, выводимого из SNI. Несовместим с `-r` и `--generate-client-random-prefix`. В `rules.toml` должно быть правило с этим ключом и действием `allow` | нет |
| `--format` | `-f` | Формат вывода: `deeplink` (ссылка `tt://?`) или `toml` | `deeplink` |
| `--name` | `-n` | Имя сервера для клиента | нет |
| `--dns-upstream` | `-d` | DNS-сервер для клиента; можно повторять | нет |
| `--subscription-url` | нет | Подменяет адрес подписки (только хост и путь); логин и пароль берутся из `credentials.toml` | нет |
| `--subscription-only` | нет | Печатает минимальную ссылку только с адресом подписки. Нужны `--format deeplink`, включённая `[subscription]` и сертификат хоста подписки, который проверяется системными центрами сертификации | нет |

В экспортируемых настройках и ссылке `tls_profile` не задаётся: клиент берёт `chrome`, пока профиль не указан отдельно. Формат ссылки: [DEEP_LINK.md](DEEP_LINK.md).

После ссылки сервер печатает адрес страницы с QR-кодом на `trusttunnel.org` (это страница апстрима). Приложение CatTunnel берёт ссылку и из текста, так что страница не обязательна.

В ссылке нет настроек NineLives и CatTail. Они приходят клиенту через подписку или тегами ссылки: [docs/ninelives.md](docs/ninelives.md), [docs/cattail.md](docs/cattail.md).

### Примеры

```bash
# Запуск сервера
./trusttunnel_endpoint vpn.toml hosts.toml

# Подробный журнал
./trusttunnel_endpoint vpn.toml hosts.toml -l debug

# Журнал в файл
./trusttunnel_endpoint vpn.toml hosts.toml --logfile /var/log/trusttunnel.log

# Настройки клиента, адрес по IP
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a 203.0.113.1

# Адрес с портом
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a 203.0.113.1:443

# Адрес по имени
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com

# Файл TOML вместо ссылки
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com --format toml

# Явный client_random_prefix
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --client-random-prefix a0b0/f0f0

# Новый client_random_prefix с правилом в rules.toml
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --generate-client-random-prefix

# Новый префикс с заданной маской
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --generate-client-random-prefix --prefix-mask aaaa7777

# Ключ для client_random, выводимого из SNI
./trusttunnel_endpoint vpn.toml hosts.toml -c username -a vpn.example.com \
    --client-random-auth-key 00112233445566778899aabbccddeeff
```

### Мастер настройки

`--setup-wizard` запускается первым аргументом. Без параметров он задаёт вопросы. Неинтерактивный режим:

```bash
sudo ./trusttunnel_endpoint --setup-wizard -m non-interactive \
    -a 0.0.0.0:443 -c admin:secretpass -n vpn.example.com \
    --lib-settings vpn.toml --hosts-settings hosts.toml
```

| Параметр | Кратко | Что делает |
| --- | --- | --- |
| `--mode` | `-m` | `interactive` (по умолчанию) или `non-interactive` |
| `--address` | `-a` | Адрес прослушивания; обязателен в неинтерактивном режиме |
| `--creds` | `-c` | `логин:пароль`; обязателен в неинтерактивном режиме |
| `--hostname` | `-n` | Имя в сертификате; обязательно в неинтерактивном режиме |
| `--lib-settings` | нет | Куда записать основной файл; обязателен в неинтерактивном режиме |
| `--hosts-settings` | нет | Куда записать файл TLS-хостов; обязателен в неинтерактивном режиме |
| `--cert-type` | нет | `self-signed`, `letsencrypt` или `provided` |
| `--cert-chain-path`, `--cert-key-path` | нет | Готовые сертификат и ключ; обязательны при `provided` |
| `--acme-email` | нет | Почта для Let's Encrypt; обязательна при `letsencrypt` |
| `--acme-challenge` | нет | Способ проверки ACME; в неинтерактивном режиме только `http-01` (по умолчанию) |
| `--acme-staging` | нет | Тестовое окружение Let's Encrypt |

Подсказка мастера (`-h`) ссылается на документацию апстрима, а не CatTunnel.

---

## Файлы настроек

### Основной файл (vpn.toml)

> Если сервер слушает порт 443 напрямую, `listen_address = "0.0.0.0:443"`. Если в Docker порт 443 хоста отображается на 8443 контейнера, внутри `vpn.toml` укажите `listen_address = "0.0.0.0:8443"`.

```toml
# Адрес прослушивания
listen_address = "0.0.0.0:443"

# Можно ли маршрутизировать IPv6
ipv6_available = true

# Можно ли клиентам ходить в частную сеть сервера.
# Относится только к трафику клиентов; server_address обратного прокси доступен всегда.
allow_private_network_connections = false

# Срок TLS-рукопожатия (секунды)
tls_handshake_timeout_secs = 10

# Срок клиентского слушателя (секунды)
client_listener_timeout_secs = 600

# Срок установки исходящего соединения (секунды)
connection_establishment_timeout_secs = 30

# Простой туннелируемых TCP-соединений (секунды)
tcp_connections_timeout_secs = 604800

# Простой туннелируемых UDP-«соединений» (секунды)
udp_connections_timeout_secs = 300

# Файл пользователей
credentials_file = "credentials.toml"

# Файл правил (необязательно)
rules_file = "rules.toml"

# Добавлено в форке (необязательно)
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

# Пересылка соединений (необязательно, по умолчанию напрямую)
[forward_protocol]
direct = {}

# Обратный прокси (необязательно)
# [reverse_proxy]
# server_address = "127.0.0.1:8080"
# path_mask = "/api"
# h3_backward_compatibility = false

# ICMP (необязательно, на части систем нужен суперпользователь)
# [icmp]
# interface_name = "eth0"
# request_timeout_secs = 3
# recv_message_queue_capacity = 256

# Метрики (необязательно)
# [metrics]
# address = "127.0.0.1:1987"
# request_timeout_secs = 3
# per_client_metrics = false
```

### Файл TLS-хостов (hosts.toml)

```toml
# Основные хосты: туннель и служебные запросы
[[main_hosts]]
hostname = "vpn.example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"
# Дополнительные имена с сертификатом этого хоста (необязательно)
allowed_sni = ["alt.example.com"]

# Хосты проверки доступности (необязательно)
[[ping_hosts]]
hostname = "ping.vpn.example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"

# Хосты проверки скорости (необязательно)
[[speedtest_hosts]]
hostname = "speed.vpn.example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"

# Хосты обратного прокси (нужна [reverse_proxy] в основном файле)
# [[reverse_proxy_hosts]]
# hostname = "api.example.com"
# cert_chain_path = "certs/cert.pem"
# private_key_path = "certs/key.pem"
```

### Файл пользователей (credentials.toml)

```toml
[[client]]
username = "user1"
password = "secure_password_1"

[[client]]
username = "user2"
password = "secure_password_2"
# Необязательно: лимиты соединений этого пользователя
# max_http2_conns = 8
# max_http3_conns = 1
```

### Файл правил (rules.toml)

```toml
# Правила проверяются по порядку, действует первое подошедшее.
# Если ни одно не подошло, подключение разрешено.

# Запретить диапазон адресов
[[rule]]
cidr = "192.168.1.0/24"
action = "deny"

# Разрешить подключения с заданным префиксом TLS client random
[[rule]]
client_random_prefix = "aabbcc"
action = "allow"

# Запретить, если совпали и адрес, и client random с маской
[[rule]]
cidr = "10.0.0.0/8"
client_random_prefix = "a0b0/f0f0"
action = "deny"
```

---

## Справочник настроек

### Основные настройки

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `listen_address` | строка | `0.0.0.0:443` | Адрес и порт прослушивания |
| `ipv6_available` | логический | `true` | Можно ли маршрутизировать IPv6 |
| `allow_private_network_connections` | логический | `false` | Разрешить клиентам частную сеть сервера |
| `tls_handshake_timeout_secs` | целое | `10` | Срок TLS-рукопожатия, секунды |
| `client_listener_timeout_secs` | целое | `600` | Срок клиентского слушателя, секунды |
| `connection_establishment_timeout_secs` | целое | `30` | Срок исходящего соединения, секунды |
| `tcp_connections_timeout_secs` | целое | `604800` | Простой TCP-соединения (1 неделя) |
| `udp_connections_timeout_secs` | целое | `300` | Срок UDP-«соединения» (5 минут) |
| `credentials_file` | строка | нет | Путь к файлу пользователей |
| `rules_file` | строка | нет | Путь к файлу правил. Нет файла или он не читается: разрешено всё |
| `speedtest_enable` | логический | `false` | Проверка скорости на основных хостах |
| `speedtest_path` | строка | `/speedtest` | Начало пути проверки скорости |
| `ping_enable` | логический | `false` | Проверка доступности на основных хостах |
| `ping_path` | строка | `/ping` | Начало пути проверки доступности |
| `auth_failure_status_code` | целое | `407` | Код ответа на неудачную проверку логина для `CONNECT` |
| `non_connect_auth_failure_status_code` | целое | нет | Тот же код для запросов не `CONNECT`; если не задан, берётся `auth_failure_status_code` |
| `default_max_http2_conns_per_client` | целое | нет | Предел одновременных соединений HTTP/1 и HTTP/2 на пользователя. Клиент открывает по умолчанию 8 соединений HTTP/2. Не задан: без предела. Перекрывается `max_http2_conns` в `credentials.toml` |
| `default_max_http3_conns_per_client` | целое | нет | Предел соединений HTTP/3 на пользователя. Не задан: без предела. Перекрывается `max_http3_conns` |

Пути начинаются с `/`. Сервер не стартует, если один путь совпадает с другим или служит его началом в этих парах: `ping_path` и `speedtest_path`, `ping_path` и `xhttp_path`, `speedtest_path` и `xhttp_path`, путь подписки и каждый из `ping_path`, `speedtest_path`, `path_mask`.

`auth_failure_status_code` и `non_connect_auth_failure_status_code` принимают `407`, `405`, `404` или `403`. Значение не `407` ломает проверку логина прокси в Chrome.

Без пользователей сервер на публичном адресе не стартует, а на loopback-адресе стартует с предупреждением в журнале.

### Настройки протоколов приёма

Нужен хотя бы один протокол.

#### HTTP/1.1 (`[listen_protocols.http1]`)

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `upload_buffer_size` | целое | `32768` | Буфер исходящего трафика, байты |

#### HTTP/2 (`[listen_protocols.http2]`)

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `initial_connection_window_size` | целое | `8388608` | Окно управления потоком соединения (8 МБ) |
| `initial_stream_window_size` | целое | `131072` | Окно потока (128 КБ) |
| `max_concurrent_streams` | целое | `1000` | Одновременных потоков |
| `max_frame_size` | целое | `16384` | Наибольшая полезная нагрузка кадра |
| `header_table_size` | целое | `65536` | Размер таблицы заголовков |

#### QUIC и HTTP/3 (`[listen_protocols.quic]`)

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `recv_udp_payload_size` | целое | `1350` | Наибольшая принимаемая нагрузка UDP |
| `send_udp_payload_size` | целое | `1350` | Наибольшая отправляемая нагрузка UDP |
| `initial_max_data` | целое | `104857600` | Начальный предел данных соединения (100 МБ) |
| `initial_max_stream_data_bidi_local` | целое | `1048576` | Предел локального двунаправленного потока (1 МБ) |
| `initial_max_stream_data_bidi_remote` | целое | `1048576` | Предел удалённого двунаправленного потока (1 МБ) |
| `initial_max_stream_data_uni` | целое | `1048576` | Предел однонаправленного потока (1 МБ) |
| `initial_max_streams_bidi` | целое | `4096` | Двунаправленных потоков |
| `initial_max_streams_uni` | целое | `4096` | Однонаправленных потоков |
| `max_connection_window` | целое | `25165824` | Наибольшее окно соединения (24 МБ) |
| `max_stream_window` | целое | `16777216` | Наибольшее окно потока (16 МБ) |
| `disable_active_migration` | логический | `true` | Запретить активную смену адреса |
| `enable_early_data` | логический | `true` | Разрешить 0-RTT |
| `message_queue_capacity` | целое | `4096` | Очередь мультиплексора QUIC |

### Пересылка соединений

По умолчанию соединения идут напрямую:

```toml
[forward_protocol]
direct = {}
```

Через прокси SOCKS5:

```toml
[forward_protocol.socks5]
address = "127.0.0.1:1080"
extended_auth = false
```

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `address` | строка | нет | **Обязательна.** Адрес прокси SOCKS5 |
| `extended_auth` | логический | `false` | Расширенная проверка логина |

### Обратный прокси

Необязательно. Сервер завершает TLS на хостах `reverse_proxy_hosts` и основных хостах по пути `path_mask` и передаёт запрос по HTTP/1.1 на `server_address`. В форке обратный прокси служит сайтом-приманкой.

```toml
[reverse_proxy]
server_address = "127.0.0.1:8080"
path_mask = "/api"
h3_backward_compatibility = false
```

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `server_address` | строка | нет | **Обязательна.** Адрес исходного сервера |
| `path_mask` | строка | нет | **Обязательна.** Начало пути, должно начинаться с `/` |
| `h3_backward_compatibility` | логический | `false` | Подменять метод при переводе HTTP/3 в HTTP/1 |

Переведённые запросы несут заголовок `X-Original-Protocol` (`HTTP1` или `HTTP3`). `server_address` задаёт оператор, поэтому `allow_private_network_connections` на него не действует.

### ICMP

Необязательно. На части систем нужен суперпользователь.

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `interface_name` | строка | `eth0` (Linux), `en0` (остальные) | Сетевой интерфейс сокета ICMP |
| `request_timeout_secs` | целое | `3` | Срок ICMP-запроса, секунды |
| `recv_message_queue_capacity` | целое | `256` | Очередь на клиента |

### Метрики

Необязательно. Метрики Prometheus; подробности: [METRICS.md](METRICS.md).

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `address` | строка | `127.0.0.1:1987` | Адрес метрик |
| `request_timeout_secs` | целое | `3` | Срок запроса, секунды |
| `per_client_metrics` | логический | `false` | Ряды по пользователям и путь `/clients`. Раскрывает имена пользователей и адреса клиентов |

Пути обработчика метрик: `/health-check`, `/metrics`, `/clients` и добавленный в форке `/ja4`. `/ja4` отдаёт JSON со счётчиками отпечатков JA4 входящих ClientHello с начала работы сервера. Форк добавил и метрики `xhttp_reserved_bytes` и `xhttp_memory_rejections_total` (бюджет памяти NineLives).

### Подписка

Необязательно. Сервер отдаёт JSON с настройками конкретного пользователя по HTTPS на основном хосте.

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

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `enabled` | логический | `false` | Главный выключатель |
| `hostname` | строка | нет | Должно совпасть с `hostname` из `main_hosts` |
| `path` | строка | `/subscription` | Путь (точное совпадение); начинается с `/`, не может быть просто `/` |
| `address` | строка | нет | **Обязательна.** `host:port` или `ip:port` для JSON |
| `name` | строка | нет | Подсказка имени для клиента |
| `dns_upstreams` | массив | `[]` | Подсказка DNS-серверов |
| `custom_sni` | строка | нет | Свой SNI для клиента |
| `client_random_prefix` | строка | нет | `hex[/hex]` |

Поля проверяются при каждом запуске, в том числе при `--client_config`, если секция есть, даже при `enabled = false`. `address` служит и запасным адресом для `--client_config`.

Порядок ответов на запрос к пути подписки:

| Условие | Ответ |
| --- | --- |
| Подписка не настроена или имя хоста (SNI) не то | `404` |
| Нет или неверен логин (HTTP Basic), либо адрес клиента в списке «только сайт» | `401` |
| Подписка выключена | `403` |
| Метод не `GET` | `405` |
| Иначе | `200` и JSON только этого пользователя |

---

## Ключи форка

Добавлено в форке. В апстриме этих ключей нет. Все они необязательны и по умолчанию выключены или нейтральны.

### NineLives (XHTTP, протокол 1)

Подробно: [docs/ninelives.md](docs/ninelives.md). Ключи лежат в основном файле. NineLives не совместим с XHTTP из Xray и с клиентом TrustTunnel, слово XHTTP осталось в именах ключей по истории.

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `xhttp_enable` | логический | `false` | Включает NineLives на основных хостах |
| `xhttp_path` | строка | `/api/v2/` | Начало пути запросов; к нему добавляется идентификатор сессии, например `/api/v2/<sid>` |
| `xhttp_secret_key` | строка | нет | Seed подписи рукопожатия: base64 от 32 байт. Без верного ключа клиент, начавший обмен ключами, получает `400` |
| `xhttp_resume_timeout` | целое, секунды | `60` | Сколько сервер хранит сессию после обрыва соединения. `0`: сессия не продолжается, клиент получает `400` |
| `xhttp_unacked_limit` | целое, байты | `1048576` | Неподтверждённые байты на сессию; при включённом NineLives не меньше `65536` |
| `xhttp_memory_limit` | целое, байты | четверть памяти, от 64 до 512 МиБ | Общий бюджет буферов NineLives и CatTail. Берётся меньшее из физической памяти и предела cgroup; если оба неизвестны, считается от 256 МиБ, то есть 64 МиБ. Должен быть больше нуля |
| `xhttp_supported_versions` | массив | `[1]` | Только `[1]`: другое значение не проходит проверку |
| `transport_mode` | строка | `"xhttp"` | Только `"xhttp"`: другое значение не проходит проверку |

`xhttp_secret_key` при запуске не проверяется, неверное значение проявляется отказом при обмене ключами. Путь `xhttp_path` не должен пересекаться с `ping_path` и `speedtest_path`.

### CatTail

Необязательный внешний слой на отдельном TCP-порту: [docs/cattail.md](docs/cattail.md). Без секции `[cattail]` порт не открывается.

```toml
[cattail]
listen_address = "0.0.0.0:20443"
key = "<base64 от 32 случайных байт>"
# необязательно
pattern = """
stop=3
0=400-1400,c,200-900
1=300-1200,c,100-700
2=100-600,c
mid=0-32
end=0-96
"""
```

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `listen_address` | строка | нет | **Обязательна.** TCP-адрес слушателя CatTail; не должен совпадать с основным `listen_address`. Диапазон портов на него отображает файрвол хоста |
| `key` | строка | нет | **Обязателен.** Ключ сервера: base64 от 32 байт |
| `pattern` | строка | схема, показанная выше | Схема набивки направления сервера |

Неверные ключ или схема не дают серверу запуститься.

### Адреса «только сайт»

| Настройка | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `decoy_only_ips_file` | строка | нет | Файл со списком адресов и подсетей |

В файле один адрес или подсеть (CIDR) на строку, `#` начинает комментарий, строки, которые не разобрались, пропускаются. Сервер проверяет время изменения файла не чаще раза в 5 секунд и перечитывает файл, когда оно изменилось. Нет файла или он не читается: список пуст. `SIGHUP` для этого не нужен.

С адреса из списка сайт-приманка показывается как обычно, а любой запрос к VPN отвечает как на неверный пароль, даже при верных данных: `CONNECT`, NineLives и подписка (`401` у подписки), вход по учётным данным в SNI. На порт CatTail такой адрес получает молчание.

```text
 запрос с адреса из decoy_only_ips_file
        |
        v
 TLS-рукопожатие и сайт-приманка: как у всех
        |
        +-- CONNECT, NineLives, учётные данные в SNI --> ответ как на неверный пароль
        +-- подписка ------------------------------------> 401
        +-- порт CatTail --------------------------------> молчание
```

Рисунок 1. Что получает адрес из списка «только сайт».

### Остальное, что добавлено форком

| Где | Ключ | По умолчанию | Описание |
| --- | --- | --- | --- |
| `hosts.toml` | `allowed_sni` | `[]` | Дополнительные имена хоста (есть и в апстриме) |
| `rules.toml` | `client_random_auth_key` | нет | См. [Правила](#правила) (есть и в апстриме) |

---

## TLS-хосты

У каждого хоста три обязательных поля и одно необязательное:

| Поле | Тип | Описание |
| --- | --- | --- |
| `hostname` | строка | **Обязательно.** Имя для сопоставления с SNI; уникально среди всех хостов |
| `cert_chain_path` | строка | **Обязательно.** Файл PEM с цепочкой сертификатов |
| `private_key_path` | строка | **Обязательно.** Файл PEM с ключом |
| `allowed_sni` | массив | Дополнительные имена с сертификатом этого хоста |

Сертификат и ключ читаются при запуске; нечитаемый файл не даёт запуститься. Нужен хотя бы один основной хост.

### Виды хостов

- **`main_hosts`**: основные хосты, туннель и служебные запросы.
- **`ping_hosts`**: отвечают `200 OK` на HTTPS `GET` (проверка доступности).
- **`speedtest_hosts`**: проверка скорости:
    - `GET /Nmb.bin` (N от 1 до 100): отдать N мегабайт;
    - `POST /upload.html`: приём до 120 МБ.
- **`reverse_proxy_hosts`**: пересылают на обратный прокси; без `[reverse_proxy]` игнорируются.

На основных хостах запрос направляется по пути в таком порядке:

```text
 запрос на основной хост
        |
        v
 путь подписки (точное совпадение)       --> подписка
 ping_path (если ping_enable)            --> проверка доступности
 speedtest_path (если speedtest_enable)  --> проверка скорости
 xhttp_path (если xhttp_enable)          --> NineLives
 path_mask обратного прокси              --> обратный прокси
 всё остальное                           --> туннель (CONNECT)
```

Рисунок 2. Выбор обработчика на основном хосте. Совпадение по началу пути, кроме подписки.

---

## Правила

Правила фильтруют входящие соединения по адресу клиента и по полю TLS `client_random`.

### Устройство правила

```toml
[[rule]]
cidr = "192.168.0.0/16"           # необязательно: диапазон адресов
client_random_prefix = "aabbcc"   # необязательно: префикс или префикс/маска (hex)
client_random_auth_key = "0011..." # необязательно: ключ (hex) для client_random, выводимого из SNI
action = "allow"                  # обязательно: "allow" или "deny"
```

### Порядок проверки

1. Правила проверяются по порядку.
2. Действует первое подошедшее.
3. Если ни одно не подошло, подключение **разрешено**.
4. Если заданы `cidr` и условие по `client_random`, должны совпасть оба.
5. `client_random_prefix` и `client_random_auth_key` взаимоисключающие: если заданы оба, действует только ключ.
6. Если в файле есть правило с префиксом или ключом, соединение без `client_random` запрещается сразу.

### Сопоставление client random

**Простой префикс:**

```toml
client_random_prefix = "aabbcc"
```

Совпадает, если `client_random` начинается с `0xaabbcc`.

**Префикс с маской:**

```toml
client_random_prefix = "a0b0/f0f0"
```

Побайтово сравнивается `client_random & маска` с `префикс & маска` в пределах длины маски и префикса.

**Ключ (client_random_auth_key):**

```toml
client_random_auth_key = "00112233445566778899aabbccddeeff"
```

Совпадает, если последние 16 байт 32-байтного `client_random` равны

```text
AES-128-ECB(SHA256(SNI)[0:16], HKDF-SHA256(secret = client_random_auth_key, salt = client_random[0:16], info = "tls13 encryption context"))
```

Первые 16 байт случайны в каждом соединении, поэтому `client_random` каждый раз другой, а проверка детерминирована.

Ограничения:

- Ключ: непустой hex чётной длины. Рекомендуется 16 байт (32 символа) и больше; мастер настройки отказывается создавать правило с более коротким ключом.
- Проверка зависит от SNI. Если в файле есть правило с ключом, соединения без SNI или без `client_random` запрещаются.
- Берётся SNI из ClientHello. Если у клиента `custom_sni`, проверяется он.
- Ключ равен паролю: кто его знает, тот проходит проверку. Он попадает в экспортируемые настройки и ссылки, храните их как секрет.
- Клиент должен уметь поле `client_random_auth_key`.

```text
 клиент                                         сервер
   |  ClientHello: client_random =                 |
   |    [16 случайных байт | AES-128-ECB(SHA256(SNI)[0:16], HKDF(key, эти 16 байт))]
   |---------------------------------------------->|
   |                     сервер выводит те же 16 байт из ключа правила
   |                     и SNI; совпало: правило подходит, действует action
```

Рисунок 3. Проверка `client_random_auth_key`.

### Примеры

```toml
# Запретить диапазон адресов
[[rule]]
cidr = "192.168.1.0/24"
action = "deny"

# Разрешить префикс client random
[[rule]]
client_random_prefix = "deadbeef"
action = "allow"

# Запретить внутренние сети с заданной подписью клиента
[[rule]]
cidr = "10.0.0.0/8"
client_random_prefix = "bad0/ff00"
action = "deny"

# Пускать только тех, кто доказал знание ключа, привязанного к SNI
[[rule]]
client_random_auth_key = "00112233445566778899aabbccddeeff"
action = "allow"

# Запретить всё остальное (ставить последним)
[[rule]]
action = "deny"
```

---

## Работа сервера

### Перечитывание настроек

Сигнал `SIGHUP` перечитывает файл TLS-хостов и секцию `[subscription]` основного файла без перезапуска:

```bash
kill -HUP $(pidof trusttunnel_endpoint)
```

Если новая `[subscription]` не разбирается или не проходит проверку, остаётся прежняя, а в журнал пишется предупреждение. Остальные настройки применяются при перезапуске. Список `decoy_only_ips_file` перечитывается сам.

### Служба systemd

Шаблон службы лежит в [scripts/trusttunnel.service.template](scripts/trusttunnel.service.template). Он рассчитан на каталог `/opt/trusttunnel` и файлы `vpn.toml`, `hosts.toml`:

```bash
sudo cp /opt/trusttunnel/trusttunnel.service.template /etc/systemd/system/trusttunnel.service
sudo systemctl daemon-reload
sudo systemctl enable --now trusttunnel

# Перечитать TLS-хосты и подписку (в шаблоне нет ExecReload, поэтому reload не работает)
sudo systemctl kill -s HUP trusttunnel

# Журнал
sudo journalctl -u trusttunnel -f
```

Установка и обновление: [README.md](README.md#установка-и-запуск). Скрипт `scripts/install.sh` в этом дереве взят из апстрима и скачивает архивы апстрима, им не пользуйтесь.

---

## См. также

- [README.md](README.md): установка и запуск
- [docs/features.md](docs/features.md): что возможностям CatTunnel нужно от сервера
- [docs/upstream-divergence.md](docs/upstream-divergence.md): отличия от апстрима
- [DEVELOPMENT.md](DEVELOPMENT.md): разработка
- [PROTOCOL.md](PROTOCOL.md): протокол
- [CHANGELOG.md](CHANGELOG.md): изменения
