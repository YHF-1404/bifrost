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

In addition to the classic CLI, recent versions ship a **WebUI** (HTTP
+ WebSocket, default `127.0.0.1:8080`) for inspecting networks,
admitting/kicking devices, editing per-device fields, pushing routes,
and visualising the topology as a draggable node graph. The frontend
is a small React app under `web/`; the backend is the same daemon
binary. See [WebUI](#webui).

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
| Member approval          | Per-device admit toggle (CLI + WebUI)    | OAuth, ACL files                          | Web console + API                         | Static key exchange        |
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
┌─ Client (e.g. router, aarch64) ─────┐         ┌─ Server (Linux, x86_64) ────────────┐
│  bifrost-client (daemon)            │         │  bifrost-server (daemon)             │
│  ├─ TAP   tapXXXX  (10.0.0.2/24)    │         │  ├─ Bridge × N  (one per net_uuid)   │
│  ├─ ConnTask  (TCP / SOCKS5)        │         │  ├─ TAP × M    (one per session)     │
│  ├─ App  (state machine)            │         │  ├─ Hub        (single actor)        │
│  └─ admin  /run/bifrost/client.sock │         │  ├─ ConnTask × N                     │
│                                     │         │  ├─ SessionTask × N                  │
│                                     │         │  ├─ admin   /run/bifrost/server.sock │
│                                     │         │  └─ WebUI HTTP / WS  (0.0.0.0:8080)  │
└────────────────┬────────────────────┘         └──────────────────┬───────────────────┘
                 │                                                  │
                 │       postcard-framed wire protocol over TCP     │
                 │  (optionally tunneled through SOCKS5 → Xray /    │
                 │   V2Ray / Shadowsocks / SSH -D / stunnel / ...)  │
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
* **Per-network L2 isolation (Phase 2).** Each virtual network gets
  its own Linux bridge (auto-derived `bf-<8-hex>` from the network
  UUID, or whatever `NetRecord.bridge_name` holds). A device's TAP
  is attached only to the bridge of its network — networks share no
  broadcast domain, no ARP, no routing table. `mknet` creates the
  kernel bridge; `delete_net` tears it down.
* **Session is the long-lived state.** A `SessionTask` survives
  reconnects so the local TAP, its IP, and its routes are preserved
  across transient network hiccups. Server-side it has a
  configurable disconnect timeout; client-side it never expires by
  itself.
* **Routes are derived, not configured.** Each admitted client
  declares the LAN subnets behind it (`lan_subnets`); the server
  stitches them into a per-network routing table at push time —
  installed on that network's bridge only, not globally. No more
  `[[routes]]` to keep in sync.
* **Two control surfaces, one source of truth.** The Unix-socket
  admin RPC (`bifrost-server admin <cmd>`) and the HTTP API
  (`/api/...`) both go through the same `HubHandle` methods —
  the WebUI is just another consumer of the data plane.

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

# 5. Admit and configure the client in one go.
#    A `device set` with `--admit true` promotes a pending row; without
#    it the client stays in pending state. Each flag is independent —
#    pass only the ones you want to change.
CLIENT_UUID=$(ssh root@<client-ip> \
  'grep ^uuid /etc/bifrost/client.toml | cut -d\" -f2')
ssh root@<server-ip> "bifrost-server admin device set $CLIENT_UUID \
  --admit true --name router --ip 10.0.0.2/24 --lan 192.168.200.0/24"

# 6. (later, optional) Kick a device back to pending without removing
#    its row, e.g. for maintenance:
#    ssh root@<server-ip> "bifrost-server admin device set $CLIENT_UUID --admit false"

# 7. Push the derived route table to all members
ssh root@<server-ip> "bifrost-server admin device push <NET_UUID>"

# 8. Verify
ssh root@<server-ip> 'ping -c3 10.0.0.2'
```

Optional — open `http://127.0.0.1:8080` (over an SSH port-forward) to
get the same picture in a browser. See [WebUI](#webui).

Subsequent reconnects auto-reuse the persisted state — both
`approved_clients` and `joined_network` are written to TOML at the
appropriate moments.

---

## Build

### Local development (macOS / Linux)

```bash
cargo build  --workspace
cargo test   --workspace                                 # ~164 tests
cargo clippy --workspace --all-targets -- -D warnings

# Frontend (optional — only if you're going to touch the WebUI)
cd web && npm install && npm run build
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

[bridge]                      # legacy global block — only `disconnect_timeout`
                              # is still authoritative; `name` / `ip` are
                              # ignored unless you're upgrading from a Phase-1
                              # config (see "Migration" below).
name = "br-bifrost"
ip = "10.0.0.1/24"
disconnect_timeout = 60       # seconds; how long a TAP outlives its socket

[admin]
socket = "/run/bifrost/server.sock"

[web]                         # `deploy/server.toml.example` ships
                              # `0.0.0.0:8080` so a VPS behind a network-level
                              # firewall can serve the WebUI directly. The
                              # in-code default is `127.0.0.1:8080` — flip
                              # whichever fits your setup.
enabled = true
listen = "0.0.0.0:8080"

[metrics]                     # reserved; no consumer wired yet
enabled = false
listen = "127.0.0.1:9090"

# These two sections are populated automatically by the daemon:
# [[networks]]          ← `mknet`
# [[approved_clients]]  ← `device set`
#
# Each [[networks]] row owns its kernel bridge (Phase 2):
#   name         = "hml-net"
#   uuid         = "..."
#   bridge_name  = "br-bifrost"        # auto-derived `bf-<8-hex>` for fresh
#                                      # networks; legacy bridge name preserved
#                                      # for the first network on upgrade
#   bridge_ip    = "10.0.0.1/24"       # empty = pure-L2, no host-side address
#
# Each [[approved_clients]] row carries:
#   client_uuid  = "..."               # set by the daemon
#   net_uuid     = "..."               # set by the daemon
#   tap_ip       = "10.0.0.2/24"       # `device set --ip`
#   display_name = "router"            # `device set --name`
#   lan_subnets  = ["192.168.200.0/24"]  # `device set --lan`
#   admitted     = true                # `device set --admit`
#
# Each network's route table is **derived** from its members'
# `lan_subnets` and installed on that network's bridge only —
# no `[[routes]]` section anywhere.
```

#### Migration from Phase 1 (single global bridge)

`ServerConfig::load` runs a one-shot migration: for every
`[[networks]]` row whose `bridge_name` is empty, the **first** one
inherits `[bridge].name` / `[bridge].ip` from the legacy block
(your existing kernel bridge keeps owning its network without
operator action), and any further networks auto-derive
`bf-<8-hex>` from their UUID. Once the rewritten config has been
saved back, the migration is a no-op on subsequent loads.

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
| `rename <net-uuid> <new-name>` | Rename an existing network. |
| `rmnet <net-uuid>` | Delete a network and cascade-remove its devices (kills live sessions, drops conns, clears persisted rows, re-syncs routes). |
| `device list [<net-uuid>]` | List devices (admitted + pending). Filter by network if given. |
| `device set <client-uuid> [--name X] [--ip Y/CIDR] [--admit BOOL] [--lan A,B,…]` | Mutate one device. Each flag is independent: omitted = no change; `--ip ""` clears; `--lan ""` clears the list. `--admit true` promotes a pending row to admitted (allocates the TAP, sends `JoinOk`); `--admit false` kicks a session back to pending (kills the live socket, leaves the row in place). Live `SET_IP` is pushed if the device is online. |
| `device push <net-uuid>` | Re-derive routes for the network (from every member's `lan_subnets`), install them on the server bridge, and send `SetRoutes` to each joined client. |
| `list` | Snapshot of networks, sessions, and pending requests. |
| `send <msg>` | Broadcast a `Frame::Text` to every connected client. |
| `sendfile <path>` | Read a local file and broadcast it as `Frame::File`. |
| `shutdown` | Ask the daemon to exit cleanly. |

> Earlier versions had `approve <sid>` / `deny <sid>` / `setip` /
> `route add/del/push`. All four are gone. **Admit/kick is now a
> field** on `device set` (`--admit true|false`); a kicked device
> stays as a pending row and a re-join lands back in pending state.
> The per-device `lan_subnets` field plus `device push` replaces the
> global routes table; `device set --ip` replaces `setip`. There is
> no migration path — Bifrost is alpha; remove old `[[routes]]` from
> `server.toml` by hand, then re-create them with `device set --lan`.

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

## WebUI

The server daemon ships an HTTP + WebSocket API that the React
frontend in `web/` consumes — same port serves the SPA, the REST API,
and the `/ws` event stream.

### Bind address

In code the default is **`127.0.0.1:8080`** — safe everywhere, reach
it via SSH `-L`:

```bash
ssh -L 8080:127.0.0.1:8080 root@<server-ip>
# then open http://127.0.0.1:8080 in your browser
```

The shipped `deploy/server.toml.example`, on the other hand, sets
`listen = "0.0.0.0:8080"` so direct browser access works on a VPS
behind a provider-level firewall (no auth, do **not** bind publicly
without a network-level gate). Override per-deploy via the `[web]`
block, or with the CLI flags `--web-listen <addr>` / `--no-web`.

### Endpoints

| Method | Path | Behavior |
|---|---|---|
| `GET` | `/api/networks` | Network list with `device_count` / `online_count`. |
| `POST` | `/api/networks` | Create a virtual network. Body `{ "name": "..." }`. |
| `PATCH` | `/api/networks/:nid` | Rename. Body `{ "name": "..." }`. |
| `DELETE` | `/api/networks/:nid` | Cascade-delete network and all its device rows (also drops the saved layout file). |
| `GET` | `/api/networks/:nid/devices` | Combined view of admitted + pending devices. |
| `PATCH` | `/api/networks/:nid/devices/:cid` | In-place edit: `name`, `tap_ip`, `lan_subnets`, **`admitted`** (true admits, false kicks back to pending). All fields optional. |
| `POST` | `/api/networks/:nid/routes/push` | Re-derive routes from every member's `lan_subnets`, push `SetRoutes` to all joined peers. |
| `GET` | `/api/networks/:nid/layout` | Saved graph node positions: `{ positions: { "<id>": { x, y } } }`. Returns empty object on a fresh network (200 OK), 404 for unknown network. |
| `PUT` | `/api/networks/:nid/layout` | Atomic full-replace of saved positions; `<save_dir>/layouts/<nid>.json`. |
| `GET` | `/ws` | WebSocket; pushes `metrics.tick` (1 Hz throughput sample), `device.{online,offline,changed,pending,removed}`, `routes.changed`, `network.{created,changed,deleted}`. Server keeps it warm with 25 s pings. |

Errors all share the envelope `{ "error": "<message>" }`. 4xx for
caller-fixable mistakes (unknown network, malformed CIDR, IP
collision); 5xx is reserved for hub failure.

### Production: single binary

The `bifrost-server` binary embeds `web/dist/` at compile time via
[`rust-embed`](https://crates.io/crates/rust-embed). Build the
frontend first, then build the server:

```bash
cd web && npm install && npm run build && cd ..
cargo build --release -p bifrost-server          # or scripts/build-cross.sh
```

`scripts/build-cross.sh` does both steps for cross-compilation —
pass `--skip-web` to reuse the existing `web/dist/`. After that
`bifrost-server` serves the SPA on the same port as the API:

* `GET /` → embedded `index.html`, `Cache-Control: no-cache`.
* `GET /assets/*` → hashed JS/CSS, `Cache-Control: public, max-age=31536000, immutable`.
* `GET /networks/:nid` → SPA fallback (still `index.html` + 200) so
  React Router can handle deep links.
* `GET /api/*` and `GET /ws` win against the static fallback.

If the build is fresh (no `web/dist/` yet) `build.rs` writes a
placeholder `index.html` — `cargo build` succeeds, but the WebUI just
shows a "WebUI not built" page until you run `npm run build`.

### Frontend dev workflow

For iterating on the React side without rebuilding the Rust binary
on every change, run Vite's dev server in front of the daemon:

```bash
cd web
npm install
npm run dev        # http://127.0.0.1:5173, proxies /api and /ws to backend
```

Set `BIFROST_BACKEND=http://<host>:<port>` if the server is somewhere
other than `127.0.0.1:8080`. HMR works as expected.

Pages currently shipping:

* `/networks` — table of virtual networks; **create** with the inline
  "+ New network" button, **rename** by clicking the name (inline
  edit), **delete** with the per-row Delete button (confirm dialog).
  Counts and edits stream in via `network.*` and `device.*` WS events,
  with a 30 s safety-net poll.
* `/networks/:nid` — devices in a network, with two interchangeable
  views toggled per-tab and persisted to `localStorage`:
  * **Table view** — admit toggle (a single switch per row replaces
    the old approve/deny/kick buttons), inline edit on name / TAP IP
    / LAN subnets, throughput cell with numeric bps + sparkline.
  * **Graph view** — React Flow canvas filling the viewport. Hub
    card centers; device cards orbit on a deterministic ring on
    first load. **Node positions persist server-side** (PUT to
    `/api/networks/:nid/layout` debounced 300 ms after each drag),
    so a fresh browser on a different machine sees the same
    arrangement. **Floating edges** snap to the closest pair of
    side-midpoints between any two nodes — drag a device around
    the hub and the line follows. **Saving / Saved chip** in the
    canvas's top-right corner gives explicit feedback on each
    drag-release. Hub card's network name is editable inline (same
    PATCH endpoint the index page uses, both views stay in sync via
    `network.changed` WS events).
* `Push routes` button in the toolbar recomputes the derived table
  and pushes `SetRoutes` to every joined peer. After any
  `lan_subnets` edit, the button switches to amber + soft pulse +
  trailing dot to remind the user the change isn't live until pushed
  — plus an info toast at the moment of the edit. Cleared on a
  successful push.
* Header status badge — live / connecting / offline, driven by the
  WebSocket connection.
* Live throughput on every device — both the numeric `B/s` / `KB/s`
  / `MB/s` value and a 60-sample sparkline, fed by 1 Hz
  `metrics.tick` events.

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
│  │  └─ src/routes.rs              #   derive routes from per-device lan_subnets
│  ├─ bifrost-web/                 # axum HTTP / WS server consumed by web/
│  ├─ bifrost-server/              # daemon binary + driver lib
│  └─ bifrost-client/              # daemon binary + driver lib
│
├─ web/                            # React + Vite + TS + Tailwind frontend
│  ├─ src/views/                   #   NetworkList, NetworkDetail,
│  │                                 #   DevicesAsTable, DevicesAsGraph
│  ├─ src/components/
│  │  ├─ Layout, InlineEdit, Sparkline, ThroughputCell, Toaster
│  │  ├─ graph/                    #   ServerNode, DeviceNode,
│  │  │                              #   FloatingEdge, graphLayout
│  │  └─ ui/                       #   Button, Card, Badge, Switch, Table
│  └─ src/lib/                     #   api / ws / types / metrics /
│                                    #   eventInvalidator / toast / format / cn
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
                     bifrost-server ──→ bifrost-web
                          │                  │
       bifrost-client ────┤                  │
                          ▼                  ▼
                     bifrost-core ←──────────┘
                      │      │
                      ▼      ▼
                bifrost-proto  bifrost-net
                                │
                                └─ #[cfg(target_os = "linux")]
                                     linux::{LinuxTap, LinuxBridge, LinuxPlatform}
```

---

## Status

### Completed (P0 — core)

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

### Completed (Phase 1.0–1.6 — WebUI v1, Phase 2.0 — multi-tenant L2)

* **Per-device `lan_subnets`** replaces the global `[[routes]]`
  table; the route table is derived per-network at push time. CLI
  migrated to `device list/set/push`.
* **Single admit toggle** replaces approve / deny / kick.
  `device set --admit true|false` (CLI) or `PATCH … {admitted: …}`
  (HTTP) is the one knob; a kicked device stays as a pending row,
  a fresh `Join` lands back in pending state — no protocol changes.
* **Network CRUD on the WebUI** — create, rename, delete networks
  from the index page and the graph view's hub card. Backed by
  `POST/PATCH/DELETE /api/networks[/:nid]` and three new event
  types (`network.created/changed/deleted`).
* **`bifrost-web` crate** — axum HTTP/WS on `127.0.0.1:8080` by
  default. Full CRUD over networks and devices, `/api/.../layout`
  for graph node positions, `/api/.../routes/push` for route push.
  Errors share `{"error": "..."}`.
* **`web/` SPA** — React + Vite + TypeScript + Tailwind. Two
  interchangeable views toggled per-tab; inline edits with
  optimistic updates + rollback + toast on validation failure;
  per-device admit switch; "Push routes" button that pulses amber
  while LAN-subnet changes are unpushed.
* **Graph view (React Flow)** with editable hub-name card, **floating
  edges** that snap to the closest side midpoints between any two
  nodes, **server-side persisted node positions** with
  saving/saved/error indicator chip, fitView on first load, and a
  minimap.
* **Per-session throughput counters** (`AtomicU64` in `SessionTask`)
  drive a 1 Hz sampler that broadcasts `metrics.tick` events.
  Devices show both the human-readable bps and a 60-sample
  sparkline. WS events drive TanStack-Query invalidation so the UI
  doesn't poll; the safety-net refetch is at 30 s.
* **Single-binary deploy** — `rust-embed` bakes `web/dist/` into
  `bifrost-server`. Same port serves the SPA, API, and WS; deep
  links fall back to `index.html`; hashed assets get
  `Cache-Control: immutable`. `<save_dir>/layouts/<nid>.json` holds
  per-network UI state.
* **Per-network Linux bridges (Phase 2.0).** Each virtual network
  owns its own kernel bridge — `mknet` creates one, `delete_net`
  tears it down, an admitted device's TAP attaches to its
  network's bridge only. Networks share no broadcast domain, ARP,
  MAC table, or route table. `NetRecord` carries `bridge_name`
  (auto-derived `bf-<8-hex>` from UUID) and optional `bridge_ip`
  (host-side gateway address). One-shot migration on upgrade
  hands the legacy `[bridge]` config to the first network.
* **168 tests passing**, clippy-clean with `-D warnings`.

### Roadmap

| Phase | Item | Notes |
|---|---|---|
| 2.x | `--ip <cidr>` flag on `mknet` CLI + `POST /api/networks` body, plus a per-network bridge editor in the WebUI | Phase 2.0 ships kernel-level isolation but new networks default to `bridge_ip = ""` — the operator can hand-edit `server.toml` to give a network a host-side address until the surface ships. |
| —   | **Noise XX transport** | `Hello.caps` bit already reserved; add a `snow`-backed `Transport` impl, no business-logic changes |
| —   | **Prometheus metrics** | `metrics-exporter-prometheus`; per-session bytes / frames / drops (`[metrics]` config exists but is currently a no-op) |
| —   | **Per-session pcap dump** | `SessionCmd::PcapStart/Stop` already defined in core |
| —   | **macOS / Windows clients** | `bifrost-net::macos::utun` (IP-only), `bifrost-net::windows::wintun` (full L2) |

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
