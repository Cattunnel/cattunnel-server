English: [CERT_RENEWAL.en.md](CERT_RENEWAL.en.md)

# Продление сертификата Let's Encrypt

## Содержание

- [Что нужно заранее](#что-нужно-заранее)
- [Установка Certbot](#установка-certbot)
- [Выпуск сертификата](#выпуск-сертификата)
    - [Вариант А: standalone](#вариант-а-standalone)
    - [Вариант Б: webroot](#вариант-б-webroot)
- [Сертификат Certbot в настройках сервера](#сертификат-certbot-в-настройках-сервера)
- [Автоматическое продление](#автоматическое-продление)
- [Как сервер подхватывает новый сертификат](#как-сервер-подхватывает-новый-сертификат)
- [Проверка продления](#проверка-продления)
- [Если что-то не работает](#если-что-то-не-работает)

Сервер CatTunnel работает только с действующим TLS-сертификатом. Мастер `trusttunnel_endpoint --setup-wizard` умеет выпустить его сам, но для долгой работы лучше взять Let's Encrypt через [Certbot][certbot] и включить автоматическое продление. Если сертификат Let's Encrypt уже выпущен без Certbot, выпустите его заново через Certbot, иначе он не будет продлеваться сам.

Порядок действий тот же, что в TrustTunnel ([оригинал](https://github.com/TrustTunnel/TrustTunnel/blob/master/CERT_RENEWAL.md)): сервер читает сертификат и ключ из `hosts.toml` так же, как endpoint TrustTunnel. Имена бинарника `trusttunnel_endpoint` и службы `trusttunnel` в форке остались прежними (см. [README](README.md)).

[certbot]: https://eff-certbot.readthedocs.io/en/stable/

```text
 Certbot (таймер)           Let's Encrypt            CatTunnel server
       |  1. certbot renew        |                        |
       |------------------------->|                        |
       |  2. проверка HTTP-01     |                        |
       |<-------------------------|  порт 80/tcp           |
       |  3. новый сертификат     |                        |
       |<-------------------------|                        |
       |  4. обновляет файлы в /etc/letsencrypt/live/      |
       |  5. deploy hook: systemctl restart trusttunnel -->|
       |                          |   6. читает новые файлы|
```

Рисунок 1. Продление: Certbot обновляет файлы, deploy hook перезапускает службу, сервер читает новые файлы.

## Что нужно заранее

- Публичное DNS-имя (запись A/AAAA), которое указывает на сервер.
- Порт **80/tcp**, доступный из интернета на время выпуска и продления (проверка HTTP-01).
- Доступ root на сервере.

Руководство использует HTTP-01. Для wildcard-сертификата нужен DNS-01.

## Установка Certbot

Ставьте способом, который рекомендует ваш дистрибутив. Пример:

```bash
# Debian/Ubuntu
sudo apt update
sudo apt install -y certbot
```

## Выпуск сертификата

Выберите один из режимов.

### Вариант А: standalone

Подходит, если на порту 80 ничего не слушает. Certbot ненадолго поднимет свой веб-сервер.

```bash
sudo certbot certonly --standalone -d example.com
```

### Вариант Б: webroot

Подходит, если на сервере уже есть HTTP-сервер. Он должен отдавать `/.well-known/acme-challenge/` из каталога webroot.

```bash
sudo certbot certonly --webroot -w /var/www/html -d example.com
```

После выпуска Certbot напечатает нужные пути:

```console
Certificate is saved at: /etc/letsencrypt/live/example.com/fullchain.pem
Key is saved at:         /etc/letsencrypt/live/example.com/privkey.pem
```

Это символические ссылки, Certbot сам обновляет их при продлении.

## Сертификат Certbot в настройках сервера

Если настроек сервера ещё нет, запустите `trusttunnel_endpoint --setup-wizard` и выберите «Provide path to existing certificate». Пути укажите через пробел, в любом порядке.

```console
? How would you like to create a certificate? ›
  Issue a Let's Encrypt certificate (requires a public domain)
  Generate a self-signed certificate
❯ Provide path to existing certificate
? Path to certificate file(s):
  - Single file containing both cert and key: /path/to/combined.pem
  - Separate files: /path/to/cert.pem /path/to/key.pem
 › /etc/letsencrypt/live/example.com/fullchain.pem /etc/letsencrypt/live/example.com/privkey.pem
```

Если настройки уже есть, замените пути в `hosts.toml`.

Было:

```toml
[[main_hosts]]
hostname = "example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"
```

Стало:

```toml
[[main_hosts]]
hostname = "example.com"
cert_chain_path = "/etc/letsencrypt/live/example.com/fullchain.pem"
private_key_path = "/etc/letsencrypt/live/example.com/privkey.pem"
```

## Автоматическое продление

На большинстве современных дистрибутивов Certbot сам ставит таймер systemd. Проверить:

```bash
systemctl list-timers | grep -E 'certbot|letsencrypt'
```

Если таймеров systemd нет, используйте cron:

```bash
sudo crontab -e
```

Добавьте строку:

```bash
0 3 * * * certbot renew --quiet
```

## Как сервер подхватывает новый сертификат

Certbot обновляет файлы, но сервер читает их при запуске и при перечитывании настроек TLS-хостов. Проще всего добавить deploy hook, который перезапускает службу после каждого успешного продления:

```bash
sudo certbot reconfigure --deploy-hook "systemctl restart trusttunnel"
```

Для Certbot старше 2.3.0 добавьте в раздел `[renewalparams]` файла `/etc/letsencrypt/renewal/<ваш-домен>.conf` строку:

```conf
renew_hook = systemctl restart trusttunnel
```

Замените `trusttunnel` на настоящее имя вашей службы.

Перезапуск не обязателен. Сервер перечитывает файл `hosts.toml` и сертификаты по сигналу `SIGHUP` без остановки ([CONFIGURATION.md](CONFIGURATION.md#hot-reloading-tls-hosts-and-subscription-settings), на английском), поэтому hook может послать сигнал: `systemctl kill -s HUP trusttunnel`.
## Проверка продления

Один раз запустите пробное продление и убедитесь, что оно проходит целиком:

```bash
sudo certbot renew --dry-run
```

С режимом `--standalone` порт 80 на время пробы должен быть свободен.

## Если что-то не работает

- **Порт 80 занят.** Остановите процесс на время продления или перейдите на `--webroot`.
- **DNS.** Имя должно указывать на публичный адрес сервера.
- **Файрвол.** Откройте входящий 80/tcp из интернета.
- **Права.** Служба должна читать `/etc/letsencrypt/live/.../fullchain.pem` и `privkey.pem`.
