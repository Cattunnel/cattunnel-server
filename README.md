# CatTunnel server

CatTunnel server: VPN-сервер, к которому подключаются приложения CatTunnel. Это форк TrustTunnel endpoint (версия 1.3.0-beta.2), протокол тот же: TLS, поверх него HTTP/2 (или HTTP/3 по QUIC), и каждое TCP-соединение пользователя идёт отдельным потоком `CONNECT`. Форк добавляет NineLives, второй транспорт из пары HTTP-запросов, необязательный внешний слой CatTail и меняет TLS-приёмник, сайт-приманку и ответ на `CONNECT`.

[English](README.en.md)

Это дерево: `1.3.0-beta.2-44-gb30fe98`, сервер к CatTunnel 1.5.9.5.

```text
 клиент CatTunnel                        CatTunnel server :443
   TLS + HTTP/2 ----------------------->  TLS-приёмник
   поток CONNECT на каждое соединение       |
   приложения или пара GET/POST             +-- верный логин: туннель
   (NineLives)                              |     CONNECT, DATA по 4096 Б,
                                            |     или NineLives
                                            +-- нет логина, адрес из
                                                «только сайт»: сайт-приманка

 по желанию: отдельный порт CatTail
   кадры CatTail (без заголовков TLS и SNI) -> снять слой -> тот же TLS
```

## Репозитории CatTunnel

