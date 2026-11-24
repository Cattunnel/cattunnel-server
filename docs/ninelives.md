# NineLives: протокол 1 (внутреннее имя XHTTP)

NineLives несёт каждое туннелируемое TCP-соединение парой HTTP/2-потоков внутри
одного TLS-соединения, а не одним потоком CONNECT. Имя NineLives используется в
интерфейсах и документах; в коде, ключах конфигурации и файлах остаётся внутреннее
имя `xhttp` (`xhttp_enable`, `transport_mode = "xhttp"`, `http_xhttp.rs`). Форма
запросов похожа на XHTTP из Xray; сам протокол свой, с ним несовместим и не несёт
бренда на проводе: путь берётся из конфигурации.

Это обзор со схемами. Нормативный текст: [xhttp-protocol.md](xhttp-protocol.md)
(таблицы кадров, транскрипт рукопожатия, доказательство возобновления, лимиты). При
расхождении верны `xhttp-protocol.md` и код. Связанные документы:
[connect.md](connect.md), [cattail.md](cattail.md),
[upstream-divergence.md](upstream-divergence.md). Английский перевод:
[ninelives.en.md](ninelives.en.md), при расхождении верен русский текст.

Версии в 1.5.9.5. Реализована только версия протокола 1: `xhttp_supported_versions`
должна быть `[1]`, иное значение не проходит проверку настроек. Версия 2
(NineLivesSec: усиленное собственное шифрование и режим CDN) и версия 3
зарезервированы, endpoint их отклоняет; `transport_mode`, отличный от `xhttp`,
тоже отклоняется. Набор шифрования 1 единственный.

## 1. Два потока, одно соединение

```text
 client                          one TLS connection (h2)               endpoint
   |                                                                       |
   |== stream A: GET  /api/v2/<sid>   (downlink) ========================>|
   |<=============== response 200, body = frames of the downlink ==========|
   |                                                                       |
   |== stream B: POST /api/v2/<sid>   (uplink, request body is open) =====>|
   |   body = frames of the uplink                                          |
   |                                                                       |
   |   both carry   x-xhttp-version: 1   and the same credentials          |
```

Рисунок 1. Сессия состоит из GET (байты от сервера к клиенту) и POST (байты от
клиента к серверу) с одним путём. Много сессий делят одно TLS-соединение.

Идентификатор сессии (SID): от 1 до 64 символов `A-Z a-z 0-9 - _`. Endpoint
аутентифицирует каждую половину до спаривания, обе половины обязаны принадлежать
одному пользователю. Половина ждёт пару до 30 секунд; ждущих сессий не больше 1024.
Запрос без ровно одного `x-xhttp-version: 1` получает HTTP 400.

## 2. Слои

```text
 destination TCP bytes
        |
 +------v-------------------------------------------+
 | NineLives frames: OPEN DATA ACK FIN RST PAD ...   |  offsets per direction
 +------+-------------------------------------------+
        |   suite 1: every frame becomes one record
 +------v-------------------------------------------+
 | record: u32 length | AES-128-GCM ciphertext | tag |  AAD = SID
 +------+-------------------------------------------+
        |
 +------v-------------------------------------------+
 | HTTP/2 DATA of the POST (up) / of the GET (down)  |
 +------+-------------------------------------------+
        |
 +------v-------------------------------------------+
 | TLS (and, optionally, CatTail outside it)         |
 +--------------------------------------------------+
```

Рисунок 2. Слои сессии NineLives. Шифрование тела применяется, если у клиента есть
публичный ключ Ed25519 endpoint'а; без него кадры идут без шифрования внутри TLS.

## 3. Начало сессии

Ответ downlink отправляется до соединения с назначением, потому что обмен ключами
отвечает в его теле.

```text
 client (POST body)                         endpoint (GET response body)
   |                                              |
   |<---------- 200 + x-xhttp-version: 1 ---------|  headers of the GET
   |-- HELLO  (suite 1, caps 0, X25519 pub, nonce)|  54 bytes, plain frame
   |<--- HELLO_ACK (X25519 pub, Ed25519 sig) -----|  102 bytes, plain frame
   |     both sides derive c2s / s2c AES keys     |
   |-- [OPEN host:port]   (sealed record) ------->|
   |<-- [RESUME_TOKEN 32 random bytes] (sealed) --|  then the destination is
   |                                              |  connected
   |-- [DATA ...] ------------------------------->|
   |<-------------------------- [DATA ...] -------|
   |-- [ACK offset] ----------------------------->|  uplink ACK, see section 5
   |<----------------------- [ACK offset] --------|
```

Рисунок 3. Рукопожатие с шифрованием тела. Без ключа endpoint'а HELLO и HELLO_ACK
пропускаются, а OPEN идёт первым кадром открытым текстом. Проверка живости шлёт PING
вместо OPEN и получает PONG с тем же nonce; она не создаёт возобновляемую сессию и
не получает RESUME_TOKEN.

Выработка ключей (набор 1):

