# NineLives (XHTTP, протокол 1)

Внутреннее имя XHTTP историческое: протокол не совместим с XHTTP из Xray, его ссылки и конфигурации не принимаются.
Этот документ описывает wire format протокола 1 (NineLives) в выпуске CatTunnel 1.5.9.
Версии 2 и 3 и режим CDN зарезервированы, но не реализованы.
CONNECT использует свой прежний протокол и не участвует в согласовании XHTTP.

## Транспорт и выбор версии

Одна TCP-сессия назначения использует два HTTP/2 streams с одинаковым путём:
GET `<xhttp_path><sid>` несёт downlink в response body;
POST с тем же путём несёт uplink в request body. SID — 1..64 ASCII символа
из `A-Z`, `a-z`, `0-9`, `-`, `_`. HTTP-аутентификация применяется до спаривания;
половины и возобновление должны принадлежать одному владельцу.
Половина ожидает пару до 30 секунд; число ожидающих сессий ограничено 1024.

Оба запроса обязаны передать ровно один заголовок `x-xhttp-version: 1`.
Отсутствие, повтор, список значений, ведущий ноль и неизвестное значение дают HTTP 400.
Успешные GET и POST ответы подтверждают `x-xhttp-version: 1`.
Выбор версии не предусматривает откат или определение по длине тела.
Таблица `xhttp_supported_versions` в конфигурации обеих сторон сейчас равна `[1]`.
Пустая C API таблица означает default `[1]`; явная пустая TOML-таблица недопустима.

На сервере `xhttp_enable = true` включает XHTTP listener, `transport_mode = "xhttp"`
задаёт его режим. Остальные серверные transports включаются собственными настройками.
В клиентском `[endpoint]` `transport_mode` принимает `connect` или `xhttp`;
при отсутствии выбирается по существующему `xhttp_enable` (default CONNECT).
В C API `transport_mode = nullptr` сохраняет этот выбор.
`xhttp-cdn` и `sudoku` отклоняются как неподдерживаемые режимы.
Старый `xhttp_legacy_negotiation` удалён; клиентский TOML parser явно отвергает его.
C API изменён и требует пересборки потребителей native-библиотеки.

## Формат кадра

Все целые числа — unsigned big endian. Кадры идут подряд; границы HTTP DATA,
TLS records и вызовов чтения не задают границы кадра.

| Поле | Размер | Значение |
| --- | --- | --- |
| type | 1 байт | Номер из реестра ниже |
| offset | 8 байт | Byte offset соответствующего направления |
| short_length | 2 байта | Длина, если старший бит равен 0 |
| long_length | 4 байта, опционально | Присутствует, если short_length имеет бит 0x8000 |
| payload | length байт | Тело кадра |

Для payload до 32767 байт заголовок занимает 11 байт.
Для более длинного payload заголовок занимает 15 байт:
`short_length = 0x8000 | (length >> 16)`, затем `long_length = length`.
Decoder вычисляет `((short_length & 0x7fff) << 16) | long_length`.
Максимум формата — `0x7fffffff`; реализация ограничивает весь buffered frame
2 MiB и отказывает по объявленной превышающей длине до накопления payload.
Encrypted record дополнительно ограничивает размер plaintext 1 MiB.
Неизвестный type — ошибка, включая тип внутри успешно проверенного AEAD record;
его нельзя пропускать как расширение.

| type | Имя | Payload и offset |
| --- | --- | --- |
| 0x01 | OPEN | Адрес назначения `host:port` в текстовом виде; offset 0 |
| 0x02 | DATA | Байты приложения; offset первого байта |
| 0x03 | ACK | Пустое тело, offset подтверждённых байтов противоположного направления; особый FIN ACK ниже |
| 0x04 | FIN | Пустое тело; offset конца направления |
| 0x05 | RST | Ошибка/сброс сессии |
| 0x06 | PAD | Игнорируемое тело padding, не продвигает offsets приложения |
| 0x07 | RESUME | 48 байт: offsets и HMAC; offset заголовка 0 |
| 0x08 | PING | Nonce health check, вместо OPEN; offset 0 |
| 0x09 | PONG | Точное повторение nonce PING; offset 0 |
| 0x0a | HELLO | 54 байта, начало шифрованного транспорта; offset 0 |
| 0x0b | HELLO_ACK | 102 байта, signed ответ; offset 0 |
| 0x0c | RESUME_TOKEN | 32 случайных байта, один раз после OPEN; offset 0 |

