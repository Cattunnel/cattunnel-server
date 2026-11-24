Русский: [DEEP_LINK.md](DEEP_LINK.md). The Russian text is the main one; where the two differ, the Russian text is correct.

# The tt:// connection link

A `tt://` link carries the connection settings of a server from one device to another: one URI instead of a set of fields. The format was created in TrustTunnel and CatTunnel did not change it. The `deeplink` crate of the fork is the same as the TrustTunnel crate of version 1.3.1: there is no code difference. The server itself (`trusttunnel_endpoint`, the `-c` option) and the [`trusttunnel-deeplink`](deeplink/README.en.md) crate produce such links.

The format has no fields for NineLives or CatTail. Their parameters are not carried in `tt://`, see [docs/upstream-divergence.en.md](docs/upstream-divergence.en.md), section B4.

## Contents

- [Versions](#versions)
- [URI format](#uri-format)
- [Binary payload](#binary-payload)
- [Tags](#tags)
- [Encoding and parsing rules](#encoding-and-parsing-rules)
- [How the server issues a link](#how-the-server-issues-a-link)
- [Examples](#examples)
- [Versioning](#versioning)
- [Security](#security)

## Versions

The current format version is 2 (the `CURRENT_VERSION` constant in the crate).

| Version | What changed |
| --- | --- |
| 2 | The `subscription_url` field; when it is present, the static connection parameters are optional |
| 1 | Fields for the version, the server display name and DNS upstreams |
| draft 2 | The `tt://?` form instead of `tt://`: the data sits in the query, not in the host name, because the query keeps letter case |
| draft 1 | First edition |

The tags `0x0F` (`client_random_auth_key`) and `0x10` (`tls_profile`) were added later without a version change. A client that does not know a tag skips it and works with its defaults.

Tag `0x10` holds a numeric profile identifier. In the TrustTunnel pre-release `1.3.0-rc.3` it held the profile name as a string. The parser accepts only the numeric form, so a client that expects a string must be updated.

## URI format

```uri
tt://?<base64url payload>
```

- Scheme: `tt`.
- Payload: the settings in binary form, encoded with [Base64url](https://datatracker.ietf.org/doc/html/rfc4648#section-5) without `=` padding.

Base64url replaces `+` and `/` with `-` and `_`, so the result can be placed in a URI without escaping. The encoder always writes the `tt://?` prefix. The parser accepts both `tt://?` and `tt://` without the question mark.

```text
 settings (DeepLinkConfig)
   |  check: required fields present, subscription_url starts with https://
   v
 TLV entries: version, then the fields in a fixed order
   |
   v
 one byte buffer
   |  Base64url without padding
   v
 tt://?AAECAQ92cG4uZXhhbXBsZS5jb20...
```

Figure 1. How settings become a link.

## Binary payload

Each field is written as a tag, length, value triple (TLV):

| Part | Encoding | Meaning |
| --- | --- | --- |
| Tag | TLS varint | Field number, table below |
| Length | TLS varint | How many bytes the value takes |
| Value | as many bytes as the length says | Field content |

```text
 +-----------+-----------+-----------------------------+
 |    Tag    |  Length   |           Value             |
 |  varint   |  varint   | as many bytes as the length |
 |           |           | says; the tag sets the type |
 +-----------+-----------+-----------------------------+
   1 byte      1-2 bytes
   (tags < 64) (lengths < 16384)
```

Figure 2. One TLV entry.

### Variable-length integers

The tag and the length use the encoding of [RFC 9000, section 16](https://www.rfc-editor.org/rfc/rfc9000.html#section-16) (QUIC and TLS 1.3). The two high bits of the first byte give the size of the number:

| High 2 bits | Size | Usable bits | Maximum |
| --- | --- | --- | --- |
| `00` | 1 byte | 6 | 63 |
| `01` | 2 bytes | 14 | 16,383 |
| `10` | 4 bytes | 30 | 1,073,741,823 |
| `11` | 8 bytes | 62 | 4,611,686,018,427,387,903 |

Multi-byte numbers use network byte order (most significant first). Tags fit in one byte today, and lengths up to 16,383 take one or two bytes.

### Value types

| Type | What it is |
| --- | --- |
| VarInt | A variable-length integer, as above |
| Byte | One byte, `0x00`..`0xFF` |
| Bool | One byte: `0x01` true, `0x00` false; any other byte or length is an error |
| String | A UTF-8 string |
| Bytes | Raw bytes |
| String[] | A sequence of strings: each has a length (VarInt) first, then its UTF-8 bytes. The TLV value is the concatenation of all elements |

## Tags

| Tag | Field | Type | Content | Required |
| --- | --- | --- | --- | --- |
| `0x00` | `version` | VarInt | Format version | no (default `0`) |
| `0x01` | `hostname` | String | Server name | yes * |
| `0x02` | `addresses` | String | One `address:port` (IP or domain); several addresses are written as several entries with this tag | yes * |
| `0x03` | `custom_sni` | String | A custom name for SNI | no |
| `0x04` | `has_ipv6` | Bool | Whether the client has IPv6 | no (default `true`) |
| `0x05` | `username` | String | Login | yes * |
| `0x06` | `password` | String | Password | yes * |
| `0x07` | `skip_verification` | Bool | Do not verify the server certificate | no (default `false`) |
| `0x08` | `certificate` | Bytes | DER certificates one after another; omit it when system root certificates verify the chain | no |
| `0x09` | `upstream_protocol` | Byte | `0x01` `http2`, `0x02` `http3` | no (default `http2`) |
| `0x0A` | `anti_dpi` | Bool | Turn on anti-DPI measures | no (default `false`) |
| `0x0B` | `client_random_prefix` | String | A hex string `prefix[/mask]` | no |
| `0x0C` | `name` | String | Server name for display in the interface | no |
| `0x0D` | `dns_upstreams` | String[] | DNS upstream addresses, for example `1.1.1.1`, `tls://dns.example.com`, `https://dns.example.com/dns-query` | no |
| `0x0E` | `subscription_url` | String | HTTPS address of the subscription; it may hold a login and password (Basic) | no |
| `0x0F` | `client_random_auth_key` | String | A key in hex; the client derives the TLS `client_random` from it and the SNI | no |
| `0x10` | `tls_profile` | Byte | ClientHello fingerprint profile, table below | no |

\* Required unless the `subscription_url` tag (`0x0E`) is present. When it is present, the static connection parameters are optional and serve as a fallback for the first attempt if the first subscription fetch fails.

The `tls_profile` values:

| Value | Profile |
| --- | --- |
| `0x00` | `chrome` |
| `0x01` | `chrome_canary` |
| `0x02` | `safari` |
| `0x03` | `firefox` |
| `0x04` | `okhttp` |
| `0x05` | `openssl` |
| `0x06` | `boringssl` (no mimicry) |

The numbers are set by the format itself and match `VpnTlsProfile` in the [TrustTunnelClient](https://github.com/TrustTunnel/TrustTunnelClient/blob/main/common/include/vpn/utils.h) client. In the CatTunnel client the profile list differs from these numbers (see section B4 in [docs/upstream-divergence.en.md](docs/upstream-divergence.en.md)). The CatTunnel server does not put `tls_profile` into a link. When the tag is absent, the format tells the client to use `chrome`. In the reference script `scripts/config_to_deeplink.py` the old name `default` is accepted as a synonym of `boringssl`.

## Encoding and parsing rules

1. Fields may come in any order. The crate encoder writes them like this: version, `hostname`, `username`, `password`, all `addresses`, `client_random_prefix`, `client_random_auth_key`, `tls_profile`, `custom_sni`, `has_ipv6`, `skip_verification`, `anti_dpi`, `certificate`, `upstream_protocol`, `name`, `dns_upstreams`, `subscription_url`.
2. Tag `0x02` may repeat, each entry adds one address. Every other tag is written at most once; if repeated, the last entry wins.
3. The encoder omits a field that holds its default: `has_ipv6` is written only for `false`, `skip_verification` and `anti_dpi` only for `true`, `upstream_protocol` only for `http3`. Empty `client_random_prefix` and `client_random_auth_key` are omitted too.
4. The encoder always writes version `2` as the first entry.
5. The parser skips unknown tags by their declared length, so the format can grow without breaking old clients.
6. The parser rejects: a version above 2; an entry whose length is larger than the remaining data; a Bool that is not exactly one byte `0x00` or `0x01`; an `upstream_protocol` other than `0x01` and `0x02`; a `tls_profile` that is not one byte in `0x00`..`0x06`; a string that is not UTF-8; a `client_random_prefix` or `client_random_auth_key` that is not hex (in the prefix both parts, before and after `/`, are checked).
7. Without `subscription_url`, a non-empty `hostname`, `username` and `password` and at least one address are required. With `subscription_url`, any other tag may be omitted: a "subscription only" link consists of tags `0x00` and `0x0E`, and the client needs a successful subscription fetch before it can connect.
8. `subscription_url` must start with `https://`; the check runs both when encoding and when parsing.

```text
 tt://?<base64url>
   |
   +-- no tt:// prefix            -> scheme error
   v
 Base64url without padding
   +-- does not decode            -> Base64 error
   v
 loop over TLV: tag, length, value
   +-- length exceeds the rest    -> truncated entry
   +-- unknown tag                -> skip, go on
   +-- version above 2            -> version error
   +-- value of the wrong form    -> field error
   v
 final check
   +-- subscription_url present   -> it must start with https://
   '-- absent                     -> hostname, address, login, password required
```

Figure 3. The order in which the crate parses a link.

## How the server issues a link

The command prints a link for a user from `credentials.toml`:

```shell
trusttunnel_endpoint vpn.toml hosts.toml -c <username> -a <address>
```

`--format` sets the output format, `deeplink` by default; `toml` prints an ordinary client settings file. The `--subscription-only` option issues a subscription-only link: it needs a `[subscription]` section in `vpn.toml` and, unless `--subscription-url` is set, a subscription host certificate that system root certificates can verify. The other options (`--custom-sni`, `--client-random-prefix`, `--client-random-auth-key`, `--name`, `--dns-upstream`) go into the matching fields. The full list of options: [CONFIGURATION.md](CONFIGURATION.md).

After the link the command prints the address of a QR code page on the TrustTunnel project site (`https://trusttunnel.org/qr.html#tt=...`). It contains the whole link together with the password, so treat it like a password.

The link carries the name of the first main host of `hosts.toml` and the fixed values `has_ipv6 = true`, `skip_verification = false`, `upstream_protocol = http2`, `anti_dpi = false`; because they equal the defaults, these fields are not visible in the link. The server includes the certificate (DER) only when system root certificates cannot verify it. The server does not add the `tls_profile` tag to a link.

## Examples

The three links below were produced by running the `trusttunnel-deeplink` crate.

### A full link

Settings: `hostname = vpn.example.com`, address `203.0.113.10:443`, login `alice`, password `secret123`.

```text
 00 01 02                     version = 2
 01 0F "vpn.example.com"      hostname
 05 05 "alice"                username
 06 09 "secret123"            password
 02 10 "203.0.113.10:443"     address
```

```uri
tt://?AAECAQ92cG4uZXhhbXBsZS5jb20FBWFsaWNlBglzZWNyZXQxMjMCEDIwMy4wLjExMy4xMDo0NDM
```

`has_ipv6`, `skip_verification`, `anti_dpi` and `upstream_protocol` equal their defaults and are not in the link.

### A custom SNI and the firefox profile

The same fields, login `premium`, password `s3cretPass`, plus `custom_sni = example.org` and `tls_profile = firefox`. Two entries were added: `10 01 03` (profile `0x03`) and `03 0B "example.org"`:

```uri
tt://?AAECAQ92cG4uZXhhbXBsZS5jb20FB3ByZW1pdW0GCnMzY3JldFBhc3MCEDIwMy4wLjExMy4xMDo0NDMQAQMDC2V4YW1wbGUub3Jn
```

### Subscription only

Two entries: the version and `subscription_url = https://alice:s3cret@vpn.example.com/subscription` (49 bytes, length `0x31`):

```uri
tt://?AAECDjFodHRwczovL2FsaWNlOnMzY3JldEB2cG4uZXhhbXBsZS5jb20vc3Vic2NyaXB0aW9u
```

`scripts/deeplink_to_config.py` turns a link back into a settings file, and `scripts/config_to_deeplink.py` does the opposite.

## Versioning

The version sits in tag `0x00`.

- If the tag is absent, the parser assumes version `0`.
- The value is encoded as a VarInt.
- A client rejects a link whose version is higher than the one it supports.
- A client accepts links whose version equals the supported one or is lower.

## Security

- The login and password are in the link in clear text (after decoding). In `subscription_url` the credentials are also in clear text. Treat a link like a password and do not write it to logs without need.
- If `certificate` is set and `skip_verification` is `false`, the client verifies the server certificate against the given chain.
- A client should show the decoded settings to the user and wait for confirmation before connecting.
- Long certificate chains make long links. Implementations should accept links of at least 8 KiB. For a QR code, omit `certificate` when the server uses a publicly trusted certificate.