| Репозиторий | Что это |
| --- | --- |
| [Cattunnel/cattunnel](https://github.com/Cattunnel/cattunnel) | Приложение для Android, Windows и Linux |
| [Cattunnel/cattunnel-router](https://github.com/Cattunnel/cattunnel-router) | Роутерная редакция для OpenWrt, Keenetic и ASUS Merlin |
| [Cattunnel/cattunnel-server](https://github.com/Cattunnel/cattunnel-server) | Сервер (этот репозиторий) |

## Содержание

- [Имена, оставшиеся от апстрима](#имена-оставшиеся-от-апстрима)
- [Возможности](#возможности)
- [Готовые сборки](#готовые-сборки)
- [Сборка](#сборка)
- [Установка и запуск](#установка-и-запуск)
- [Обновление](#обновление)
- [Настройки](#настройки)
- [Конфигурация для клиента](#конфигурация-для-клиента)
- [Docker](#docker)
- [Документация](#документация)
- [Лицензия](#лицензия)

## Имена, оставшиеся от апстрима

Бинарники, крейты, файлы настроек и служба systemd пока называются как в апстриме, чтобы старые конфигурации, скрипты и клиенты продолжали работать: сервер `trusttunnel_endpoint`, библиотека `trusttunnel`, крейт ссылок `trusttunnel-deeplink`, каталог в примерах `/opt/trusttunnel`, служба `trusttunnel.service`. В некоторых справках бинарники ещё пишут «TrustTunnel». Такое имя в этой документации означает конкретный файл или идентификатор, а не другой продукт.

## Возможности

Коротко. Что каждой возможности нужно от сервера и какими ключами она включается: [docs/features.md](docs/features.md).

**Из апстрима**

- Туннель для TCP, UDP и ICMP поверх HTTP/1.1, HTTP/2 и HTTP/3 (QUIC).
- Вход по логину и паролю (`credentials.toml`), правила подключения по IP клиента и по полю TLS `client_random` (`rules.toml`), в том числе `client_random_auth_key`: значение выводится из общего ключа заново для каждого соединения, и для наблюдателя поле остаётся случайным.
- Сайт-приманка для всех, у кого нет верного логина (обратный прокси на локальный веб-сервер).
- Несколько имён на один TLS-хост (`allowed_sni`) с сертификатом этого хоста.
- Выгрузка настроек клиента ссылкой `tt://` или файлом TOML, JSON-подписка на каждого пользователя (`[subscription]`), метрики Prometheus, перечитывание TLS-хостов по `SIGHUP`.

**Добавлено в форке**

- **Транспорт NineLives (XHTTP, протокол 1).** TCP-сессия клиента идёт двумя HTTP/2-запросами на один путь: `GET`, в теле ответа которого идёт загрузка, и `POST`, в теле запроса которого идёт отдача. У кадров есть смещения и подтверждения. Необязательное подписанное рукопожатие (обмен ключами X25519, подпись сервера Ed25519, HKDF-SHA256, AES-128-GCM на каждое направление) шифрует поток кадров внутри TLS. После обрыва сессию можно продолжить (по умолчанию в течение 60 с). Буферы ограничены общим бюджетом памяти. Без `xhttp_enable = true` NineLives выключен. С XHTTP из Xray он **не совместим**, слово XHTTP в ключах осталось по истории. Подробно: [docs/ninelives.md](docs/ninelives.md).
- **Ответ на `CONNECT`.** Ответ `200` несёт `date`, `server`, `cache-control` и два случайных идентификатора случайной длины, а не голую строку статуса. С 1.5.9.5 данные ответа идут кадрами DATA по 4096 байт, ближе к тому, как браузер получает файл.
- **CatTail** (1.5.9.5, по желанию). Внешний слой на отдельном порту: клиент заворачивает свои TLS-байты в кадры с аутентификацией и случайной набивкой, на проводе нет заголовков TLS, SNI и постоянного префикса. Сервер снимает слой и отдаёт байты тому же TLS-приёмнику. Без секции `[cattail]` выключен. Спецификация и тестовые векторы: [docs/cattail.md](docs/cattail.md).
- **Профиль TLS в ссылке.** Тег `0x10` в `tt://` передаёт клиенту числовой профиль TLS; клиенты без поддержки тега его пропускают.
- **TLS-приёмник.** ClientHello, разбитый на несколько TLS-записей, собирается до разбора, так что клиент может его разбивать. Не-TLS отбрасывается сразу.
- **Сайт-приманка.** Правильно проходят keep-alive HTTP/1.1, тела частями (chunked) и трейлеры, длина строк размера и трейлеров ограничена, на хостах обратного прокси разрешён HTTP/2.
- **Адреса «только сайт».** Файл из ключа `decoy_only_ips_file` (адреса и подсети по одному в строке, `#` для комментариев) перечитывается, когда меняется. Клиенту с такого адреса сервер на любой запрос к VPN (`CONNECT`, NineLives, подписка) отвечает так же, как на неверный пароль, а сайт-приманку показывает как всем. Список заполняет внешний скрипт, например из ipset адресов, которые стучались в закрытые порты.
- **Метрики.** Считаются отпечатки JA4 входящих ClientHello, выгружается бюджет памяти NineLives (`xhttp_reserved_bytes`, `xhttp_memory_rejections_total`).

**Возможности клиента, которым от сервера ничего не нужно**

Паддинг HTTP/2, пул соединений, TLS-профили браузеров и режимы анти-DPI клиента CatTunnel работают и с этим сервером, и с немодифицированным апстримом. Серверу достаточно принимать стандартный паддинг HTTP/2 и несколько TLS-соединений от одного клиента, что он и делает. Подробно: [docs/features.md](docs/features.md#возможности-клиента).

## Готовые сборки

В [релизах](https://github.com/Cattunnel/cattunnel-server/releases) лежат статические сборки для Linux: `trusttunnel-v<версия>-linux-x86_64.tar.gz` и `trusttunnel-v<версия>-linux-aarch64.tar.gz` (внутри `trusttunnel_endpoint` и LICENSE) и `SHA256SUMS`:

```shell
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf trusttunnel-v1.5.9.5-linux-x86_64.tar.gz
```

## Сборка

Собрать из исходников:

Нужно (см. [DEVELOPMENT.md](DEVELOPMENT.md)): Rust 1.95 (зафиксирован в `rust-toolchain.toml`), CMake, libclang и компилятор C++. Поддерживаются Linux и macOS.

```shell
git clone https://github.com/Cattunnel/cattunnel-server.git
cd cattunnel-server
cargo build --bins --release
```

Получится `target/release/trusttunnel_endpoint`; мастер настройки встроен в него (`--setup-wizard`). То же через Makefile: `make endpoint/build`, мастер: `make endpoint/setup`.

Тесты и линтеры: `make test`, `make lint` (нужны `markdownlint-cli`, rustfmt и clippy).

## Установка и запуск

### Быстрая установка

Скрипт скачивает готовую сборку CatTunnel server из [релизов](https://github.com/Cattunnel/cattunnel-server/releases), сверяет архив с `SHA256SUMS` и ставит бинарник в `/opt/trusttunnel`:

```shell
curl -fsSL https://raw.githubusercontent.com/Cattunnel/cattunnel-server/refs/heads/main/scripts/install.sh | sh -s -
```

После `sh -s -` можно указать параметры:

| Параметр | Действие |
| --- | --- |
| `-V 1.5.9.5` | Установить указанную версию вместо последнего релиза |
| `-o DIR` | Выбрать каталог вместо `/opt/trusttunnel` |
| `-p archive` | Использовать локальный архив. Если рядом есть `SHA256SUMS`, проверить хеш; иначе показать предупреждение |
| `-u` | Удалить бинарник, не удаляя настройки и сертификаты |
| `-a y\|n` | Заранее ответить на вопрос о смене владельца каталога |
| `-v`, `-h` | Подробный вывод, справка |

```text
скрипт -> определить ОС и архитектуру -> выбрать последний релиз или -V
       -> скачать архив и SHA256SUMS -> проверить хеш
       -> распаковать во временный каталог -> установить бинарник
       -> оставить существующие настройки и сертификаты на месте
```

`SHA256SUMS` скачивается из того же релиза и не подписан. Проверка выявляет повреждение при загрузке, но не защищает от подмены самого релиза. Готовые сборки есть для Linux x86_64 и aarch64; сборок для macOS нет. При установке из исходников скопируйте `target/release/trusttunnel_endpoint` и `scripts/trusttunnel.service.template` в выбранный каталог вручную.

### Что дальше

Создайте настройки мастером. Он спросит адрес, пользователей, правила и сертификат (Let's Encrypt, самоподписанный или готовые файлы):

```shell
cd /opt/trusttunnel
sudo ./trusttunnel_endpoint --setup-wizard
```

`sudo` нужен для работы с сертификатами. `./trusttunnel_endpoint --setup-wizard -h` покажет параметры для запуска без вопросов. Продление сертификата через Certbot: [CERT_RENEWAL.md](CERT_RENEWAL.md).

Запуск службой systemd. Шаблон рассчитан на бинарник и файлы настроек в `/opt/trusttunnel` с именами по умолчанию; если пути другие, поправьте его:

```shell
sudo cp /opt/trusttunnel/trusttunnel.service.template /etc/systemd/system/trusttunnel.service
sudo systemctl daemon-reload
sudo systemctl enable --now trusttunnel
```

## Обновление

Запустите ту же команду установки снова. Скрипт заменит бинарник, но оставит существующие `vpn.toml`, `hosts.toml`, `rules.toml`, `credentials.toml` и сертификаты. Работающий сервер продолжит использовать прежний бинарник до перезапуска службы. Перезапускайте её в подходящее время:

```shell
sudo systemctl restart trusttunnel
```

## Настройки

Сервер читает файлы TOML: `vpn.toml` (основные настройки), `hosts.toml` (TLS-хосты), `credentials.toml` (пользователи) и, если нужно, `rules.toml` (правила подключения). Полный справочник: [CONFIGURATION.md](CONFIGURATION.md).

Ключи, которые добавил или использует форк:

| Файл | Ключ | По умолчанию | Что делает |
| --- | --- | --- | --- |
| `vpn.toml` | `xhttp_enable` | `false` | Включает NineLives |
| `vpn.toml` | `xhttp_path` | `"/api/v2/"` | Начало пути запросов NineLives |
| `vpn.toml` | `xhttp_secret_key` | нет | Seed Ed25519 (32 байта, base64), которым подписывается рукопожатие NineLives |
| `vpn.toml` | `xhttp_resume_timeout` | `60` | Сколько секунд сессия ждёт продолжения; `0` выключает продолжение |
| `vpn.toml` | `xhttp_memory_limit` | 1/4 памяти, 64..512 МиБ | Общий бюджет буферов NineLives, байты |
| `vpn.toml` | `xhttp_unacked_limit` | `1048576` | Неподтверждённые байты на сессию |
| `vpn.toml` | `xhttp_supported_versions` | `[1]` | Есть только протокол 1 |
| `vpn.toml` | `transport_mode` | `"xhttp"` | Режим приёмника NineLives; остальные значения зарезервированы |
| `vpn.toml` | `decoy_only_ips_file` | нет | Файл с адресами, которым VPN отвечает как на неверный пароль |
| `hosts.toml` | `allowed_sni` | `[]` | Дополнительные имена основного хоста |
| `rules.toml` | `client_random_auth_key` | нет | Ключ (hex) для проверки `client_random` |

## Конфигурация для клиента

Сервер печатает настройки для пользователя из `credentials.toml`:

```shell
cd /opt/trusttunnel
./trusttunnel_endpoint vpn.toml hosts.toml -c <client_name> -a <address>
```

`<address>`: `ip`, `ip:port`, `domain` или `domain:port`. По умолчанию выводится ссылка `tt://?` ([DEEP_LINK.md](DEEP_LINK.md)), которую приложение CatTunnel берёт из текста или QR-кода. `--format toml` печатает файл TOML для консольного клиента. Другие параметры: `--name`, `--dns-upstream`, `--client-random-auth-key`, `--subscription-url`, `--subscription-only`; см. `./trusttunnel_endpoint -h` и [CONFIGURATION.md](CONFIGURATION.md#параметры-командной-строки).

В такой ссылке нет настроек NineLives. Клиенту, который должен ходить через NineLives, в ссылке нужны `xhttp_enable`, путь и открытый ключ сервера: приложение CatTunnel читает их из тегов `tt://` `0x4F`, `0x50` и `0x51` (см. [docs/ninelives.md](docs/ninelives.md#клиент)).

Ссылка с адресом подписки содержит логин и пароль HTTP Basic Auth. Храните такие ссылки как секрет.

## Docker

`Dockerfile` собирает сервер из этого дерева. `docker-compose.yml` пробрасывает порт 443 хоста на порт 8443 контейнера и держит настройки в томе:

```shell
docker compose up -d --build
```

Без файлов настроек контейнер запускает `trusttunnel_endpoint --setup-wizard` (с вопросами, если есть TTY, иначе из переменных окружения `TT_*`, см. `docker-entrypoint.sh`). Подробно: [DEVELOPMENT.md](DEVELOPMENT.md#run-with-external-configuration-cicd).

## Документация

| Файл | Что в нём |
| --- | --- |
| [docs/features.md](docs/features.md) | Что возможностям CatTunnel нужно от сервера, ключи |
| [docs/ninelives.md](docs/ninelives.md) | NineLives: как устроен, ключи, создание ключа |
| [docs/xhttp-protocol.md](docs/xhttp-protocol.md) | Формат NineLives на проводе |
| [docs/cattail.md](docs/cattail.md) | CatTail: спецификация слоя, тестовые векторы |
| [docs/connect.md](docs/connect.md) | Транспорт CONNECT и схемы обмена |
| [docs/upstream-divergence.md](docs/upstream-divergence.md) | Отличия от TrustTunnel |
| [CONFIGURATION.md](CONFIGURATION.md) | Все настройки и параметры командной строки |
| [PROTOCOL.md](PROTOCOL.md) | Базовый протокол туннеля, транспорт `CONNECT` |
| [DEEP_LINK.md](DEEP_LINK.md) | Формат ссылок `tt://` |
| [CERT_RENEWAL.md](CERT_RENEWAL.md) | Продление сертификата через Certbot |
| [METRICS.md](METRICS.md) | Метрики Prometheus |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Сборка, кросс-компиляция, Docker, бенчмарки |
| [CHANGELOG.md](CHANGELOG.md) | Изменения форка, затем история апстрима |

## Лицензия

Apache License 2.0, см. [LICENSE](LICENSE).

CatTunnel server основан на TrustTunnel от AdGuard (Apache License 2.0) и изменён. Уведомления об авторских правах в исходниках сохранены. CatTunnel: независимый проект, с AdGuard не связан и им не одобрен. «TrustTunnel» и «AdGuard»: товарные знаки их владельцев.