```text
 X25519(client_eph, server_eph) --> shared secret --+
                                                     v
 transcript = "cattunnel-xhttp-handshake-v1" | version | suite | caps
              | SID | client_pub | server_pub | client_nonce
 Ed25519 (endpoint static key) signs transcript --> client checks signature
                                                     |
 HKDF-SHA256(salt = SHA256(transcript), IKM = shared secret)
        +--> key c2s  info "cattunnel-xhttp-v1 c2s"   (16 bytes)
        +--> key s2c  info "cattunnel-xhttp-v1 s2c"   (16 bytes)
 nonce = direction (1 c2s, 2 s2c) | 00 00 00 | counter u64
```

Рисунок 4. Ключи набора 1: X25519, Ed25519, HKDF-SHA256, AES-128-GCM. Ошибка
подписи, обмена ключами или проверки AEAD никогда не переключает сессию на открытый
текст.

## 4. Кадр

```text
 short form (payload < 32768):
 +------+-----------------+--------------+-----------------+
 | type | offset (u64 BE) | length (u16) | payload         |
 | 1 B  | 8 B             | 2 B          | length bytes    |
 +------+-----------------+--------------+-----------------+
 long form (length >= 32768), 15-byte header:
 +------+-----------------+------------------+------------+---------+
 | type | offset (u64 BE) | 0x8000 | len>>16 | length u32 | payload |
 |      |                 | (u16)            | (full len) |         |
 +------+-----------------+------------------+------------+---------+
```

Рисунок 5. Заголовок кадра, 11 или 15 байт. Типы: 01 OPEN, 02 DATA, 03 ACK, 04 FIN,
05 RST, 06 PAD (игнорируется), 07 RESUME, 08 PING, 09 PONG, 0a HELLO, 0b HELLO_ACK,
0c RESUME_TOKEN. Любой другой тип является ошибкой. Endpoint буферизует не больше 2
МиБ на кадр; запечатанная запись содержит не больше 1 МиБ открытого текста. PAD
endpoint принимает и отбрасывает; сам в 1.5.9.5 его не отправляет.

## 5. Смещения, ACK и FIN

Каждое направление считает байты с 0; DATA несёт смещение своего первого байта,
поэтому дубликат узнаётся, а пропуск является ошибкой.

- ACK вверх: endpoint подтверждает байты клиента только после того, как сокет
  назначения их принял. Подтверждение запускается 64 КиБ неподтверждённых байт. Оно
  идёт по downlink.
- ACK вниз: клиент подтверждает полученные байты; endpoint хранит данные до
  подтверждения (буфер повтора) и перестаёт слать, когда окно (`xhttp_unacked_limit`,
  по умолчанию 1 МиБ) заполнено.
- FIN несёт конечное смещение направления. На FIN сервера клиент отвечает ACK с
  нагрузкой ровно `[0x04]` и тем же смещением.

## 6. Возобновление

Если TLS-соединение умерло, сессия хранится `xhttp_resume_timeout` (по умолчанию
60 с; 0 выключает возобновление). Клиент открывает новые GET и POST с тем же SID и
тем же пользователем.

```text
 client                                         endpoint (session kept)
   |-- GET  /api/v2/<sid> ----------------------->|
   |<-- 200  x-xhttp-challenge: <32 random bytes> -|  fresh per replacement pair
   |-- POST /api/v2/<sid>                          |
   |-- [HELLO] (encrypted sessions: new keys)      |
   |<-- [HELLO_ACK] --------------------------------|
   |-- [RESUME up_confirmed | down_received | proof] ->|
   |        proof = HMAC-SHA256(RESUME_TOKEN, "cattunnel-xhttp-resume-v1"
   |                 | sid | up_confirmed | down_received | challenge)
   |<-- downlink bytes from offset down_received  ----|  repeated from the
   |-- uplink bytes from offset up_confirmed -------->|  replay buffers
```

Рисунок 6. Возобновление. Доказательство проверяется за постоянное время до отмены
старых насосов; неверное даёт HTTP 403 и оставляет старый транспорт работать.
Зашифрованная сессия всегда повторяет HELLO и получает новые ключи и нулевые
счётчики; возобновление такой сессии открытым текстом отклоняется.

## 7. Бюджет памяти

Все буферы NineLives (и соединений CatTail) учитываются в `xhttp_memory_limit`:
четверть меньшего из физической памяти и лимита cgroup, в пределах 64..512 МиБ, 64
МиБ если неизвестно. Новая сессия резервирует 256 КиБ; при отказе резерва endpoint
отвечает HTTP 503 и не вытесняет существующие сессии. Метрики:
`xhttp_reserved_bytes`, `xhttp_memory_rejections_total`.

## 8. Клиентская сторона

Ядро клиента (`core/src/xhttp_*.cpp`, см. его документы) открывает от 2 до 4
TLS-соединений для NineLives, использует тот же ограничитель рукопожатий, что и
CONNECT (3 в минуту, через 2-6 с), и заменяет соединение, когда подошёл срок
ротации или пул деградировал. Ротация ключа между серверами (9а) и профиль отпечатка
относятся только к CONNECT; NineLives в 9а не участвует.

## 9. Совместимость

- Несовместим с XHTTP из Xray и с официальным клиентом TrustTunnel: кадры, шифр и
  возобновление свои.
- По умолчанию выключен (`xhttp_enable = false`). Запрос на пути NineLives с адреса
  decoy-only отклоняется как неудачная аутентификация.
- Клиент без ключа endpoint'а может использовать кадры без шифрования внутри TLS;
  endpoint шифрование не навязывает.
