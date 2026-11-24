Русский: [README.md](README.md). The Russian text is the main one; where the two differ, the Russian text is correct.

# The trusttunnel_endpoint binary

A standalone program: the CatTunnel server you can set up with one command. It is a thin wrapper over the [`trusttunnel`](../lib/README.en.md) library. The binary name comes from TrustTunnel, see the [root README](../README.en.md).

## Running

```shell
trusttunnel_endpoint vpn.toml hosts.toml
```

Two paths are required (except when running with `--version` or `--setup-wizard`): the main settings file and the TLS hosts file. Everything else comes from command line options and from the files these two point to.

## Configuration

- **TOML files.** The server reads two files. The first (`vpn.toml`) matches the `Settings` structure of the library. The second (`hosts.toml`) defines the TLS hosts the server presents to clients and matches the `TlsHostsSettings` structure. There are several kinds of hosts, each with its own job.
- **Also needed.** A file with user credentials (`credentials.toml`, its path is the `credentials_file` key of `vpn.toml`) and the certificate files the TLS hosts refer to. The optional rules file `rules.toml` is set by the `rules_file` key.
- **Setup wizard.** `trusttunnel_endpoint --setup-wizard` creates the required files. `--setup-wizard` must be the first argument on the command line, otherwise the program exits with an error. The options for a run without questions are shown by `trusttunnel_endpoint --setup-wizard -h`. Quick install: [README.en.md](../README.en.md).
- **Command line options.** The full list: `trusttunnel_endpoint -h`. The reference of all settings and options: [CONFIGURATION.en.md](../CONFIGURATION.en.md).

The main options:

| Option | Default | What it does |
| --- | --- | --- |
| `-v`, `--version` | none | Prints the version and exits |
| `-l`, `--loglvl` | `info` | Log level: `info`, `debug`, `trace` |
| `--logfile` | stdout | File for the log |
| `--jobs` | number of CPUs | Number of worker threads |
| `--sentry_dsn` | not set | Sentry address for error reports; without it Sentry does not start |
| `-c`, `--client_config <name>` | none | Prints client settings for a user and exits, see below |

On Unix the open file limit is raised at start to the smaller of the hard limit and 65536.

## Exporting client settings

The `-c` option (`--client_config`) prints the connection settings for a user from `credentials.toml` and exits at once; the server does not start. The other options of this mode:

| Option | Default | What it does |
| --- | --- | --- |
| `-a`, `--address` | none | Server address for the client: `ip`, `ip:port`, `domain` or `domain:port`. The default port comes from `listen_address`. May be repeated. Without it the address from `[subscription]` is used when the subscription is enabled |
| `-s`, `--custom-sni` | none | A custom SNI name; it must equal `hostname` or an `allowed_sni` of a main host |
| `-r`, `--client-random-prefix` | none | A `client_random` prefix (hex); `rules.toml` must have a rule for it |
| `--client-random-auth-key` | none | A key for `client_random` (hex); `rules.toml` must have a rule with this key. Cannot be combined with `-r` and `--generate-client-random-prefix` |
| `--generate-client-random-prefix` | none | Creates a new prefix, appends an allow rule to `rules.toml` and puts the prefix into the settings |
| `--prefix-length` | `4` | Length of the created prefix in bytes |
| `--prefix-percent` | `70` | Share of one bits in the mask |
| `--prefix-mask` | none | A custom mask in hex; cannot be combined with `--prefix-length` and `--prefix-percent` |
| `-f`, `--format` | `deeplink` | `deeplink` prints a `tt://?` link, `toml` prints a client settings file |
| `-n`, `--name` | none | Server name to show in the app |
| `-d`, `--dns-upstream` | none | A DNS upstream for the client settings; may be repeated |
| `--subscription-url` | from `[subscription]` | A custom base part of the subscription address; login and password are added from `credentials.toml` |
| `--subscription-only` | none | A subscription-only link, without static parameters |

Example:

```shell
trusttunnel_endpoint vpn.toml hosts.toml -c alice -a vpn.example.com
```

```text
 vpn.toml + hosts.toml + credentials.toml
   |
   v
 -c alice: is the user in credentials.toml? else an error
   |
   +-- addresses: from -a, else from [subscription], else an error
   +-- -s: is the name in hosts.toml? else an error
   +-- -r or --client-random-auth-key (if rules_file is set): is there a rule, and not deny? else an error
   |   (--generate-client-random-prefix: the rule is appended)
   v
 --format deeplink -> a tt://?... link and the address of a QR code page
 --format toml     -> a client settings file
 (the server does not start, the program exits)
```

Figure 1. What happens with `-c`.

The settings always carry the name of the first main host of `hosts.toml`. The values that have no option are fixed: `has_ipv6 = true`, `skip_verification = false`, `upstream_protocol = "http2"`, `anti_dpi = false`. The server includes the certificate when system root certificates cannot verify it. The server does not add the `tls_profile` tag. The link format: [DEEP_LINK.md](../DEEP_LINK.en.md).

## Reload on a signal

The `SIGHUP` signal re-reads the TLS hosts file and the `[subscription]` section of `vpn.toml` without a restart.

```text
 kill -HUP <pid>
   |
   v
 re-read hosts.toml -------------> apply the TLS hosts
   |                               (read or parse error: the process exits)
   v
 re-read [subscription] from vpn.toml
   |
   +-- valid -> apply the subscription
   '-- error -> write to the log, keep the old subscription
```

Figure 2. The order of a `SIGHUP` reload.

The other keys of `vpn.toml` are not re-read on `SIGHUP`: they need a restart. Check the TLS hosts file before you send the signal, because the program exits if it cannot read or apply it.

`Ctrl+C` (`SIGINT`) starts a shutdown: the server signals its tasks to stop and waits for them to finish.

## License

Apache 2.0.
