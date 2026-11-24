Русский: [PROTOCOL.md](PROTOCOL.md) (the Russian text is the main one; where the two differ, the Russian text is correct)

# TrustTunnel Protocol Specification (as used by CatTunnel)

**Specification revision 1.0** (upstream, Dec 24, 2025)

CatTunnel, a fork of [TrustTunnel](https://github.com/TrustTunnel/TrustTunnel),
uses this same base protocol. This document describes the common protocol. What
the fork adds on top of it is collected in section 1.4 and in separate documents:
[docs/connect.md](docs/connect.md), [docs/ninelives.md](docs/ninelives.md),
[docs/cattail.en.md](docs/cattail.en.md),
[docs/upstream-divergence.md](docs/upstream-divergence.md).

The words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119. The endpoint
code is in this repository (`lib/src/`), the client code is in the
[cattunnel](https://github.com/Cattunnel/cattunnel) repository.

## Abstract

TrustTunnel is a VPN protocol that tunnels network traffic through standard
HTTP/2 and HTTP/3 (QUIC) connections. It relies on widely used web protocols, so
the traffic looks like regular HTTPS and is harder for network intermediaries to
detect and block.

This document specifies the protocol completely, so that independent
implementations work with existing clients and endpoints.

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Protocol Overview](#2-protocol-overview)
3. [Transport Layer](#3-transport-layer)
4. [Session Establishment](#4-session-establishment)
5. [TCP Connection Tunneling](#5-tcp-connection-tunneling)
6. [UDP Multiplexing](#6-udp-multiplexing)
7. [ICMP Multiplexing](#7-icmp-multiplexing)
8. [Health Checks](#8-health-checks)
9. [Authentication](#9-authentication)
10. [Error Handling](#10-error-handling)
11. [Wire Formats](#11-wire-formats)
12. [Security Considerations](#12-security-considerations)
13. [Implementation Notes](#13-implementation-notes)
14. [Appendix A: Reserved Pseudo-Hosts](#appendix-a-reserved-pseudo-hosts)
15. [Appendix B: Example Message Flows](#appendix-b-example-message-flows)
16. [Appendix C: Version History](#appendix-c-version-history)
17. [References](#references)

---

## 1. Introduction

### 1.1 Purpose

TrustTunnel tunnels arbitrary network traffic (TCP, UDP and ICMP) through
encrypted HTTP/2 or HTTP/3 connections. The protocol has these goals:

- **Stealth**: traffic looks like standard HTTPS and is hard to tell apart from
  regular web browsing.
- **Performance**: many connections are multiplexed efficiently over one
  transport session.
- **Reliability**: sessions can be recovered and health-checked.
- **Flexibility**: both HTTP/2 (over TLS) and HTTP/3 (over QUIC) are supported.

### 1.2 Terminology

| Term           | Definition                                                           |
|----------------|----------------------------------------------------------------------|
| **Client**     | The application that opens connections to the endpoint              |
| **Endpoint**   | The server where tunneled connections terminate                     |
| **Session**    | An HTTP/2 or HTTP/3 connection between client and endpoint          |
| **Stream**     | An individual HTTP stream within a session                          |
| **Connection** | A tunneled TCP, UDP, or ICMP flow                                   |

### 1.3 Conventions

- All multi-byte integers are transmitted in **network byte order** (big-endian).
- IP addresses are transmitted in binary form (4 bytes for IPv4, 16 bytes for IPv6).
- In fixed-length formats, an IPv4 address is zero-padded to 16 bytes.

### 1.4 CatTunnel and this protocol

CatTunnel uses this same protocol: an HTTP CONNECT request over HTTP/2 or
HTTP/3, the pseudo-hosts `_check`, `_udp2` and `_icmp`, and
`Proxy-Authorization: Basic` authentication. The CatTunnel endpoint parses the UDP
and ICMP packet formats of sections 6 and 7 as described here
(`lib/src/http_udp_codec.rs`, `lib/src/http_icmp_codec.rs`). The code of the fork
has no protocol version number of its own. "Revision 1.0" is the number of the
upstream specification text.

The fork adds the following. The request format stays the same.

| What | Upstream | CatTunnel | Details |
|---|---|---|---|
| 200 response to a CONNECT to a host | 200 without headers | web server headers: `date`, `server: nginx`, `cache-control`, `x-request-id`, `x-trace-id` | [docs/connect.md](docs/connect.md), section 3.1 |
| DATA frames to the client | as much as flow control allows | at most 4096 bytes on CONNECT streams | [docs/connect.md](docs/connect.md), section 3.2 |
| Headers of the client CONNECT request | `user-agent`, `proxy-authorization` | the same plus the headers a browser sends | `connect-client.md` in the [cattunnel](https://github.com/Cattunnel/cattunnel) repository |
| Address listed in `decoy_only_ips_file` | none | any tunnel request is refused as if the password were wrong | [docs/connect.md](docs/connect.md), section 3.4 |
| Other ways to carry the same tunnel | none | NineLives, the CatTail outer layer | [docs/ninelives.md](docs/ninelives.md), [docs/cattail.md](docs/cattail.md) |

The full list of differences: [docs/upstream-divergence.md](docs/upstream-divergence.md).
Repositories: client [cattunnel](https://github.com/Cattunnel/cattunnel), routers
[cattunnel-router](https://github.com/Cattunnel/cattunnel-router), endpoint
[cattunnel-server](https://github.com/Cattunnel/cattunnel-server).

```text
 TCP :443 ---------------------+         TCP, CatTail port
                               |                 |
                               |       +---------v---------+
                               |       | CatTail: removes  |
                               |       | the outer layer   |
                               |       +---------+---------+
                               v                 v
                      +--------------------------------+
                      | TlsListener: TLS               |
                      +----------------+---------------+
                                       v
                      +--------------------------------+
                      | HttpDemux: channel per request |
                      +---+------------+----------+----+
                          v            v          v
                       Tunnel        Xhttp     subscription,
                       (CONNECT)   (NineLives) decoy site, ping,
                                               speedtest
```

Figure 1. Where the transports meet the endpoint. The CONNECT tunnel (this
document) goes to the Tunnel channel. NineLives is selected in the same request
demultiplexer, and CatTail sits in front of TLS and carries the same TLS bytes.

---

## 2. Protocol Overview

### 2.1 Architecture

```text
┌─────────────────────────────────────────────────────────────────┐
│                 CatTunnel or TrustTunnel client                 │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────────┐ │
│  │ TCP Streams │  │ UDP Stream   │  │ ICMP Stream             │ │
│  │ (per-conn)  │  │ (multiplexed)│  │ (multiplexed)           │ │
│  └──────┬──────┘  └──────┬───────┘  └───────────┬─────────────┘ │
│         │                │                      │               │
│         └────────────────┼──────────────────────┘               │
│                          │                                      │
│                ┌─────────▼─────────┐                            │
│                │  HTTP/2 or HTTP/3 │                            │
│                │     Session       │                            │
│                └─────────┬─────────┘                            │
│                          │                                      │
│                ┌─────────▼─────────┐                            │
│                │   TLS or QUIC     │                            │
│                └─────────┬─────────┘                            │
└──────────────────────────┼──────────────────────────────────────┘
                           │
                    ┌──────▼──────┐
                    │   Internet  │
                    └──────┬──────┘
                           │
┌──────────────────────────┼──────────────────────────────────────┐
│                ┌─────────▼─────────┐                            │
│                │   TLS or QUIC     │                            │
│                └─────────┬─────────┘                            │
│                ┌─────────▼─────────┐                            │
│                │  HTTP/2 or HTTP/3 │                            │
│                │     Session       │                            │
│                └─────────┬─────────┘                            │
│         ┌────────────────┼─────────────────────┐                │
│         │                │                     │                │
│  ┌──────▼──────┐  ┌──────▼───────┐  ┌───────────▼─────────────┐ │
│  │ TCP Streams │  │ UDP Stream   │  │ ICMP Stream             │ │
│  │ (per-conn)  │  │ (multiplexed)│  │ (multiplexed)           │ │
│  └─────────────┘  └──────────────┘  └─────────────────────────┘ │
├─────────────────────────────────────────────────────────────────┤
│                CatTunnel or TrustTunnel endpoint                │
└─────────────────────────────────────────────────────────────────┘
```

Figure 2. Layers of the client and the endpoint. TCP, UDP and ICMP share one
HTTP/2 or HTTP/3 session over TLS or QUIC.

### 2.2 Connection Model

Different protocols use different strategies.

- **TCP**: each TCP connection gets its own HTTP stream through the CONNECT method.
- **UDP**: all UDP datagrams are multiplexed over a single HTTP stream.
- **ICMP**: all ICMP echo requests and replies are multiplexed over a single HTTP stream.

---

## 3. Transport Layer

### 3.1 HTTP/2 Transport

When using HTTP/2:

- the client establishes a TLS 1.2 or newer connection;
- ALPN negotiation MUST select `h2`;
- the initial stream window size is **131072 bytes** (Chrome's value);
- standard HTTP/2 flow control applies.

The value 131072 is set in the client (`core/src/http2_upstream.cpp`, constant
`HTTP2_STREAM_INITIAL_WINDOW_SIZE`). The endpoint has the same default (key
`initial_stream_window_size` in `[listen_protocols.http2]`, 128 KiB), and its
default connection window is 8 MiB (`initial_connection_window_size`).

The endpoint also accepts the ALPN `http/1.1`, but the client core opens the tunnel
only over `h2` or `h3`.

**TLS Requirements:**

- TLS 1.2 or newer (the endpoint uses the default versions of the rustls library);
- server certificate verification is REQUIRED;
- SNI MUST be sent.

### 3.2 HTTP/3 Transport

When using HTTP/3:

- the client establishes a QUIC connection;
- ALPN negotiation MUST select `h3`;
- the default QUIC version is `0x00000001` (RFC 9000);
- standard QUIC flow control applies.

**QUIC Configuration:**

- The client sets the maximum idle timeout to `2 × (connection_timeout + health_check_timeout)`.
  With default values that is 2 × (30 + 7) = 74 seconds (`core/src/http3_upstream.cpp`,
  `core/include/vpn/vpn.h`).
- The endpoint sets the maximum idle timeout to `client_listener_timeout_secs`,
  600 seconds by default (`lib/src/quic_multiplexer.rs`, `lib/src/settings.rs`).

### 3.3 Protocol Selection

Clients MAY select the protocol automatically:

1. Try HTTP/3.
2. After a configurable delay (default 1000 ms), start a fallback connection over HTTP/2.
3. Use whichever protocol establishes first.

---

## 4. Session Establishment

### 4.1 Connection Flow

1. The client starts a TLS or QUIC handshake with the endpoint.
2. The client verifies the endpoint certificate.
3. HTTP/2: the client sends a SETTINGS frame.
4. HTTP/3: the client opens an HTTP/3 session after the QUIC handshake.
5. The session counts as established when the first stream can be opened.

```text
 client                                          endpoint
   |== TLS ClientHello (SNI, ALPN h2) ============>|
   |<=========== ServerHello, certificate ==========|  client verifies the certificate
   |== HTTP/2 SETTINGS ===========================>|
   |== HEADERS CONNECT host:port ================>|  first stream open:
   |                                               |  session established
```

Figure 3. Session establishment over HTTP/2. Over HTTP/3 a QUIC handshake replaces
TLS, followed by an HTTP/3 session.

### 4.2 Endpoint Discovery

An endpoint is identified by:

- **Address**: IP address and port (for example, `192.0.2.1:443`).
- **Hostname**: used for SNI and certificate verification.
- **Remote ID** (optional): another identifier for certificate verification.

---

## 5. TCP Connection Tunneling

### 5.1 Opening a TCP Tunnel

To tunnel a TCP connection, the client sends an HTTP CONNECT request:

```http
CONNECT example.com:443 HTTP/2
:method: CONNECT
:authority: example.com:443
user-agent: <platform> <app_name>
proxy-authorization: Basic <base64(username:password)>
```

**Headers:**

| Header                | Required   | Description                                  |
|-----------------------|------------|----------------------------------------------|
| `:method`             | Yes        | Must be `CONNECT`                            |
| `:authority`          | Yes        | Target host and port as `host:port`          |
| `user-agent`          | By client  | Platform and application name                |
| `proxy-authorization` | Yes        | Basic authentication credentials             |

The client always sends `user-agent`. The endpoint reads it as the application
name and does not require it (`lib/src/http_codec.rs`, function `user_agent`). A
request without `proxy-authorization` is refused as an authentication failure when
authentication is configured (section 5.2). A header that does not start with
`Basic ` is treated as a request error, and the endpoint answers 502 with
`x-warning: 300`.

The CatTunnel client adds the headers a browser sends to the request (`accept`,
`accept-language`, `sec-ch-ua`, `sec-fetch-*` and others). The endpoint does not use
the extra headers. The list is in `connect-client.md` in the
[cattunnel](https://github.com/Cattunnel/cattunnel) repository.

**Authority Format:**

- IPv4: `192.0.2.1:443`
- IPv6: `[2001:db8::1]:443`
- hostname: `example.com:443`

### 5.2 Endpoint Response

**Success (200 OK):**

```http
HTTP/2 200
```

After a 200 response the stream becomes a bidirectional byte stream. Everything the
client sends on the stream goes to the target host, and everything the target host
sends goes back to the client on the same stream.

Upstream answers with an empty 200. The CatTunnel endpoint adds web server headers
to the answer to a tunnel CONNECT (`date`, `server: nginx`,
`cache-control: no-cache, no-store, must-revalidate`, `x-request-id`, `x-trace-id`)
and sets neither `content-length` nor `transfer-encoding` (`lib/src/http_codec.rs`).
Answers to `_check`, `_udp2` and `_icmp` stay a plain 200.

**Authentication Required (407):**

```http
HTTP/2 407
proxy-authenticate: Basic realm=Authorization Required
```

The failure status is set by `auth_failure_status_code` (407 by default; 407, 405,
404 and 403 are allowed). Non-CONNECT requests have their own
`non_connect_auth_failure_status_code`. The endpoint adds `proxy-authenticate` only
to 407. A request that exceeds the connection limit gets the same status, and so
does a request from an address listed in `decoy_only_ips_file` (see
[docs/connect.md](docs/connect.md), section 3.4).

A client that receives 407 on any stream closes the session with an authentication
error and has to authenticate again.

**Other Errors:**

The endpoint reports every other error with status 502 and an `x-warning` header:

| Cause                               | `x-warning`                                | Extra header                      |
|-------------------------------------|---------------------------------------------|-----------------------------------|
| I/O error, anything else            | `300 - Connection failed for some reason`   |                                   |
| Host unreachable                    | `301 - ` and the error text                 |                                   |
| Connection timeout                  | `302 - ` and the error text                 |                                   |
| Address not routable (DNS)          | `310 - ` and the error text                 | `x-adguard-vpn-error: <hostname>`  |
| Loopback address (DNS)              | `311 - ` and the error text                 | `x-adguard-vpn-error: <hostname>`  |

The client MUST treat any response other than 200 as a connection failure.

```text
 request arrives at the endpoint
        |
        v
 HttpDemux: subscription path, ping, speedtest, NineLives, decoy site?
        | yes -> their own handlers
        | no (Tunnel channel)
        v
 client address in decoy_only_ips_file?
        | yes -> answer as for a wrong password (407 or the set status)
        | no
        v
 check Proxy-Authorization (and the connection limit)
        | refused -> 407 or the set status
        v
 by authority and method:
   _check + CONNECT                 -> 200, stream closed (END_STREAM)
   _udp2 or _icmp + CONNECT         -> 200, datagram multiplexer
   _check, _udp2, _icmp, not CONNECT -> 502
   anything else                    -> rules, TCP connect, 200, byte channel
```

Figure 4. What the endpoint does with a tunnel request. The order is taken from
`lib/src/http_downstream.rs` and `lib/src/tunnel.rs`.

### 5.3 Data Transfer

After a successful CONNECT:

- the endpoint forwards client data to the target;
- the endpoint forwards target data to the client;
- standard HTTP/2 or HTTP/3 flow control applies;
- closing the stream ends the connection.

On HTTP/2 CONNECT streams the CatTunnel endpoint cuts DATA frames to the client to
4096 bytes (`lib/src/http2_codec.rs`, constant `CONNECT_DATA_CHUNK`).

The endpoint closes an idle TCP connection after `tcp_connections_timeout_secs`
(604800 seconds by default, one week).

### 5.4 Connection Closure

- **Graceful**: the `END_STREAM` flag in HTTP/2 or `FIN` in HTTP/3.
- **Abrupt**: `RST_STREAM` in HTTP/2 or `STOP_SENDING` and `RESET_STREAM` in HTTP/3.

---

## 6. UDP Multiplexing

### 6.1 Overview

All UDP traffic goes over a single HTTP stream to reduce overhead. The stream is
opened with a CONNECT request to a pseudo-host.

### 6.2 Stream Establishment

```http
CONNECT _udp2 HTTP/2
:method: CONNECT
:authority: _udp2
user-agent: <platform> _udp2
proxy-authorization: Basic <base64(username:password)>
```

The authority `_udp2` is sent without a port and is reserved for the UDP
multiplexer. A request with another method gets 502.

### 6.3 Outgoing Packet Format (Client → Endpoint)

```text
+----------+----------------+-------------+---------------------+------------------+------------------+----------+---------+
|  Length  | Source Address | Source Port | Destination Address | Destination Port | App Name Len (L) | App Name | Payload |
| 4 bytes  |    16 bytes    |   2 bytes   |      16 bytes       |     2 bytes      |      1 byte      | L bytes  | N bytes |
+----------+----------------+-------------+---------------------+------------------+------------------+----------+---------+
```

**Field Descriptions:**

| Field               | Size     | Description                                         |
|---------------------|----------|-----------------------------------------------------|
| Length              | 4 bytes  | Packet length excluding this field                  |
| Source Address      | 16 bytes | Source IP (IPv4 zero-padded to 16 bytes)            |
| Source Port         | 2 bytes  | Source UDP port                                     |
| Destination Address | 16 bytes | Destination IP (IPv4 zero-padded to 16 bytes)       |
| Destination Port    | 2 bytes  | Destination UDP port                                |
| App Name Length     | 1 byte   | Length of the application name (0 to 255)           |
| App Name            | L bytes  | Application name, UTF-8                             |
| Payload             | N bytes  | UDP payload                                         |

The endpoint drops a packet if `Length` is below 37 (the size of the fields after
`Length` without name and payload), if the packet is too large, or if the
application name is not UTF-8.

**IPv4 Padding:**
An IPv4 address is transmitted as 16 bytes with 12 leading zero bytes:

```text
00 00 00 00 00 00 00 00 00 00 00 00 XX XX XX XX
```

### 6.4 Incoming Packet Format (Endpoint → Client)

```text
+----------+----------------+-------------+---------------------+------------------+---------+
|  Length  | Source Address | Source Port | Destination Address | Destination Port | Payload |
| 4 bytes  |    16 bytes    |   2 bytes   |      16 bytes       |     2 bytes      | N bytes |
+----------+----------------+-------------+---------------------+------------------+---------+
```

Packets from the endpoint to the client have no application name field.

### 6.5 Connection Tracking

The endpoint tracks UDP "connections" by the 4-tuple:

- source address;
- source port;
- destination address;
- destination port.

The endpoint closes an idle UDP "connection" after `udp_connections_timeout_secs`,
**300 seconds** by default.

---

## 7. ICMP Multiplexing

### 7.1 Overview

ICMP echo requests (ping) are multiplexed over a single HTTP stream, like UDP. The
endpoint serves them only if its configuration has an `[icmp]` section. Without it
an ICMP request is not served (`lib/src/tunnel.rs`, message "ICMP forwarding isn't
set up").

### 7.2 Stream Establishment

```http
CONNECT _icmp HTTP/2
:method: CONNECT
:authority: _icmp
user-agent: <platform> _icmp
proxy-authorization: Basic <base64(username:password)>
```

### 7.3 Echo Request Format (Client → Endpoint)

```text
+----------+---------------------+-----------------+---------------+-----------+
|    ID    | Destination Address | Sequence Number | TTL/Hop Limit | Data Size |
| 2 bytes  |      16 bytes       |     2 bytes     |    1 byte     |  2 bytes  |
+----------+---------------------+-----------------+---------------+-----------+
```

**Field Descriptions:**

| Field               | Size     | Description                                  |
|---------------------|----------|----------------------------------------------|
| ID                  | 2 bytes  | ICMP identifier                              |
| Destination Address | 16 bytes | Target IP (IPv4 zero-padded)                 |
| Sequence Number     | 2 bytes  | ICMP sequence number                         |
| TTL/Hop Limit       | 1 byte   | IP TTL or IPv6 hop limit                     |
| Data Size           | 2 bytes  | Size of the echo data (data is not sent)     |

The client does not send the echo data itself: the endpoint fills it with random
bytes of the declared size.

### 7.4 Echo Reply Format (Endpoint → Client)

```text
+----------+----------------+--------+--------+-----------------+
|    ID    | Source Address |  Type  |  Code  | Sequence Number |
| 2 bytes  |    16 bytes    | 1 byte | 1 byte |     2 bytes     |
+----------+----------------+--------+--------+-----------------+
```

**Field Descriptions:**

| Field           | Size     | Description                                                     |
|-----------------|----------|-----------------------------------------------------------------|
| ID              | 2 bytes  | ICMP identifier (matches the request)                           |
| Source Address  | 16 bytes | Address of the responding host                                  |
| Type            | 1 byte   | ICMP type (IPv4: 0 echo reply, 3 destination unreachable, 11 time exceeded) |
| Code            | 1 byte   | ICMP code                                                       |
| Sequence Number | 2 bytes  | ICMP sequence number (matches the request)                      |

A reply can be an echo reply or an ICMP error message that carries the original
echo request.

---

## 8. Health Checks

### 8.1 Purpose

A health check shows that the session still works and can process requests.

### 8.2 Health Check Request

The client sends a CONNECT request to a pseudo-host:

```http
CONNECT _check HTTP/2
:method: CONNECT
:authority: _check
user-agent: <platform> unknown
proxy-authorization: Basic <base64(username:password)>
```

The application name is empty for a health check, so the client puts the word
`unknown` into `user-agent`.

### 8.3 Health Check Response

- **200 OK**: the session is healthy. The endpoint closes the stream at once (`END_STREAM`).
- **407**: authentication failure.
- **Other**: the session may be unhealthy.
- A `_check` request with a method other than CONNECT gets 502.

### 8.4 Health Check Timing

- Default health check timeout: **7000 ms**.
- The client issues a check after opening a connection or sending data if at least
  `timeout_ms` (30 seconds by default) has passed since the previous one, and when
  the device wakes up.
- Any data received from the endpoint cancels a pending check.
- If a check is not confirmed within the timeout, the client starts session
  recovery (section 10.3).

The CatTunnel client also uses `_check` as a keepalive: the endpoint waits for the
next request on a connection for at most `client_listener_timeout_secs` (600
seconds by default), and a tiny CONNECT to `_check` restarts that wait. Details:
[docs/connect.md](docs/connect.md), section 6.

---

## 9. Authentication

### 9.1 Basic Authentication

TrustTunnel uses HTTP Basic Authentication:

```text
proxy-authorization: Basic <credentials>
```

where `<credentials>` is

```text
base64(username + ":" + password)
```

### 9.2 Authentication Errors

| HTTP Status | Error Code      | Description                          |
|-------------|-----------------|--------------------------------------|
| 407         | `AUTH_REQUIRED` | Invalid or expired credentials       |

The failure status can be changed with `auth_failure_status_code` (section 5.2).
The client core recognizes an authentication failure only by status 407
(`core/src/http2_upstream.cpp`), so another status suits only clients that need to
look like an ordinary site.

When it receives a 407 response, the client MUST:

1. Close the current session.
2. Obtain new credentials (if possible).
3. Establish a new session.

---

## 10. Error Handling

### 10.1 HTTP/2 Error Codes

| Error Code        | Meaning                    |
|-------------------|----------------------------|
| `NO_ERROR` (0x0)  | Graceful shutdown          |
| `CANCEL` (0x8)    | Stream cancelled by client |
| Custom: `0x64`    | Authentication required    |

The upstream text of the specification gives the custom code as `0x1F`. In the
CatTunnel client code it is `0x64` (`net/include/net/http_session.h`), and on a
GOAWAY with this code the client treats the session as rejected for authentication.
The endpoint in this repository does not send such a code: it reports an
authentication failure with the response status (section 5.2).

### 10.2 HTTP/3 Error Codes

| Error Code                     | Meaning                          |
|--------------------------------|----------------------------------|
| `H3_NO_ERROR` (0x100)          | Graceful shutdown                |
| `H3_REQUEST_CANCELLED` (0x10c) | Stream cancelled                 |
| Custom: `0x64`                 | Authentication required          |

### 10.3 Session Recovery

When a session is lost:

1. The client enters recovery mode.
2. The interval between reconnection attempts grows exponentially:
   - initial interval: **1000 ms**;
   - backoff rate: **1.3**;
   - location update period: **10000 ms**;
   - number of attempts: **30** (after that the client moves to the disconnected
     state with the "location unavailable" error).
3. New connections during recovery:
   - if the killswitch is enabled: they are not routed directly;
   - if the killswitch is disabled: they are routed directly, bypassing the tunnel.

```text
 session lost
      |
      v
 attempt 1 --1000 ms--> attempt 2 --(x1.3)--> attempt 3 --(x1.3)--> ... up to 30 attempts
      |
      +-- every 10000 ms during a long recovery: select the endpoint again
      |
      +-- killswitch off: new connections go directly
          killswitch on:  new connections do not go directly
```

Figure 5. Session recovery. Defaults are from `core/include/vpn/vpn.h`.

---

## 11. Wire Formats

### 11.1 Integer Encoding

All integers are encoded in **network byte order** (big-endian):

```text
uint16: [MSB][LSB]
uint32: [MSB][...][...][LSB]
```

### 11.2 IP Address Encoding

**IPv4 (padded to 16 bytes):**

```text
00 00 00 00 00 00 00 00 00 00 00 00 [4 bytes IPv4]
```

**IPv6 (16 bytes):**

```text
[16 bytes IPv6]
```

**Detection:**
The endpoint treats an address as IPv4 if its first 12 bytes are zero. The upstream
text says `::1` (the IPv6 loopback) is an exception, but the endpoint code has no
such exception (`lib/src/net_utils.rs`, function `get_fixed_size_ip`): `::1` would be
read as the IPv4 address `0.0.0.1`.

### 11.3 Authority String Format

The `:authority` header uses these formats:

| Type     | Format           | Example             |
|----------|------------------|---------------------|
| IPv4     | `address:port`   | `192.0.2.1:443`     |
| IPv6     | `[address]:port` | `[2001:db8::1]:443` |
| Hostname | `hostname:port`  | `example.com:443`   |

The pseudo-hosts `_udp2`, `_icmp` and `_check` are sent without a port.

---

## 12. Security Considerations

### 12.1 Transport Security

- All traffic MUST be encrypted with TLS 1.2 or newer, or with QUIC.
- Certificate verification MUST be performed.
- Clients SHOULD support certificate pinning.

### 12.2 Credential Security

- Credentials are sent in every CONNECT request.
- Base64 gives no protection: TLS provides confidentiality.
- Implementations SHOULD support credential rotation.

### 12.3 Traffic Analysis Resistance

TrustTunnel gives some resistance to traffic analysis:

- traffic looks like standard HTTPS;
- several connections are multiplexed;
- but traffic patterns may still differ.

CatTunnel adds measures against recognition (a web-server style 200 answer, DATA
frames of 4096 bytes, browser headers, a browser TLS profile, the CatTail outer
layer). They are not a guarantee: the traffic profile can still give the tunnel away.

### 12.4 Endpoint Trust

- The endpoint sees all tunneled traffic (decrypted at the endpoint).
- End-to-end encryption (for example, HTTPS inside the tunnel) adds protection.
- Connect only to endpoints you trust.

---

## 13. Implementation Notes

### 13.1 Flow Control

**HTTP/2:**

- Respect stream and connection window sizes.
- Initial stream window: 131072 bytes.
- Send WINDOW_UPDATE frames in time.

**HTTP/3:**

- Use QUIC flow control.
- Check stream capacity before sending.

### 13.2 Connection Buffering

Implementations SHOULD buffer data when:

- flow control prevents sending it at once;
- the client is temporarily unable to receive.

Buffer limits in the client core:

- the memory threshold is a setting (default `DEFAULT_CONNECTION_MEMORY_BUFFER_SIZE`);
- the file buffer is up to 4 MiB (`core/include/vpn/vpn.h`).

### 13.3 Timeouts

| Timeout                         | Default  | Description                                              |
|---------------------------------|----------|----------------------------------------------------------|
| Client connection to endpoint   | 30 s     | Time to establish a connection to the endpoint           |
| Endpoint connection to target   | 30 s     | `connection_establishment_timeout_secs`                  |
| Health check                    | 7 s      | Time to receive the health check response                |
| TCP idle                        | 604800 s | Idle TCP connections (one week, client and endpoint)     |
| UDP idle                        | 300 s    | Idle UDP "connections" (client and endpoint)             |
| Wait for a request on a session | 600 s    | `client_listener_timeout_secs` of the endpoint           |
| TLS handshake                   | 10 s     | `tls_handshake_timeout_secs` of the endpoint             |

The values in the upstream specification (TCP idle 7200 s, UDP 120 s) differ from the
CatTunnel code: see the table.

### 13.4 Split Tunneling

Clients MAY implement split tunneling:

- **General mode** (`VPN_MODE_GENERAL`): everything goes through the endpoint except exclusions.
- **Selective mode** (`VPN_MODE_SELECTIVE`): only what is listed goes through the endpoint.

Exclusions can be given as:

- domain names (`example.com` covers the domain and its `www` subdomain, `*.example.com`
  covers any subdomain but not the domain itself);
- IP addresses (with or without a port);
- CIDR ranges;
- a port for any address (`*:443`).

### 13.5 DNS Handling

Clients MAY intercept DNS queries and:

- route them through the tunnel;
- use their own DNS upstreams (`tls://`, `https://`, `quic://`, `sdns://`);
- resolve excluded domains locally.

---

## Appendix A: Reserved Pseudo-Hosts

| Host     | Port                      | Purpose                   |
|----------|---------------------------|---------------------------|
| `_udp2`  | none (authority has no port) | UDP multiplexer stream |
| `_icmp`  | none (authority has no port) | ICMP multiplexer stream |
| `_check` | none (authority has no port) | Health check stream    |

All three are accepted only with the CONNECT method. With another method the
endpoint answers 502.

---

## Appendix B: Example Message Flows

### B.1 TCP Connection

```text
Client                                    Endpoint                              Target
   |                                          |                                    |
   |----CONNECT example.com:443-------------->|                                    |
   |                                          |----TCP SYN------------------------>|
   |                                          |<---TCP SYN-ACK---------------------|
   |                                          |----TCP ACK------------------------>|
   |<---HTTP 200 OK---------------------------|                                    |
   |                                          |                                    |
   |----DATA (TLS ClientHello)--------------->|----DATA (TLS ClientHello)--------->|
   |<---DATA (TLS ServerHello)----------------|<---DATA (TLS ServerHello)----------|
   |    ...                                   |    ...                             |
```

Figure 6. Opening a TCP tunnel and the first data.

### B.2 UDP Datagram

```text
Client                                    Endpoint                              Target
   |                                          |                                    |
   |----CONNECT _udp2------------------------>|                                    |
   |<---HTTP 200 OK---------------------------|                                    |
   |                                          |                                    |
   |----[UDP Packet: src→dst, payload]------->|----UDP datagram------------------->|
   |<---[UDP Packet: dst→src, response]-------|<---UDP datagram--------------------|
```

Figure 7. Opening the UDP stream and exchanging datagrams.

---

## Appendix C: Version History

| Version | Date          | Changes                           |
|---------|---------------|-----------------------------------|
| 1.0     | Dec 24, 2025  | Initial public specification (upstream) |

CatTunnel has no protocol version numbering of its own in the code.

---

## References

- [RFC 9000](https://datatracker.ietf.org/doc/html/rfc9000): QUIC
- [RFC 9114](https://datatracker.ietf.org/doc/html/rfc9114): HTTP/3
- [RFC 9113](https://datatracker.ietf.org/doc/html/rfc9113): HTTP/2
- [RFC 7231](https://datatracker.ietf.org/doc/html/rfc7231): HTTP/1.1 Semantics and Content
- [RFC 7235](https://datatracker.ietf.org/doc/html/rfc7235): HTTP/1.1 Authentication
- [TrustTunnel (upstream)](https://github.com/TrustTunnel/TrustTunnel)
- [CatTunnel](https://github.com/Cattunnel/cattunnel), [cattunnel-router](https://github.com/Cattunnel/cattunnel-router), [cattunnel-server](https://github.com/Cattunnel/cattunnel-server)
