Русский: [VERIFY_RELEASES.md](VERIFY_RELEASES.md) (the Russian text is the main one; where they differ, the Russian text is correct)

# Verifying releases

Two sections: how to verify **CatTunnel server** archives, and how to verify the signature of the official **TrustTunnel** releases (the upstream instruction, kept).

## CatTunnel server releases

The [releases](https://github.com/Cattunnel/cattunnel-server/releases) carry `trusttunnel-v<version>-linux-x86_64.tar.gz`, `trusttunnel-v<version>-linux-aarch64.tar.gz` and a `SHA256SUMS` file ([README](README.en.md#prebuilt-binaries)). Fork releases have no GPG signature: the signing key belongs to upstream (AdGuard), only the upstream CI gets its secrets, and the signing jobs in this repository are limited by a condition to the upstream repository (`.github/workflows/publish-release.yml`). So CatTunnel archives contain no `.sig` file and the upstream instruction below does not apply to them.

Checksum check:

```shell
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf trusttunnel-v1.5.9.5-linux-x86_64.tar.gz
```

The command compares the downloaded archives with `SHA256SUMS`. `--ignore-missing` avoids an error about the archive for the other platform that you did not download.

```text
 releases of Cattunnel/cattunnel-server
   |-- trusttunnel-v<version>-linux-<arch>.tar.gz
   |-- SHA256SUMS
        |
        v
 sha256sum -c --ignore-missing SHA256SUMS
        |
        +-- "OK"      -> the archive is intact, tar -xzf
        +-- "FAILED"  -> download again, do not run it
```

Figure 1. Checking a CatTunnel server archive against SHA256SUMS.

The sum protects against damage during download. It does not protect against a replaced release: whoever replaces the archive can replace `SHA256SUMS` next to it. If this matters, compare the sum through a second channel or build the binary from source ([DEVELOPMENT.en.md](DEVELOPMENT.en.md)).

## TrustTunnel releases (upstream instruction)

This section applies to the official [TrustTunnel](https://github.com/TrustTunnel/TrustTunnel/releases) releases, not to CatTunnel. Since TrustTunnel v0.9.126 the executables are signed. An archive holds a small `.sig` file with the signature data. If someone replaces the binary inside the archive, the check shows it is not an official AdGuard build.

```text
 TrustTunnel archive
   |-- trusttunnel_endpoint
   |-- trusttunnel_endpoint.sig
        |
        v
 gpg --recv-key (once)  ->  gpg --verify ...sig
        |
        +-- "Good signature from AdGuard <devteam@adguard.com>"
```

Figure 2. Checking the signature of an official TrustTunnel release.

### How to verify that the executable was built by AdGuard

1. Unpack the TrustTunnel archive.
2. Import the public key from the keyserver. For the current release:

    ```text
    gpg --keyserver 'keys.openpgp.org' --recv-key '28645AC9776EC4C00BCE2AFC0FE641E7235E2EC6'
    ```

   The command prints something like:

    ```text
    gpg: key 0FE641E7235E2EC6: public key "AdGuard <devteam@adguard.com>" imported
    gpg: Total number processed: 1
    gpg:               imported: 1
    ```

3. Verify. On Unix:

   ```text
   gpg --verify trusttunnel/trusttunnel_endpoint.sig
   ```

   The output looks like this:

   ```text
   gpg: assuming signed data in 'trusttunnel'
   gpg: Signature made Mon 2 Feb 2026 19:30:55 MSK
   gpg:                using RSA key 28645AC9776EC4C00BCE2AFC0FE641E7235E2EC6
   gpg:                issuer "devteam@adguard.com"
   gpg: Good signature from "AdGuard <devteam@adguard.com>" [ultimate]
   ```

   Check that:

    - the RSA key is 28645AC9776EC4C00BCE2AFC0FE641E7235E2EC6;
    - the issuer name is AdGuard;
    - the e-mail address is devteam@adguard.com.

   You may also see this warning:

   ```text
   gpg: WARNING: The key's User ID is not certified with a trusted signature!
   gpg:          There is no indication that the signature belongs to the owner.
   Primary key fingerprint: 2864 5AC9 776E C4C0 0BCE  2AFC 0FE6 41E7 235E 2EC6
   ```
