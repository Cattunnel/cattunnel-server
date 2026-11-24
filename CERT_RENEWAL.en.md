Русский: [CERT_RENEWAL.md](CERT_RENEWAL.md) (the Russian text is the main one; where they differ, the Russian text is correct)

# Let's Encrypt certificate renewal

## Table of contents

- [Prerequisites](#prerequisites)
- [Install Certbot](#install-certbot)
- [Issue the certificate](#issue-the-certificate)
    - [Option A: standalone](#option-a-standalone)
    - [Option B: webroot](#option-b-webroot)
- [Use the Certbot certificate in the server settings](#use-the-certbot-certificate-in-the-server-settings)
- [Enable automatic renewal](#enable-automatic-renewal)
- [How the server picks up the new certificate](#how-the-server-picks-up-the-new-certificate)
- [Test renewal](#test-renewal)
- [Troubleshooting](#troubleshooting)

The CatTunnel server needs a valid TLS certificate. The `trusttunnel_endpoint --setup-wizard` command can issue one, but for a long-lived setup use Let's Encrypt with [Certbot][certbot] and enable automatic renewal. If you issued a Let's Encrypt certificate without Certbot, issue it again with Certbot, otherwise it will not renew by itself.

The procedure is the same as in TrustTunnel ([original](https://github.com/TrustTunnel/TrustTunnel/blob/master/CERT_RENEWAL.md)): the server reads the certificate and key from `hosts.toml` the same way the TrustTunnel endpoint does. The binary name `trusttunnel_endpoint` and the service name `trusttunnel` are unchanged in the fork (see the [README](README.en.md)).

[certbot]: https://eff-certbot.readthedocs.io/en/stable/

```text
 Certbot (timer)            Let's Encrypt            CatTunnel server
       |  1. certbot renew        |                        |
       |------------------------->|                        |
       |  2. HTTP-01 check        |                        |
       |<-------------------------|  port 80/tcp           |
       |  3. new certificate      |                        |
       |<-------------------------|                        |
       |  4. updates files in /etc/letsencrypt/live/       |
       |  5. deploy hook: systemctl restart trusttunnel -->|
       |                          |   6. reads the new files|
```

Figure 1. Renewal: Certbot updates the files, the deploy hook restarts the service, the server reads the new files.

## Prerequisites

- A public DNS name (A/AAAA record) pointing to the server.
- Port **80/tcp** reachable from the Internet during issuance and renewal (HTTP-01 validation).
- Root access on the server.

This guide uses HTTP-01. A wildcard certificate needs DNS-01.

## Install Certbot

Use the method recommended for your distribution. Example:

```bash
# Debian/Ubuntu
sudo apt update
sudo apt install -y certbot
```

## Issue the certificate

Choose one of the modes.

### Option A: standalone

Use it when nothing listens on port 80. Certbot starts its own web server for a moment.

```bash
sudo certbot certonly --standalone -d example.com
```

### Option B: webroot

Use it when an HTTP server is already running. It must serve `/.well-known/acme-challenge/` from the webroot.

```bash
sudo certbot certonly --webroot -w /var/www/html -d example.com
```

After issuance Certbot prints the paths you need:

```console
Certificate is saved at: /etc/letsencrypt/live/example.com/fullchain.pem
Key is saved at:         /etc/letsencrypt/live/example.com/privkey.pem
```

These files are symlinks that Certbot updates on every renewal.

## Use the Certbot certificate in the server settings

If you have no settings yet, run `trusttunnel_endpoint --setup-wizard` and choose "Provide path to existing certificate". Give the paths separated by a space, in any order.

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

If the settings already exist, change the paths in `hosts.toml`.

Was:

```toml
[[main_hosts]]
hostname = "example.com"
cert_chain_path = "certs/cert.pem"
private_key_path = "certs/key.pem"
```

Now:

```toml
[[main_hosts]]
hostname = "example.com"
cert_chain_path = "/etc/letsencrypt/live/example.com/fullchain.pem"
private_key_path = "/etc/letsencrypt/live/example.com/privkey.pem"
```

## Enable automatic renewal

On most modern Linux distributions Certbot installs a systemd timer. To check:

```bash
systemctl list-timers | grep -E 'certbot|letsencrypt'
```

Without systemd timers, use cron:

```bash
sudo crontab -e
```

Add:

```bash
0 3 * * * certbot renew --quiet
```

## How the server picks up the new certificate

Certbot updates the files, but the server reads them at start and when it reloads the TLS hosts settings. The simplest way is a deploy hook that restarts the service after each successful renewal:

```bash
sudo certbot reconfigure --deploy-hook "systemctl restart trusttunnel"
```

For Certbot older than 2.3.0, add this line to the `[renewalparams]` section of `/etc/letsencrypt/renewal/<yourdomain>.conf`:

```conf
renew_hook = systemctl restart trusttunnel
```

Replace `trusttunnel` with your real service name.

A restart is not required. The server reloads `hosts.toml` and the certificates on `SIGHUP` without stopping ([CONFIGURATION.md](CONFIGURATION.md#hot-reloading-tls-hosts-and-subscription-settings)), so the hook can send the signal: `systemctl kill -s HUP trusttunnel`.

## Test renewal

Run a dry run once to check that renewal works end to end:

```bash
sudo certbot renew --dry-run
```

With `--standalone`, port 80 must be free during the dry run.

## Troubleshooting

- **Port 80 is busy.** Stop the process for the renewal, or switch to `--webroot`.
- **DNS.** The name must resolve to the public IP of the server.
- **Firewall.** Allow inbound 80/tcp from the Internet.
- **Permissions.** The service must be able to read `/etc/letsencrypt/live/.../fullchain.pem` and `privkey.pem`.
