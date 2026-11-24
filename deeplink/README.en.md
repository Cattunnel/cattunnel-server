Русский: [README.md](README.md). The Russian text is the main one; where the two differ, the Russian text is correct.

# The trusttunnel-deeplink crate

A standalone Rust library that encodes and parses `tt://` links, which carry the connection settings of a server. The crate comes from TrustTunnel unchanged (the fork code has no difference from TrustTunnel 1.3.1); the crate name stayed the same, see the [root README](../README.en.md). The link format is described in [DEEP_LINK.md](../DEEP_LINK.en.md).

## What the crate does

- Encodes and parses every tag of the format (`0x00`..`0x10`), format version 2.
- Returns typed errors (`DeepLinkError`) with readable messages.
- Works with Base64url without padding.
- Converts certificates between PEM and DER.
- Is covered by tests: round trips, properties (proptest) and a comparison with the Python scripts.

How the two main functions work:

```text
 encode(&config)                           decode(uri)
   |                                          |
   v                                          v
 field check (validate)                     tt://? or tt:// prefix
   |                                          |
   v                                          v
 TLV entries, version 2                     Base64url -> bytes
   |                                          |
   v                                          v
 Base64url without padding                  parse TLV, unknown tags are skipped
   |                                          |
   v                                          v
 the tt://?... string                       field check (validate) -> DeepLinkConfig
```

Figure 1. What `encode` and `decode` do.

## Adding the crate

Add the crate to `Cargo.toml` from the CatTunnel server repository:

```toml
[dependencies]
trusttunnel-deeplink = { git = "https://github.com/Cattunnel/cattunnel-server" }
```

Inside this tree use `path = "deeplink"`. The `serde` feature is on by default: it adds `Serialize` and `Deserialize` for `DeepLinkConfig`, `Protocol` and `TlsProfile`. Turn it off with `default-features = false`.

## Quick start

### Encoding

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

Addresses are `address:port` strings (an IP or a domain). A link starts with the version tag (`AAEC` at the start means `00 01 02`), so every version 2 link starts the same way.

### Parsing

```rust
use trusttunnel_deeplink::decode;

let uri = "tt://?AAECAQ92cG4uZXhhbXBsZS5jb20FBWFsaWNlBglzZWNyZXQxMjMCEDIwMy4wLjExMy4xMDo0NDM";
let config = decode(uri).unwrap();

println!("Hostname: {:?}", config.hostname);
println!("Username: {:?}", config.username);
```

## Settings fields

The `DeepLinkConfig` struct has these fields:

| Field | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `hostname` | `Option<String>` | yes * | `None` | Server name |
| `addresses` | `Vec<String>` | yes * | `[]` | Server addresses (`address:port`) |
| `username` | `Option<String>` | yes * | `None` | Login |
| `password` | `Option<String>` | yes * | `None` | Password |
| `custom_sni` | `Option<String>` | no | `None` | A custom name for SNI |
| `has_ipv6` | `bool` | no | `true` | IPv6 is available |
| `skip_verification` | `bool` | no | `false` | Do not verify the certificate |
| `certificate` | `Option<Vec<u8>>` | no | `None` | Certificate chain in DER |
| `upstream_protocol` | `Protocol` | no | `Http2` | Protocol to the server: `Http2` or `Http3` |
| `anti_dpi` | `bool` | no | `false` | Anti-DPI measures |
| `client_random_prefix` | `Option<String>` | no | `None` | TLS `client_random` prefix, hex |
| `client_random_auth_key` | `Option<String>` | no | `None` | Key for `client_random`, hex |
| `name` | `Option<String>` | no | `None` | Server name for display |
| `dns_upstreams` | `Vec<String>` | no | `[]` | DNS upstream addresses |
| `tls_profile` | `Option<TlsProfile>` | no | `None` | ClientHello fingerprint profile; with `None` the tag is not written to the link |
| `subscription_url` | `Option<String>` | no | `None` | HTTPS subscription address (version 2) |

\* Required unless `subscription_url` is set. With it, the static parameters are optional and serve as a fallback.

The `TlsProfile` values are `Chrome`, `ChromeCanary`, `Safari`, `Firefox`, `OkHttp`, `OpenSsl`, `BoringSsl`. In serde and in `FromStr` they are named `chrome`, `chrome_canary`, `safari`, `firefox`, `okhttp`, `openssl`, `boringssl`. The reference script `scripts/config_to_deeplink.py` also accepts the deprecated name `default` as a synonym of `boringssl`; the crate does not know it.

## More

### Certificates

```rust
use trusttunnel_deeplink::cert::{der_to_pem, pem_to_der};

// PEM (one or several blocks) to DER, one after another
let pem = "-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----";
let der = pem_to_der(pem).unwrap();

// back to PEM
let pem_again = der_to_pem(&der).unwrap();
```

### The builder

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

`build()` checks the fields and returns an error if a required one is missing, if `client_random_prefix` or `client_random_auth_key` is not hex, or if `subscription_url` does not start with `https://`.

### A subscription-only link

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

## Errors

All functions return `DeepLinkError`:

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

The main variants are `InvalidScheme`, `InvalidBase64`, `TruncatedTlv`, `MissingRequiredField`, `InvalidSubscriptionUrl`, `InvalidProtocol`, `InvalidTlsProfile`, `UnsupportedVersion`, `InvalidBoolean`, `InvalidUtf8`, `InvalidCertificate`. The full list is in `deeplink/src/error.rs`.

## Tests

```shell
# all tests of the crate
cargo test -p trusttunnel-deeplink

# round trips
cargo test -p trusttunnel-deeplink --test roundtrip

# comparison with the Python scripts
cargo test -p trusttunnel-deeplink --test python_compat

# property tests
cargo test -p trusttunnel-deeplink --test proptest
```

`python_compat` needs `python3` in `PATH` and the script `scripts/config_to_deeplink.py` in the repository root.