OPEN, RESUME и PING допустимы только в соответствующей фазе запуска.
Health check не создаёт возобновляемую TCP-сессию и не получает RESUME_TOKEN.
В обычной сессии токен обязателен и предшествует DATA сервера.

## Suite 1: handshake и records

Body encryption опционален для прямого подключения. Клиент с настроенным
Ed25519 public key начинает с открытого кадра HELLO. При некорректном заданном
ключе подключение завершается ошибкой. Ошибка KEX/подписи/AEAD никогда не
переключает сессию на открытое тело. В открытом режиме TLS-терминатор видит
и payload, и RESUME_TOKEN; HMAC proof не защищает от такого терминатора.

| HELLO payload | Размер |
| --- | --- |
| suite = 1 | u16, 2 байта |
| caps = 0 | u32, 4 байта |
| client X25519 ephemeral public key | 32 байта |
| client nonce | 16 байт |

| HELLO_ACK payload | Размер |
| --- | --- |
| suite = 1 | u16, 2 байта |
| caps = 0 | u32, 4 байта |
| server X25519 ephemeral public key | 32 байта |
| Ed25519 signature | 64 байта |

Префикс negotiation равен `00 01 00 00 00 00`. Неизвестный suite или любой
ненулевой caps отвергается до принятия KEX. Длины обязаны точно совпадать.
Повторный HELLO_ACK после завершённого handshake недопустим.

Подписываемый transcript, без разделителей и без NUL:

```text
ASCII("cattunnel-xhttp-handshake-v1")
|| version:u16(1) || suite:u16(1) || caps:u32(0)
|| ASCII(sid) || client_public[32] || server_public[32] || client_nonce[16]
```

Статический серверный Ed25519 seed подписывает transcript;
клиент проверяет подпись закреплённым публичным ключом.
Общий секрет — X25519 с новыми ephemeral keys обеих сторон.
HKDF-SHA256 использует salt `SHA256(transcript)` и IKM = shared secret;
16-байтовые AES-128-GCM keys выводятся раздельно с info
`ASCII("cattunnel-xhttp-v1 c2s")` и `ASCII("cattunnel-xhttp-v1 s2c")`.

HELLO и HELLO_ACK передаются открытыми XHTTP-кадрами.
После handshake каждый отправленный кадр шифруется в record:
`ciphertext_length:u32 || AES-GCM ciphertext || tag[16]`.
Длина включает tag, исключает 4-байтовый header; допустимо 16..1048592 байт.
AAD — точные ASCII bytes SID.
Nonce: `direction:u8 || 00 00 00 || counter:u64`; direction 1 для c2s, 2 для s2c.
Независимые counters начинаются с 0; значение `u64::MAX` не используется,
переполнение даёт ошибку. Потерянный или повторённый record нарушает
последовательность implicit counters: продолжать старое crypto-state нельзя.

Rust transport использует `RecordSealer: Send` / `RecordOpener: Send` через
boxed states; suite 1 и фабрика handshake находятся в `lib/src/xhttp_crypto.rs`.
В C++ единственная граница — класс `XhttpCrypto`, transport не выводит ключи.
Общий deterministic vector: `lib/tests/fixtures/xhttp_suite1.json` и клиентский
`core/test/xhttp_suite1_vector.h` (X25519, transcript hash, подпись, HKDF, оба records).

## Возобновление

Сессия хранит исходный RESUME_TOKEN, offsets и replay buffers после разрыва streams.
Новая пара использует прежний SID и того же аутентифицированного владельца.
GET существующей сессии выдаёт свежий `x-xhttp-challenge`: 32 случайных байта
в standard base64. Challenge относится к этой replacement-паре и потребляется
при её обработке; старый proof повторно не подходит.

Payload RESUME:

```text
up_confirmed:u64 || down_received:u64 || proof[32]
proof = HMAC-SHA256(RESUME_TOKEN,
    ASCII("cattunnel-xhttp-resume-v1") || ASCII(sid)
    || up_confirmed:u64 || down_received:u64 || challenge[32])
```

