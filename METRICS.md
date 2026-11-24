English: [METRICS.en.md](METRICS.en.md)

# Метрики сервера

Сервер CatTunnel (`trusttunnel_endpoint`) отдаёт метрики в формате Prometheus по обычному HTTP/1, без TLS. Слушатель метрик выключен, пока в `vpn.toml` нет секции `[metrics]`. Метрики из TrustTunnel работают так же, как в [апстриме](https://github.com/TrustTunnel/TrustTunnel/blob/v1.3.1/METRICS.md). Форк добавляет два показателя бюджета памяти NineLives и CatTail (`xhttp_reserved_bytes`, `xhttp_memory_rejections_total`) и путь `/ja4` со счётчиком отпечатков ClientHello.

## Содержание

- [Включение](#включение)
- [Пути слушателя](#пути-слушателя)
- [Метрики](#метрики)
- [Бюджет памяти NineLives и CatTail](#бюджет-памяти-ninelives-и-cattail)
- [Метрики по пользователям и /clients](#метрики-по-пользователям-и-clients)
- [Отпечатки ClientHello: /ja4](#отпечатки-clienthello-ja4)
- [Использование как библиотеки](#использование-как-библиотеки)
- [Если метрик нет или они странные](#если-метрик-нет-или-они-странные)
- [См. также](#см-также)

## Включение

Добавьте секцию в `vpn.toml`:

```toml
[metrics]
address = "127.0.0.1:1987"
request_timeout_secs = 3
per_client_metrics = false
```

Значения в примере совпадают с умолчаниями. Если секция есть, а ключей в ней нет, действуют те же умолчания.

| Ключ | Умолчание | Что делает |
| --- | --- | --- |
| `address` | `127.0.0.1:1987` | Адрес и порт слушателя метрик |
| `request_timeout_secs` | `3` | Сколько секунд слушатель ждёт запрос после подключения |
| `per_client_metrics` | `false` | Включает ряды `*_per_user` и путь `/clients` |

Слушатель не проверяет ни логин, ни метод запроса: любой, кто достучится до порта, видит все метрики. Поэтому оставляйте `address` на `127.0.0.1` или закрывайте порт в файрволе. С `per_client_metrics = true` метрики показывают имена пользователей, а `/clients` ещё и IP-адреса клиентов, так что слушатель стоит защитить особенно тщательно.

Если порт занят или адрес не удалось открыть, сервер завершается с ошибкой `Metrics listener failure`, а не работает без метрик.

## Пути слушателя

| Путь | Ответ |
| --- | --- |
| `/metrics` | Метрики в текстовом формате Prometheus |
| `/clients` | JSON по пользователям; только при `per_client_metrics = true`, иначе `404` |
| `/ja4` | JSON с количеством ClientHello по отпечаткам JA4 (добавлено в форке) |
| `/health-check` | `200 OK`, пока сервер работает |

На любой другой путь слушатель отвечает `400 Bad Request`.

```text
 соединение клиента (CONNECT)            слушатель метрик, 127.0.0.1:1987
   сессия открыта ---> client_sessions        GET /metrics ------> текст Prometheus
   байты от клиента -> inbound_traffic_bytes  GET /clients ------> JSON по пользователям
   байты клиенту ----> outbound_traffic_bytes GET /ja4 ---------> JSON по отпечаткам
   выход к цели -----> outbound_tcp_sockets   GET /health-check -> 200 OK
                       outbound_udp_sockets
 TLS-соединение на TCP
   ClientHello ------> счётчик JA4
 NineLives и CatTail
   резерв буфера ----> xhttp_reserved_bytes
   отказ в резерве --> xhttp_memory_rejections_total
```

Рисунок 1. Откуда берутся значения и по каким путям их отдаёт слушатель.

## Метрики

| Имя | Тип | Метки | Что считает |
| --- | --- | --- | --- |
| `client_sessions` | gauge | `protocol_type` | Открытые сессии клиентов |
| `inbound_traffic_bytes` | counter | `protocol_type` | Байты, загруженные клиентами (клиент, сервер, цель) |
| `outbound_traffic_bytes` | counter | `protocol_type` | Байты, скачанные клиентами (цель, сервер, клиент) |
| `outbound_tcp_sockets` | gauge | нет | Исходящие TCP-соединения сервера к целям |
| `outbound_udp_sockets` | gauge | нет | Исходящие UDP-сокеты сервера |
| `xhttp_reserved_bytes` | gauge | нет | Занятая часть бюджета памяти NineLives и CatTail, байты (форк) |
| `xhttp_memory_rejections_total` | counter | нет | Отказы в резерве из этого бюджета (форк) |
| `client_sessions_per_user` | gauge | `username`, `protocol_type` | Сессии по пользователям, только с `per_client_metrics` |
| `inbound_traffic_bytes_per_user` | counter | `username` | Загрузка по пользователям, только с `per_client_metrics` |
| `outbound_traffic_bytes_per_user` | counter | `username` | Скачивание по пользователям, только с `per_client_metrics` |

Метка `protocol_type` принимает значения `HTTP1`, `HTTP2` и `HTTP3` (заглавными).

Пример ответа `/metrics` без `per_client_metrics` (числа выдуманы):

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

Ряды с метками появляются после первого события: пока не было ни одной сессии, `client_sessions` в ответе нет. Сокеты и два показателя памяти есть всегда, потому что у них нет меток.

Как считаются значения:

- `client_sessions`, `inbound_traffic_bytes` и `outbound_traffic_bytes` относятся к туннелю CONNECT. Сессия считается с открытия соединения-туннеля до его закрытия. Байты NineLives в эти два счётчика не попадают: вызовов подсчёта в `lib/src/http_xhttp.rs` нет.
- `outbound_tcp_sockets` растёт в момент попытки соединиться с целью, до того как соединение установлено, и убывает при закрытии или неудаче. В режиме SOCKS5 счётчик растёт, когда прокси ответил успехом на CONNECT.
- `outbound_udp_sockets` при прямой пересылке равен числу записей «источник и приёмник», при SOCKS5 числу UDP-ассоциаций, по одной на источник.

## Бюджет памяти NineLives и CatTail

Буферы сессий NineLives и соединений CatTail берутся из одного общего бюджета. Его размер задаёт ключ `xhttp_memory_limit` в `vpn.toml`. Если ключа нет, берётся четверть меньшего из объёма физической памяти и лимита cgroup, но не меньше 64 МиБ и не больше 512 МиБ; когда объём определить не удалось, это 64 МиБ.

Каждая сессия NineLives сразу резервирует 256 КиБ, потом дозаказывает память по мере роста буфера чтения. Соединение CatTail резервирует фиксированный кусок на время своей жизни. Подробнее: [docs/ninelives.md](docs/ninelives.md) и [docs/cattail.md](docs/cattail.md).

```text
 запрос резерва N байт (сессия NineLives, рост буфера чтения, соединение CatTail)
   |
   v
 уже занято + N <= xhttp_memory_limit ?
   |-- да --> xhttp_reserved_bytes += N
   |          при освобождении: xhttp_reserved_bytes -= N
   '-- нет --> xhttp_memory_rejections_total += 1
                новая сессия NineLives: ответ 503
                соединение CatTail: молчание, ни байта в ответ
                рост буфера чтения: ошибка для этой сессии
```

Рисунок 2. Один резерв из бюджета памяти и два показателя, которые он меняет.

`xhttp_reserved_bytes` показывает, сколько бюджета занято сейчас. Если он долго держится у значения `xhttp_memory_limit`, а `xhttp_memory_rejections_total` растёт, бюджет мал для нагрузки: поднимите `xhttp_memory_limit`. Отказ счётчик фиксирует при каждой неудачной попытке, поэтому одна перегруженная сессия может добавить несколько единиц.

## Метрики по пользователям и /clients

Опция `per_client_metrics` (по умолчанию `false`) добавляет три ряда `*_per_user` с меткой `username` и путь `/clients`. Пока имя пользователя не известно (например, у туннеля с заголовком `Proxy-Authorization` до первого запроса), метка `username` пустая.

Пример `/metrics` с `per_client_metrics = true` дополнительно содержит:

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

`/clients` отдаёт JSON-список:

```json
[{"username":"alice","ip":"203.0.113.7","sessions":3,"inbound":123456,"outbound":789012},
 {"username":"bob","ip":null,"sessions":0,"inbound":0,"outbound":0}]
```

- В список входят все пользователи из `credentials.toml` и все, у кого сейчас есть соединение. Пользователь без сессий остаётся в списке с нулевыми счётчиками, так что в мониторинге видно неиспользуемые учётные записи.
- `inbound` и `outbound` суммируются с запуска сервера, `sessions` показывает текущее число сессий.
- `ip` один из адресов, с которых пользователь сейчас подключён; при нескольких адресах выбор произвольный. Значение `null` значит, что адрес не известен: так бывает у настроенного пользователя без соединений и у HTTP/3, где адрес клиента не удалось определить.

## Отпечатки ClientHello: /ja4

Сервер считает отпечаток JA4 у каждого ClientHello, пришедшего по TCP, и ведёт счётчик по отпечаткам. Так видно, какие TLS-профили клиенты шлют на самом деле: браузер, старый телефон, приложение CatTunnel. Каждый отпечаток также пишется в журнал на уровне `debug`. Считаются только соединения по TCP: QUIC в этот счёт не входит.

`/ja4` отдаёт JSON-список пар `[отпечаток, количество]`, отсортированный по отпечатку. Счёт идёт с запуска сервера и не сохраняется между перезапусками:

```json
[["t13d1516h2_000000000000_000000000000",41],["t13d1517h2_111111111111_222222222222",7]]
```

Отпечаток состоит из трёх частей. Префикс `t13d1516h2` читается так: `t` (TCP), версия TLS `13`, `d` если в ClientHello есть SNI (иначе `i`), число шифронаборов `15`, число расширений `16`, первая и последняя буквы первого ALPN `h2`. Затем идут два хеша SHA-256, обрезанные до 12 знаков. Описание формата: [JA4 от FoxIO](https://github.com/FoxIO-LLC/ja4). Значения в примере выдуманы.

## Использование как библиотеки

Если вы встраиваете крейт `trusttunnel` (каталог `lib`), слушатель включается настройкой `Settings::metrics`. Поля `MetricsSettings` закрыты, поэтому настройки собирает построитель:

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
    // остальные настройки сервера
    .metrics(metrics)
    .build()
    .unwrap();
```

## Если метрик нет или они странные

- Порт не отвечает: проверьте, что в `vpn.toml` есть секция `[metrics]` и что вы обращаетесь на тот же `address`. Занятый порт даёт ошибку `Metrics listener failure` при запуске, поэтому смотрите журнал сервера.
- Ответ `400`: путь не из таблицы выше.
- Ответ `404` на `/clients`: `per_client_metrics` выключен.
- Нет `client_sessions` и счётчиков трафика: сессий CONNECT ещё не было, а NineLives эти счётчики не меняет.
- Нет рядов `*_per_user`: включите `per_client_metrics` и дождитесь первой сессии.
- `xhttp_memory_rejections_total` растёт: бюджет памяти исчерпан, см. раздел выше.

## См. также

- [CONFIGURATION.md](CONFIGURATION.md): справочник настроек сервера
- [PROTOCOL.md](PROTOCOL.md): протоколы
- [DEVELOPMENT.md](DEVELOPMENT.md): сборка и тесты
- [docs/features.md](docs/features.md): возможности форка и ключи
- [docs/upstream-divergence.md](docs/upstream-divergence.md): чем форк отличается от TrustTunnel
