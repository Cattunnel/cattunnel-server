English: [README.en.md](README.en.md)

# Крейт trusttunnel-deeplink

Самостоятельная библиотека на Rust для кодирования и разбора ссылок `tt://`, в которых передаются настройки подключения к серверу. Крейт взят из TrustTunnel без изменений (в коде форка нет отличий от TrustTunnel 1.3.1); имя крейта осталось прежним, см. [корневой README](../README.md#имена-оставшиеся-от-апстрима). Формат ссылки описан в [DEEP_LINK.md](../DEEP_LINK.md).

## Что умеет крейт

- Кодирует и разбирает все теги формата (`0x00`..`0x10`), версия формата 2.
- Возвращает типизированные ошибки (`DeepLinkError`) с понятным текстом.
- Работает с Base64url без заполнителей.
- Переводит сертификаты между PEM и DER.
- Проверен тестами: круговые проверки, свойства (proptest) и сверка со скриптами на Python.

Схема работы двух главных функций:

```text
 encode(&config)                           decode(uri)
   |                                          |
   v                                          v
 проверка полей (validate)                  префикс tt://? или tt://
   |                                          |
   v                                          v
 TLV-записи, версия 2                       Base64url -> байты
   |                                          |
   v                                          v
 Base64url без заполнителей                 разбор TLV, неизвестные теги пропускаются
   |                                          |
   v                                          v
 строка tt://?...                           проверка полей (validate) -> DeepLinkConfig
```

Рисунок 1. Что делают `encode` и `decode`.

## Подключение

Добавьте крейт в `Cargo.toml` из репозитория сервера CatTunnel:

```toml
[dependencies]
trusttunnel-deeplink = { git = "https://github.com/Cattunnel/cattunnel-server" }
```

Для сборки внутри этого дерева используйте `path = "deeplink"`. По умолчанию включён признак `serde`: он добавляет `Serialize` и `Deserialize` для `DeepLinkConfig`, `Protocol` и `TlsProfile`. Отключить его можно через `default-features = false`.

## Быстрый старт

### Кодирование

```rust
use trusttunnel_deeplink::{encode, DeepLinkConfig};

let config = DeepLinkConfig::builder()
    .hostname("vpn.example.com".to_string())
    .addresses(vec!["203.0.113.10:443".to_string()])
    .username("alice".to_string())
    .password("secret123".to_string())
    .build()
    .unwrap();

let uri = encode(&config).unwrap();
println!("Deep-link: {}", uri);
// tt://?AAECAQ92cG4uZXhhbXBsZS5jb20FBWFsaWNlBglzZWNyZXQxMjMCEDIwMy4wLjExMy4xMDo0NDM
```

Адреса передаются строками `адрес:порт` (IP или домен). Ссылка начинается с тега версии (`AAEC` в начале значит `00 01 02`), поэтому все ссылки версии 2 начинаются одинаково.

### Разбор

```rust
use trusttunnel_deeplink::decode;

let uri = "tt://?AAECAQ92cG4uZXhhbXBsZS5jb20FBWFsaWNlBglzZWNyZXQxMjMCEDIwMy4wLjExMy4xMDo0NDM";
let config = decode(uri).unwrap();

println!("Hostname: {:?}", config.hostname);
println!("Username: {:?}", config.username);
```

## Поля настроек

Структура `DeepLinkConfig` содержит поля:

| Поле | Тип | Обязательно | По умолчанию | Смысл |
| --- | --- | --- | --- | --- |
| `hostname` | `Option<String>` | да * | `None` | Имя сервера |
| `addresses` | `Vec<String>` | да * | `[]` | Адреса сервера (`адрес:порт`) |
| `username` | `Option<String>` | да * | `None` | Логин |
| `password` | `Option<String>` | да * | `None` | Пароль |
| `custom_sni` | `Option<String>` | нет | `None` | Своё имя для SNI |
| `has_ipv6` | `bool` | нет | `true` | IPv6 доступен |
| `skip_verification` | `bool` | нет | `false` | Не проверять сертификат |
| `certificate` | `Option<Vec<u8>>` | нет | `None` | Цепочка сертификатов в DER |
| `upstream_protocol` | `Protocol` | нет | `Http2` | Протокол к серверу: `Http2` или `Http3` |
| `anti_dpi` | `bool` | нет | `false` | Меры против DPI |
| `client_random_prefix` | `Option<String>` | нет | `None` | Префикс TLS `client_random`, hex |
| `client_random_auth_key` | `Option<String>` | нет | `None` | Ключ для `client_random`, hex |
| `name` | `Option<String>` | нет | `None` | Имя сервера для показа |
| `dns_upstreams` | `Vec<String>` | нет | `[]` | Адреса DNS-апстримов |
| `tls_profile` | `Option<TlsProfile>` | нет | `None` | Профиль отпечатка ClientHello; при `None` тег в ссылку не пишется |
| `subscription_url` | `Option<String>` | нет | `None` | Адрес подписки по HTTPS (версия 2) |

\* Обязательно, если не задан `subscription_url`. С ним статические параметры необязательны и служат запасом.

Значения `TlsProfile`: `Chrome`, `ChromeCanary`, `Safari`, `Firefox`, `OkHttp`, `OpenSsl`, `BoringSsl`. В записи serde и в `FromStr` они называются `chrome`, `chrome_canary`, `safari`, `firefox`, `okhttp`, `openssl`, `boringssl`. Справочный скрипт `scripts/config_to_deeplink.py` принимает ещё устаревшее имя `default` как синоним `boringssl`; крейт его не знает.

## Дополнительно

### Сертификаты

```rust
use trusttunnel_deeplink::cert::{der_to_pem, pem_to_der};

// PEM (одна или несколько записей) в DER, подряд
let pem = "-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----";
let der = pem_to_der(pem).unwrap();

// обратно в PEM
let pem_again = der_to_pem(&der).unwrap();
```

### Построитель

```rust
use trusttunnel_deeplink::{DeepLinkConfig, Protocol, TlsProfile};

let config = DeepLinkConfig::builder()
    .hostname("vpn.example.com".to_string())
    .addresses(vec!["203.0.113.10:443".to_string()])
    .username("user".to_string())
    .password("pass".to_string())
    .custom_sni(Some("cdn.example.org".to_string()))
    .has_ipv6(false)
    .upstream_protocol(Protocol::Http3)
    .anti_dpi(true)
    .tls_profile(Some(TlsProfile::Firefox))
    .build()
    .unwrap();
```

`build()` проверяет поля и возвращает ошибку, если нет обязательных, если `client_random_prefix` или `client_random_auth_key` записаны не в hex или если `subscription_url` не начинается с `https://`.

### Ссылка только с подпиской

```rust
use trusttunnel_deeplink::{encode, DeepLinkConfig};

let config = DeepLinkConfig::builder()
    .subscription_url(Some(
        "https://alice:s3cret@vpn.example.com/subscription".to_string(),
    ))
    .build()
    .unwrap();

let uri = encode(&config).unwrap();
// tt://?AAECDjFodHRwczovL2FsaWNlOnMzY3JldEB2cG4uZXhhbXBsZS5jb20vc3Vic2NyaXB0aW9u
```

## Ошибки

Все функции возвращают `DeepLinkError`:

```rust
use trusttunnel_deeplink::{decode, DeepLinkError};

match decode("invalid://uri") {
    Ok(_config) => println!("Success!"),
    Err(DeepLinkError::InvalidScheme(scheme)) => {
        println!("Invalid URI scheme: {}", scheme);
    }
    Err(DeepLinkError::MissingRequiredField(field)) => {
        println!("Missing required field: {}", field);
    }
    Err(DeepLinkError::UnsupportedVersion { found, max_supported }) => {
        println!("Version {} is above {}", found, max_supported);
    }
    Err(e) => println!("Other error: {}", e),
}
```

Основные варианты: `InvalidScheme`, `InvalidBase64`, `TruncatedTlv`, `MissingRequiredField`, `InvalidSubscriptionUrl`, `InvalidProtocol`, `InvalidTlsProfile`, `UnsupportedVersion`, `InvalidBoolean`, `InvalidUtf8`, `InvalidCertificate`. Полный список в `deeplink/src/error.rs`.

## Тесты

```shell
# все тесты крейта
cargo test -p trusttunnel-deeplink

# круговые проверки
cargo test -p trusttunnel-deeplink --test roundtrip

# сверка со скриптами на Python
cargo test -p trusttunnel-deeplink --test python_compat

# проверки свойств
cargo test -p trusttunnel-deeplink --test proptest
```

Для `python_compat` нужны `python3` в `PATH` и скрипт `scripts/config_to_deeplink.py` в корне репозитория.
