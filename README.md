# Bifrost

**English** · [中文](README.zh-CN.md)

> An L2-over-TCP virtual LAN that pulls scattered Linux hosts into one
> Ethernet broadcast domain.
>
> Written in Rust · Linux-only daemon · designed to run on top of an
> existing encrypted tunnel (Xray-core, V2Ray, Shadowsocks, plain
> SOCKS5).

Bifrost takes a small number of Linux hosts that may be behind NAT,
firewalls, or geographically dispersed and stitches them into a single
Layer-2 network. The server creates a Linux bridge; each client creates
a local TAP interface; raw Ethernet frames travel between them as
length-prefixed `postcard` records over a single TCP connection.

It deliberately ships **no built-in encryption** — the design assumes
you already terminate or originate your traffic inside an encrypted
overlay (Xray's VLESS-XTLS, Shadowsocks, a plain WireGuard tunnel, an
SSH `-D` SOCKS5, …) and that adding another layer of crypto would be
duplicative. The wire protocol is plaintext on the inside but is
trivially carried by any of those tunnels — the SOCKS5 client is built
in.

---

## Why Bifrost?

If you've ever wanted to *combine* the niceties of a self-hosted L2 VPN
(broadcast traffic, ARP, non-IP protocols, "looks like one switch")
with the *NAT- and censorship-handling* of a tool like
[Xray-core](https://github.com/XTLS/Xray-core) or
[V2Ray](https://github.com/v2fly/v2ray-core), Bifrost is for you.
Most existing VPNs either:

* embed their own UDP-based crypto and have to re-invent NAT
  traversal (Tailscale, ZeroTier, WireGuard), or
* speak L3 only and lose broadcast domains (most of the above).

Bifrost picks a different point in the design space: **plaintext L2
frames inside a single TCP stream**. That stream is just bytes — drop
it through Xray's VLESS, Shadowsocks-2022, or any SOCKS5 proxy and the
encryption + censorship-resistance + DPI-evasion problems are
delegated to a tool that already solves them well.

### Comparison

|                          | **Bifrost**                              | Tailscale                                 | ZeroTier                                  | WireGuard                  |
|--------------------------|------------------------------------------|-------------------------------------------|-------------------------------------------|----------------------------|
| Topology                 | Hub-and-spoke (single server)            | Mesh + DERP relay fallback                | Mesh + planet/moon root servers           | Point-to-point or manual mesh |
| Network layer            | **L2 (Ethernet)**                        | L3 (IP)                                   | L2 (Ethernet)                             | L3 (IP)                    |
| Transport                | **TCP** (or via SOCKS5)                  | UDP, TCP fallback                         | UDP, TCP fallback                         | UDP                        |
| Encryption               | **None — bring your own tunnel**         | WireGuard                                 | Salsa20 + Poly1305                        | ChaCha20Poly1305           |
| NAT traversal            | Outbound TCP only; **SOCKS5 native**     | STUN + relays via Tailscale's DERP        | STUN + ZT controllers                     | Manual port forwarding     |
| Coordinator / control    | Self-hosted single binary, no cloud      | Tailscale cloud (or self-hosted Headscale)| ZeroTier cloud (or self-hosted)           | None                       |
| Member approval          | Manual `approve <sid>` then auto         | OAuth, ACL files                          | Web console + API                         | Static key exchange        |
| Mobile clients           | No                                        | Yes (iOS / Android)                       | Yes                                       | Yes                        |
| Best fit                 | You already run Xray / V2Ray / SS        | Plug-and-play LAN over the internet       | Mesh L2 with member sprawl                | Minimal-trust point-to-point |

### When **not** to use Bifrost

* You want a turnkey app with mobile clients and zero ops — use
  Tailscale.
* You need full mesh data plane (any-to-any without going through a
  central host) — use ZeroTier or Tailscale.
* You need maximum throughput on a fast LAN — kernel-mode WireGuard or
  raw `bridge` will outperform anything in user space.
* You need built-in PKI / authentication primitives independent of an
  outer tunnel.

### When **to** use Bifrost

* You already have an Xray / V2Ray / Shadowsocks deployment doing the
  hard work of crypto + DPI evasion + AS-level routing, and you want
  L2 connectivity layered on top of it without re-implementing that
  stack.
* You're in a network where only outbound TCP to specific ports
  works, and UDP-based VPNs simply do not connect.
* You need real Ethernet semantics — broadcast, ARP, non-IP protocols,
  multicast — between a small fleet of hosts.
* You want a self-hosted, no-vendor-cloud solution with a single
  binary on each side.

---

## Architecture

```
┌─ Client (e.g. router, aarch64) ─────┐         ┌─ Server (Linux, x86_64) ───────────┐
│  bifrost-client (daemon)            │         │  bifrost-server (daemon)            │
│  ├─ TAP   tapXXXX  (10.0.0.2/24)    │         │  ├─ Bridge  br-bifrost (10.0.0.1)   │
│  ├─ ConnTask  (TCP / SOCKS5)        │         │  ├─ TAP × N  (one per client)       │
│  ├─ App  (state machine)            │         │  ├─ Hub  (single actor)             │
│  └─ admin  /run/bifrost/client.sock │         │  ├─ ConnTask × N                    │
│                                     │         │  ├─ SessionTask × N                 │
└────────────────┬────────────────────┘         │  └─ admin  /run/bifrost/server.sock │
                 │                               └──────────────────┬──────────────────┘
                 │       postcard-framed wire protocol over TCP     │
                 │  (optionally tunneled through SOCKS5 → Xray /    │
                 │   V2Ray / Shadowsocks / SSH -D / stunnel / ...)   │
                 └──────────────────────────────────────────────────┘
```

**Design highlights**

* **Hub is a single actor.** All control state — networks,
  approved clients, routes, sessions, pending requests, conns — is
  owned by a single `tokio::select!` loop. External components send
  `HubCmd` messages; no shared locking required.
* **Data plane bypasses the Hub.** When Hub approves a join it hands
  the new ConnTask a `Sender<SessionCmd>` for the bound SessionTask.
  After that, every Ethernet frame flows
  `socket → ConnTask → SessionTask → TAP` directly — the Hub is only
  involved in control events.
* **Session is the long-lived state.** A `SessionTask` survives
  reconnects so the local TAP, its IP, and its routes are preserved
  across transient network hiccups. Server-side it has a
  configurable disconnect timeout; client-side it never expires by
  itself.

---

## Quick start

Below assumes you have a Linux server with a routable address (or
reachable through a SOCKS5 tunnel) and one Linux client. SSH access
with key-based auth is required.

```bash
# 1. Build for both targets (needs Docker + cross-rs)
cargo install cross --git https://github.com/cross-rs/cross
./scripts/build-cross.sh

# 2. Deploy
SERVER_HOST=root@<server-ip>  ./scripts/deploy-server.sh
CLIENT_HOST=root@<client-ip>  ./scripts/deploy-client.sh

# 3. Bootstrap a network
ssh root@<server-ip> 'bifrost-server admin mknet hml-net'
# → network created  uuid=<NET_UUID>

# 4. Have the client request to join
ssh root@<client-ip> "bifrost-client admin join <NET_UUID>"

# 5. Approve on the server side
ssh root@<server-ip> 'bifrost-server admin list'             # find the sid
ssh root@<server-ip> 'bifrost-server admin approve <SID>'

# 6. Assign the client's TAP IP
PREFIX=$(ssh root@<client-ip> \
  'grep ^uuid /etc/bifrost/client.toml | cut -d\" -f2 | cut -c1-8')
ssh root@<server-ip> "bifrost-server admin setip $PREFIX 10.0.0.2/24"

# 7. Verify
ssh root@<server-ip> 'ping -c3 10.0.0.2'
```

Subsequent reconnects auto-reuse the persisted state — both
`approved_clients` and `joined_network` are written to TOML at the
appropriate moments.

---

## Build

### Local development (macOS / Linux)

```bash
cargo build  --workspace
cargo test   --workspace                                 # ~120 tests
cargo clippy --workspace --all-targets -- -D warnings
```

On macOS the binaries link against `NullPlatform` — they can run the
admin RPC, REPL, and protocol-layer logic, but `create_tap` returns
`Unsupported` because TAP / bridge are Linux-kernel concepts. This
keeps the development cycle fast on a Mac while still
exercising every line of the cross-platform code.

### Cross-compiling for production

`Cross.toml` is configured for the two main deployment targets:

```bash
./scripts/build-cross.sh
# Produces:
#   target/x86_64-unknown-linux-gnu/release/bifrost-server   (~4 MB)
#   target/aarch64-unknown-linux-gnu/release/bifrost-client  (~4 MB)
```

Both targets use the default `cross-rs` images (Debian-based, glibc
2.x), which are wider-compatibility than typical deployment hosts.
Verified to run on Arch Linux x86_64 (kernel 6.18) and Ubuntu 24.04
aarch64 (kernel 6.x).

If you need musl static binaries for very old / unusual hosts, change
the target to `*-unknown-linux-musl` — no code changes required.

---

## Deployment

### One-time: passwordless SSH for `root`

The deploy scripts run as `root` on the target hosts (TAP creation
requires `CAP_NET_ADMIN` and netlink access). If you only have a
sudo-capable user, install your key into root's `authorized_keys`
once:

```bash
ssh-copy-id <user>@<host>          # or scp your pubkey manually

PUBKEY=$(cat ~/.ssh/id_ed25519.pub)
ssh <user>@<host> "sudo bash -c 'mkdir -p /root/.ssh && \
  chmod 700 /root/.ssh && \
  echo \"$PUBKEY\" >> /root/.ssh/authorized_keys && \
  chmod 600 /root/.ssh/authorized_keys'"

ssh root@<host> 'whoami'           # expect: root
```

### Deploy with the provided scripts

```bash
SERVER_HOST=root@<server-ip>  ./scripts/deploy-server.sh
CLIENT_HOST=root@<client-ip>  ./scripts/deploy-client.sh
```

Each script does:

1. `systemctl stop` the running daemon (avoids `ETXTBSY` on overwrite)
2. `scp` the binary to `/usr/local/bin/`
3. `scp` the example TOML and systemd unit (does **not** overwrite an
   existing `client.toml` / `server.toml`)
4. `systemctl daemon-reload && systemctl enable --now`
5. Print `systemctl status` for verification

### systemd integration

Units are bundled at `deploy/systemd/`:

* `bifrost-server.service` — `Type=simple`, runs as root,
  `ExecStartPre` ensures `/run/bifrost` and the save directory exist.
* `bifrost-client.service` — same shape; depends on
  `network-online.target`.

Both units use `Restart=on-failure` with a 2-second delay, so the
daemon comes back automatically after a crash without flooding logs.

### Connecting through Xray / V2Ray / Shadowsocks

The client's `[proxy]` block speaks plain SOCKS5. For the typical
Xray-core deployment you'd run:

```bash
xray run -c /etc/xray/config.json     # listens on 127.0.0.1:10808 (SOCKS5)
```

Then `client.toml`:

```toml
[client]
host = "<server-ip-reachable-by-xray>"
port = 8888

[proxy]
enabled = true
host = "127.0.0.1"
port = 10808
```

The client's TCP handshake to the server now travels:

```
bifrost-client  ─┐
                 │  SOCKS5 CONNECT  ┌─ Xray (VLESS-XTLS to remote relay) ─┐
                 ▼                  ▼                                       ▼
            127.0.0.1:10808 ──→ <relay>:443 ──── encrypted ────→ <server-ip>:8888
                                                                            │
                                                                            ▼
                                                                     bifrost-server
```

The Xray tunnel handles all the crypto + obfuscation; Bifrost just
sees a TCP stream.

---

## Configuration

### `server.toml`

```toml
[server]
host = "0.0.0.0"
port = 8888
save_dir = "/var/lib/bifrost/received"

[bridge]
name = "br-bifrost"
ip = "10.0.0.1/24"           # leave empty to skip giving the bridge an address
disconnect_timeout = 60       # seconds; how long a TAP outlives its socket

[admin]
socket = "/run/bifrost/server.sock"

[metrics]                     # reserved, not yet wired
enabled = false
listen = "127.0.0.1:9090"

# These three sections are populated automatically by the daemon:
# [[networks]]          ← `mknet`
# [[approved_clients]]  ← `approve` / `setip`
# [[routes]]            ← `route add` / `route del`
```

### `client.toml`

```toml
[client]
uuid = ""                     # generated on first run
host = "<server-ip>"
port = 8888
save_dir = "/var/lib/bifrost/received"
retry_interval = 5            # seconds between reconnect attempts
joined_network = ""           # written by daemon after first JoinOk

[proxy]
enabled = true
host = "127.0.0.1"
port = 10808                  # SOCKS5 port on the local host

[tap]
ip = ""                       # written by daemon on SetIp / JoinOk

[admin]
socket = "/run/bifrost/client.sock"
```

---

## Admin CLI

Both daemons run with **no REPL by default** (systemd-friendly) and
expose a Unix socket. All operations go through `admin <subcommand>`,
which performs a single request-reply RPC and exits.

### `bifrost-server admin`

| Command | Action |
|---|---|
| `mknet <name>` | Create a virtual network; returns its UUID. |
| `approve <sid>` | Admit a pending client; allocates a TAP and adds it to the bridge. |
| `deny <sid>` | Reject a pending client. |
| `setip <prefix> <ip>` | Assign / clear a client's TAP IP. Pushed online if the client is currently bound. |
| `route add <dst/cidr> via <gw>` | Persist a route in the server config. |
| `route del <dst>` | Remove a route. |
| `route list` | Same payload as `list`'s `routes` section. |
| `route push` | Send the current route table to every bound client. |
| `list` | Snapshot of networks, sessions, pending requests, and routes. |
| `send <msg>` | Broadcast a `Frame::Text` to every connected client. |
| `sendfile <path>` | Read a local file and broadcast it as `Frame::File`. |
| `shutdown` | Ask the daemon to exit cleanly. |

### `bifrost-client admin`

| Command | Action |
|---|---|
| `join <net-uuid>` | Request to join a network (waits for server approval). |
| `leave` | Leave the current network; destroys the local TAP. |
| `status` | Print the client's connection / TAP / config state. |
| `send <msg>` | Send a text message to the server. |
| `sendfile <path>` | Send a local file to the server's `save_dir`. |
| `shutdown` | Ask the daemon to exit cleanly. |

### Interactive REPL

If you'd rather drive the daemon from a foreground shell:

```bash
bifrost-server --repl       # commands match `admin <subcommand>`
bifrost-client --repl
```

The REPL shares the same dispatcher as the admin socket — single
source of truth for command behavior.

---

## Wire protocol (v1)

Frame format:

```
┌────────────────────────┬──────────────────────────────────┐
│  u32 BE  payload_len   │  postcard-encoded `Frame`        │
└────────────────────────┴──────────────────────────────────┘
```

`Frame` is a 12-variant enum. A summary:

| Variant | Direction | Purpose |
|---|---|---|
| `Hello` / `HelloAck` | C↔S | First-frame handshake; carries `version` and `caps` bits. |
| `Join` / `JoinOk` / `JoinDeny` | C→S / S→C | Network membership negotiation. |
| `Eth(Vec<u8>)` | bidirectional | Data plane: a single Ethernet frame. |
| `SetIp { ip }` | S→C | Online TAP-IP update. |
| `SetRoutes(Vec<RouteEntry>)` | S→C | Push a routing table to the client. |
| `Text(String)` | bidirectional | Out-of-band text broadcast / echo. |
| `File { name, data }` | bidirectional | Out-of-band file delivery. |
| `Ping(u64)` / `Pong(u64)` | bidirectional | Liveness, currently passive. |

The `caps` bit field is reserved for future transport-layer upgrades
(Noise, TLS) so that adding crypto later does not require a version
bump.

The admin RPC uses the same length-prefixed-postcard pattern but a
separate enum hierarchy, transported over a Unix socket.

---

## Project layout

```
bifrost/
├─ Cargo.toml / Cargo.lock         # workspace root
├─ Cross.toml                      # cross-compilation targets
│
├─ crates/
│  ├─ bifrost-proto/               # wire protocol & admin RPC types (no IO)
│  ├─ bifrost-net/                 # Tap / Bridge / Platform trait abstraction
│  │  ├─ src/null.rs                #   NullPlatform   (compiles everywhere)
│  │  ├─ src/mock.rs                #   MockPlatform   (test helper, feature-gated)
│  │  └─ src/linux/                 #   LinuxPlatform  (#[cfg(target_os="linux")])
│  ├─ bifrost-core/                # Hub actor, Session state machine, config
│  ├─ bifrost-server/              # daemon binary + driver lib
│  └─ bifrost-client/              # daemon binary + driver lib
│
├─ deploy/
│  ├─ server.toml.example
│  ├─ client.toml.example
│  └─ systemd/{bifrost-server.service, bifrost-client.service}
│
└─ scripts/
   ├─ build-cross.sh
   ├─ deploy-server.sh
   └─ deploy-client.sh
```

Crate dependency graph:

```
bifrost-server     bifrost-client
       │                 │
       └────┬────────────┘
            ▼
       bifrost-core
        │      │
        ▼      ▼
  bifrost-proto  bifrost-net
                  │
                  └─ #[cfg(target_os = "linux")]
                       linux::{LinuxTap, LinuxBridge, LinuxPlatform}
```

---

## Status

### Completed (P0)

* `bifrost-proto` — `Frame`, `FrameCodec`, admin RPC types
* `bifrost-net` — `Tap` / `Bridge` traits + `MockPlatform` +
  `NullPlatform` + Linux backend (rtnetlink + ioctl + `AsyncFd`)
* `bifrost-core` — Hub single-actor, Session state machine, atomic
  TOML persistence
* `bifrost-server` / `bifrost-client` — daemon binaries with admin
  Unix socket and optional REPL
* Cross-compilation for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu` via `cross-rs` + Docker
* systemd integration and one-line deploy scripts
* End-to-end production deployment verified against an Arch x86_64
  server and an Ubuntu 24.04 aarch64 client (tunnelling through
  Xray's SOCKS5)
* 120 tests passing, clippy-clean with `-D warnings`

### Roadmap

| Item | Notes |
|---|---|
| **Noise XX transport** | `Hello.caps` bit already reserved; add a `snow`-backed `Transport` impl, no business-logic changes |
| **Prometheus metrics** | `metrics-exporter-prometheus`; per-session bytes / frames / drops |
| **Per-session pcap dump** | `SessionCmd::PcapStart/Stop` already defined in core |
| **macOS / Windows clients** | `bifrost-net::macos::utun` (IP-only), `bifrost-net::windows::wintun` (full L2) |
| **`route list` standalone formatting** | Currently piggybacks on `list`'s snapshot path |

### Explicitly not planned

* Multi-server federation / mesh data plane — Bifrost is hub-and-spoke
  by design.
* UDP transport — TCP + SOCKS5 friendliness is the central design
  choice.
* Built-in DHCP / NAT — Linux's bridge module already covers these.

---

## Contributing

Issues and pull requests are welcome. A few quick conventions:

* **Tests are non-negotiable.** Any new behavior should come with
  either a unit test in the relevant `src/` file, an integration test
  under `tests/`, or both. Existing coverage is the working baseline.
* **`cargo clippy --workspace --all-targets -- -D warnings` must
  pass.** CI (when enabled) will reject otherwise. Same for
  `cargo fmt --all -- --check`.
* **Linux-only code lives behind `#[cfg(target_os = "linux")]`** so
  `cargo build` on macOS / Windows continues to succeed for the
  cross-platform layers.
* **Keep `unsafe` localized.** Currently the only `unsafe` is in
  `bifrost-net::linux::tap` (ioctl + raw read/write). The crate's
  top-level `#![deny(unsafe_code)]` enforces this; per-module
  `#[allow]` is the documented escape hatch.
* **Commit messages describe the *why*.** "fix typo" is fine; "make
  Hub send HelloAck because clients race on it" is better.

For larger changes (new platform backend, new transport, new admin
verb), please open an issue first to discuss the surface area before
investing in code.

---

## License

Licensed under either of

* Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license
  ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual-licensed as above, without any
additional terms or conditions.

---

## Acknowledgments

* The protocol design borrows the "control over a single multiplexed
  TCP stream + offload crypto to an outer tunnel" pattern from
  [shadowtun](https://github.com/shadowsocks/shadow-tls) /
  [stunnel](https://www.stunnel.org/) lineage.
* The single-actor Hub pattern follows
  [`tokio::select!` examples](https://tokio.rs/tokio/tutorial/select)
  and the broader Erlang / Akka tradition.
* `rtnetlink` and `netlink-packet-route` from the
  [rust-netlink](https://github.com/rust-netlink) project make the
  Linux backend pleasingly typed.

Named after the rainbow bridge connecting Asgard and Midgard in Norse
mythology — the same idea, smaller scale.
