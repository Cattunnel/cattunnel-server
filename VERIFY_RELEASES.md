English: [VERIFY_RELEASES.en.md](VERIFY_RELEASES.en.md)

# Проверка релизов

В документе два раздела: как проверить архивы **CatTunnel server** и как проверить подпись официальных релизов **TrustTunnel** (инструкция апстрима, сохранена).

## Релизы CatTunnel server

В [релизах](https://github.com/Cattunnel/cattunnel-server/releases) лежат архивы `trusttunnel-v<версия>-linux-x86_64.tar.gz` и `trusttunnel-v<версия>-linux-aarch64.tar.gz` и файл `SHA256SUMS` ([README](README.md#готовые-сборки)). Подписи GPG у релизов форка нет: ключ подписи апстрима (AdGuard) принадлежит им, его секреты подставляет только CI апстрима, а задания подписи в этом репозитории ограничены условием на репозиторий апстрима (`.github/workflows/publish-release.yml`). Поэтому файла `.sig` в архивах CatTunnel нет, и инструкция ниже для них не подходит.

Проверка контрольной суммы:

```shell
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf trusttunnel-v1.5.9.5-linux-x86_64.tar.gz
```

Команда сверяет скачанные архивы с `SHA256SUMS`. `--ignore-missing` нужен, чтобы не получить ошибку про архив другой платформы, который вы не скачивали.

```text
 релизы Cattunnel/cattunnel-server
   |-- trusttunnel-v<версия>-linux-<arch>.tar.gz
   |-- SHA256SUMS
        |
        v
 sha256sum -c --ignore-missing SHA256SUMS
        |
        +-- "OK"      -> архив цел, tar -xzf
        +-- "FAILED"  -> скачайте заново, не запускайте
```

Рисунок 1. Проверка архива CatTunnel server по SHA256SUMS.

Сумма защищает от повреждения при скачивании. От подмены самого релиза она не защищает: кто подменит архив, подменит и `SHA256SUMS` рядом. Если это важно, сверьте сумму по второму каналу или соберите бинарник из исходников ([DEVELOPMENT.md](DEVELOPMENT.md)).

## Релизы TrustTunnel (инструкция апстрима)

Этот раздел относится к официальным релизам [TrustTunnel](https://github.com/TrustTunnel/TrustTunnel/releases), не к CatTunnel. Начиная с TrustTunnel v0.9.126 исполняемые файлы подписаны. Внутри архива лежит небольшой файл `.sig` с данными подписи. Если бинарник в архиве подменят, проверка покажет, что это не официальная сборка AdGuard.

```text
 архив TrustTunnel
   |-- trusttunnel_endpoint
   |-- trusttunnel_endpoint.sig
        |
        v
 gpg --recv-key (один раз)  ->  gpg --verify ...sig
        |
        +-- "Good signature from AdGuard <devteam@adguard.com>"
```

Рисунок 2. Проверка подписи официального релиза TrustTunnel.

### Как проверить, что файл собран AdGuard

1. Распакуйте архив TrustTunnel.
2. Получите открытый ключ с сервера ключей. Для текущего релиза:

    ```text
    gpg --keyserver 'keys.openpgp.org' --recv-key '28645AC9776EC4C00BCE2AFC0FE641E7235E2EC6'
    ```

   Команда напечатает примерно такое:

    ```text
    gpg: key 0FE641E7235E2EC6: public key "AdGuard <devteam@adguard.com>" imported
    gpg: Total number processed: 1
    gpg:               imported: 1
    ```

3. Проверьте подпись. В Unix:

   ```text
   gpg --verify trusttunnel/trusttunnel_endpoint.sig
   ```

   Вывод будет похож на этот:

   ```text
   gpg: assuming signed data in 'trusttunnel'
   gpg: Signature made Mon 2 Feb 2026 19:30:55 MSK
   gpg:                using RSA key 28645AC9776EC4C00BCE2AFC0FE641E7235E2EC6
   gpg:                issuer "devteam@adguard.com"
   gpg: Good signature from "AdGuard <devteam@adguard.com>" [ultimate]
   ```

   Убедитесь, что:

    - ключ RSA равен 28645AC9776EC4C00BCE2AFC0FE641E7235E2EC6;
    - имя издателя: AdGuard;
    - адрес: devteam@adguard.com.

   Может появиться предупреждение:

   ```text
   gpg: WARNING: The key's User ID is not certified with a trusted signature!
   gpg:          There is no indication that the signature belongs to the owner.
   Primary key fingerprint: 2864 5AC9 776E C4C0 0BCE  2AFC 0FE6 41E7 235E 2EC6
   ```