`up_confirmed` — последний известный клиенту ACK upload;
`down_received` — принятый клиентом download offset.
Proof проверяется constant-time до отмены прежних pumps и изменения offsets;
неверный proof даёт HTTP 403 и оставляет старый транспорт работающим.
Offsets проверяются до отмены и ещё раз после возврата старых socket guards.
Сервер отвергает upload offset выше записанного и download offset вне сохранённого
replay range. Replay и ACK повторяются с исходными offsets; дубликаты DATA
отбрасываются по offsets, пропуск данных отвергается.

Шифрованное возобновление всегда начинает свежий HELLO/HELLO_ACK,
получает новые ключи и нулевые counters; RESUME идёт внутри нового AEAD.
Исходный токен сохраняется. Открытое возобновление шифрованной сессии запрещено.
По умолчанию `xhttp_resume_timeout` равен 60 с; `0` отключает возобновление.
Таймауты ожидания replacement GET/guards и финального ACK ограничены 10 секундами.

## FIN, flow control и память

FIN upload закрывает запись к назначению, сохраняя возможность получить ответ.
Сервер передаёт FIN download с конечным offset. Клиент обязан подтвердить его
ACK с тем же offset и payload ровно `[0x04]`. Обычный ACK имеет пустое тело.
FIN ACK допустим только после downlink EOF и при точном конечном offset.
До него сервер сохраняет данные для повторения; потерянный FIN повторяется
на replacement transport. После FIN ACK сессия становится terminal и удаляется
без ожидания полного resume timeout. Общий half-close интерфейс приложения
остаётся отдельной задачей; наличие кадров FIN не означает её завершения.

Upload подтверждается только после принятия байтов socket sink назначения.
Download сохраняется до cancellable send и удерживается до ACK.
При полном окне downlink ждёт подтверждения. HTTP/2 wire credit учитывается
отдельно от application offsets, включая заголовки XHTTP и AEAD overhead.

Буферы XHTTP ограничены глобальным `xhttp_memory_limit`:
default — четверть min(physical RAM, cgroup limit), clamp 64..512 MiB,
fallback 64 MiB. Для 2 GiB default равен 512 MiB.
Новая сессия резервирует base 256 KiB; отказ резерва даёт HTTP 503,
не вытесняя существующие сессии. Replay blocks и decoder growth получают
дополнительный резерв до копирования, живущий до последнего владельца bytes.
Downlink ждёт бюджет, сохраняя уже прочитанный peer chunk через отмену.
Uplink при отказе decoder reserve закрывает транспорт без ACK непринятых bytes.
В этот счётчик не входят TLS/h2/kernel buffers и точный allocator RSS.
Метрики: `xhttp_reserved_bytes`, `xhttp_memory_rejections_total`.

## Версии и расширения

| Идентификатор | Назначение | Текущее поведение |
| --- | --- | --- |
| version 1 | Текущий XHTTP | Единственная поддерживаемая версия |
| version 2 | Усиленный набор шифрования и режим CDN | Зарезервирована, отказ |
| version 3 | Не назначена | Зарезервирована, отказ |
| suite 1 | X25519 + Ed25519 + HKDF-SHA256 + AES-128-GCM | Поддерживается |
| suite 2 | Будущий набор шифрования | Зарезервирован, отказ |
| caps | Будущие возможности, включая CDN и server PAD | Сейчас только 0; номера битов ещё не назначены |
| types 0x0d..0x0f | Неназначенные | Отказ |
| types 0x10..0x1f | Расширения version 2 | Зарезервированы, отказ |
| types 0x20..0x2f | version 3 | Зарезервированы, отказ |
| transport_mode xhttp-cdn / sudoku | Будущие режимы | Отказ в конфигурации |

Добавление версии требует явного обновления таблиц обеих сторон и спецификации,
проверки HTTP версии до отправки OPEN, точных suite/caps и их привязки к transcript.
Новый suite реализуется за crypto-интерфейсом с независимыми состояниями;
не добавляется криптография в transport. Новый type/bit получает определение,
правила допустимости по фазам и версии, отрицательные parser tests и общий
межъязыковой vector. По длине или при ошибке fallback не выполняется.
Клиент CatTunnel 1.5.9 включает NineLives по ссылке `tt://`: тег 0x4F включает транспорт,
0x50 задаёт путь, 0x51 несёт публичный ключ сервера для проверки HELLO_ACK.
