English: [README.en.md](README.en.md)

# fastlane

Каталог принадлежит апстриму (TrustTunnel) и в форке не менялся. В нём автоматизация подписи и нотаризации под macOS для бинарника `trusttunnel_endpoint`. Её вызывает CI апстрима ([`bamboo-specs/deploy.yaml`](../bamboo-specs/deploy.yaml)), а у CatTunnel server нет macOS-релизов: в [релизах](https://github.com/Cattunnel/cattunnel-server/releases) только Linux (см. [README](../README.md#готовые-сборки)). Для сборки и запуска сервера этот каталог не нужен.

Описание ниже сохранено для тех, кто собирает подписанный macOS-бинарник сам и имеет свои учётные данные.

## Назначение

Подпись сертификатом Developer ID и нотаризация `trusttunnel_endpoint`. Схема взята у клиента TrustTunnel и подогнана под репозиторий на Cargo.

```text
 fastlane remove_certs   (убрать старую связку ключей)
        |
 fastlane certs          (взять сертификат Developer ID через match)
        |
 сборка и подпись бинарника
        |
 fastlane notari id:"<bundle_id>" bundle:"<путь_к_бинарнику>"
        |
 fastlane remove_certs   (убрать связку ключей)
```

Рисунок 1. Порядок вызова линий в CI апстрима (`bamboo-specs/deploy.yaml`).

## Что нужно

- Командные инструменты Xcode
- Ruby с Bundler
- доступ к репозиторию сертификатов, который использует `match`
- ключ App Store Connect API (переменные окружения Bamboo)

Если инструментов Xcode нет:

```sh
xcode-select --install
```

Зависимости Ruby (`Gemfile` в корне репозитория):

```sh
bundle config --local path '.bundle/vendor'
bundle install
```

## Окружение

Значения по умолчанию лежат в `.env.default`. Главные переменные:

- `BUILD_DIR`
- `MATCH_GIT_URL`
- `MATCH_PASSWORD`
- `MATCH_KEYCHAIN_PASSWORD`
- `MATCH_KEYCHAIN_NAME`
- `MATCH_APP_IDENTIFIER`
- `bamboo_appStoreConnectApiKeyId`
- `bamboo_appStoreConnectApiKeyIssuerId`
- `bamboo_appStoreConnectApiKeyBase64Password`

## Линии

### `certs`

```sh
[bundle exec] fastlane certs
```

Загружает сертификат Developer ID, нужный для подписи под macOS.

### `remove_certs`

```sh
[bundle exec] fastlane remove_certs
```

Удаляет временную локальную связку ключей, созданную для подписи.

### `notari`

```sh
[bundle exec] fastlane notari id:"<bundle_id>" bundle:"<path_to_binary>"
```

Отправляет бинарник на нотаризацию с ключом App Store Connect по умолчанию.

Обязательные параметры:

- `id`: идентификатор пакета для нотаризации
- `bundle`: путь к подписанному бинарнику

## Примечания

- Перед нотаризацией исполняемые файлы сжимаются во временный архив.
- Stapling для исполняемых файлов намеренно пропущен.
- Каталог ведётся вручную и не генерируется.
