Русский: [README.md](README.md) (the Russian text is the main one; where they differ, the Russian text is correct)

# fastlane

This directory belongs to upstream (TrustTunnel) and is unchanged in the fork. It holds the macOS signing and notarization automation for the `trusttunnel_endpoint` binary. The upstream CI calls it ([`bamboo-specs/deploy.yaml`](../bamboo-specs/deploy.yaml)), and CatTunnel server has no macOS releases: the [releases](https://github.com/Cattunnel/cattunnel-server/releases) are Linux only (see the [README](../README.en.md#prebuilt-binaries)). You do not need this directory to build or run the server.

The description below is kept for anyone who builds a signed macOS binary on their own with their own credentials.

## Purpose

Developer ID signing and notarization of `trusttunnel_endpoint`. The flow was taken from the TrustTunnel client and adapted to a Cargo-based repository.

```text
 fastlane remove_certs   (drop the old keychain)
        |
 fastlane certs          (fetch the Developer ID certificate via match)
        |
 build and sign the binary
        |
 fastlane notari id:"<bundle_id>" bundle:"<path_to_binary>"
        |
 fastlane remove_certs   (drop the keychain)
```

Figure 1. The order of the lanes in the upstream CI (`bamboo-specs/deploy.yaml`).

## Prerequisites

- Xcode command line tools
- Ruby with Bundler
- access to the certificate repository used by `match`
- an App Store Connect API key (Bamboo environment variables)

If the Xcode command line tools are missing:

```sh
xcode-select --install
```

Ruby dependencies (`Gemfile` in the repository root):

```sh
bundle config --local path '.bundle/vendor'
bundle install
```

## Environment

Defaults are in `.env.default`. Main variables:

- `BUILD_DIR`
- `MATCH_GIT_URL`
- `MATCH_PASSWORD`
- `MATCH_KEYCHAIN_PASSWORD`
- `MATCH_KEYCHAIN_NAME`
- `MATCH_APP_IDENTIFIER`
- `bamboo_appStoreConnectApiKeyId`
- `bamboo_appStoreConnectApiKeyIssuerId`
- `bamboo_appStoreConnectApiKeyBase64Password`

## Lanes

### `certs`

```sh
[bundle exec] fastlane certs
```

Fetches the Developer ID certificate needed for macOS signing.

### `remove_certs`

```sh
[bundle exec] fastlane remove_certs
```

Removes the temporary local keychain created for signing.

### `notari`

```sh
[bundle exec] fastlane notari id:"<bundle_id>" bundle:"<path_to_binary>"
```

Notarizes the binary with the default App Store Connect key.

Required options:

- `id`: bundle identifier for notarization
- `bundle`: path to the signed binary

## Notes

- Executables are compressed into a temporary archive before notarization.
- Stapling is skipped for executables on purpose.
- The directory is maintained by hand and is not generated.
