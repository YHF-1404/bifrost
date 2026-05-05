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
+ WebSocket, default `127.0.0.1:8080`) with a single unified page
covering everything: a **drag-and-drop unified Networks + Devices
view** (left pane = pending clients waiting for an assignment, right
pane = a card per virtual network — drag a client between them to
move it), with a **Table** mode and a **Graph** mode (one canvas,
networks as bordered group frames). Each network's bridge IP is
edited with a segment-locked picker (only `/16` or `/24`); each
client's TAP IP is locked to the bridge's prefix so collisions are
caught at type-time. The frontend is a small React app under `web/`;
the backend is the same daemon binary. See [WebUI](#webui).

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
* **Server-authoritative assignment (Phase 3).** A client can be in
  at most one network at a time; the *server* decides which one
  (admin-driven, via drag-to-assign in the WebUI or `assign` over
  CLI). A new connecting client lands in the pending pool, is
  rejected from `Join` until assigned, and gets a server-pushed
  `Frame::AssignNet { net_uuid }` whenever the assignment changes —
  the client tears down its TAP and re-Joins the new network.
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

# 4. As soon as the client connects (it does on its own), the server
#    creates a pending_clients row. Phase 3 — the server now decides
#    which network a client belongs to. Assign it:
CLIENT_UUID=$(ssh root@<client-ip> \
  'grep ^uuid /etc/bifrost/client.toml | cut -d\" -f2')
ssh root@<server-ip> "bifrost-server admin assign $CLIENT_UUID <NET_UUID>"
# (the client receives `Frame::AssignNet`, tears down any old TAP,
#  and Joins the new net automatically — but admitted=false at first)

# 5. Configure and admit. Each flag is independent — pass only the
#    ones you want to change.
ssh root@<server-ip> "bifrost-server admin device set $CLIENT_UUID \
  --admit true --name router --ip 10.0.0.2/24 --lan 192.168.200.0/24"

# 6. (later, optional) Kick a device back to pending without removing
#    its row, e.g. for maintenance:
#    ssh root@<server-ip> "bifrost-server admin device set $CLIENT_UUID --admit false"
#    Or fully detach to the pending pool:
#    ssh root@<server-ip> "bifrost-server admin assign $CLIENT_UUID none"

# 7. Push the derived route table to all members
ssh root@<server-ip> "bifrost-server admin device push <NET_UUID>"

# 8. Verify
ssh root@<server-ip> 'ping -c3 10.0.0.2'
```

Optional — open `http://127.0.0.1:8080` (over an SSH port-forward) to
get the same picture in a browser, then **drag the pending client
card from the left pane onto the network card** instead of running
steps 4 and 5 by hand. See [WebUI](#webui).

Subsequent reconnects auto-reuse the persisted state — both
`approved_clients` and `joined_network` are written to TOML at the
appropriate moments.

---

## Build

### Local development (macOS / Linux)

```bash
cargo build  --workspace
cargo test   --workspace                                 # ~187 tests
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

### Optional: kernel tuning for higher throughput

Bifrost's bulk-throughput ceiling on a host with a **single-queue NIC**
(USB Ethernet adapters, most ARM SBCs, lots of embedded boards) is set
by `NET_RX` softirq pinning to one core. To distribute that work in
software run once per host, after each boot:

```bash
sudo scripts/tune-host.sh             # auto-detect default-route NIC
sudo scripts/tune-host.sh end0        # or specify the NIC
```

The script enables RPS / RFS / XPS on the chosen NIC. On the
project's LAN testbed (Cortex-A55 4-core, single-queue gigabit) it
lifted single-stream upload from 361 Mbps → 451 Mbps (~+25 %). The
settings are runtime only — re-run after each reboot, or wire it
into `/etc/rc.local` / a systemd-tmpfiles drop-in / a udev rule.

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
#   bridge_ip    = "10.0.0.1/24"       # only `/16` and `/24` accepted
#                                      # (Phase 3 — empty = pure-L2)
#
# Each [[approved_clients]] row carries:
#   client_uuid  = "..."               # set by the daemon
#   net_uuid     = "..."               # set by the daemon (one row per
#                                      #   client_uuid as of Phase 3)
#   tap_ip       = "10.0.0.2/24"       # `device set --ip`
#   display_name = "router"            # `device set --name`
#   lan_subnets  = ["192.168.200.0/24"]  # `device set --lan`
#   admitted     = true                # `device set --admit`
#
# Phase 3 — clients that have connected but aren't yet in any
# network live in a sibling section:
# [[pending_clients]]
#   client_uuid  = "..."
#   display_name = ""                  # editable while pending
#   lan_subnets  = []                  # editable while pending
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
| `mknet <name> [--ip <cidr>]` | Create a virtual network; returns its UUID. `--ip` is an optional host-side bridge gateway in CIDR form (e.g. `--ip 10.0.0.1/24`). Only `/16` or `/24` prefixes are accepted (matches the WebUI segment-locked picker). Without `--ip` the bridge is created without a host-side address and admins can set it later via the WebUI / `PATCH /api/networks/:nid`. |
| `rename <net-uuid> <new-name>` | Rename an existing network. |
| `rmnet <net-uuid>` | **Phase 3** — delete a network. Clients **detach to the pending pool** (preserving display_name + lan_subnets) instead of being removed; the kernel bridge is destroyed and routes re-synced. |
| `assign <client-uuid> <net-uuid|none>` | **Phase 3** — assign a client to a network or `none` to detach. Sends `Frame::AssignNet` to the live conn so the client tears down its TAP and re-joins the new target. After this, run `device set --ip ... --admit true` to bring it online. |
| `device list [<net-uuid>]` | List devices (admitted + pending). With no `<net-uuid>` includes the pending pool too (rows with `net=-`). |
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
>
> **Phase 3** added one new verb (`assign`) and changed `rmnet`'s
> semantics — clients detach to the pending pool instead of being
> deleted. The wire-protocol version bumped from 1 to 2 (added
> `Frame::AssignNet`), so old clients are rejected on Hello with a
> `version_mismatch` `JoinDeny` until they're upgraded too.

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
| `GET` | `/api/networks` | Network list with `device_count` / `online_count` and per-network `bridge_name` / `bridge_ip`. |
| `POST` | `/api/networks` | Create a virtual network. Body `{ "name": "..." }`. |
| `PATCH` | `/api/networks/:nid` | Edit `name` and/or `bridge_ip`. **Phase 3:** `bridge_ip` is constrained to `/16` or `/24` — non-empty values with other prefixes return 400. Changing prefix length auto-rewrites every member's `tap_ip` (octets preserved) and pushes `SetIp` to live sessions. |
| `DELETE` | `/api/networks/:nid` | **Phase 3** — destroys the network's kernel bridge and detaches its clients to the pending pool (carrying `display_name` + `lan_subnets`); does NOT delete device rows. |
| `GET` | `/api/networks/:nid/devices` | Combined view of admitted + pending-admit devices for one network. |
| `PATCH` | `/api/networks/:nid/devices/:cid` | In-place edit: `name`, `tap_ip`, `lan_subnets`, **`admitted`** (true admits, false kicks back to pending). All fields optional. |
| `POST` | `/api/networks/:nid/routes/push` | Re-derive routes from every member's `lan_subnets`, push `SetRoutes` to all joined peers. |
| `GET` | `/api/clients` | **Phase 3** — list every known client in one shot, both network-assigned and pending-unassigned. The unified WebUI uses this. |
| `PATCH` | `/api/clients/:cid` | **Phase 3** — edit a client's `name` and/or `lan_subnets` regardless of whether it's pending or admitted. |
| `POST` | `/api/clients/:cid/assign` | **Phase 3** — drag-to-assign. Body `{ "net_uuid": <uuid> | null }`; `null` detaches to the pending pool. Same-net is a no-op. Sends `Frame::AssignNet` to the conn; `admitted` and `tap_ip` are reset on every cross-network move. |
| `GET` | `/api/ui-layout` | **Phase 3** — single-file unified layout: `{ table: { left_ratio, left_collapsed }, graph: { positions, frames } }`. Replaces the old per-network `layout` files. Returns the current persisted state, or an empty default. |
| `PUT` | `/api/ui-layout` | **Phase 3** — atomic full-replace of the unified layout. The frontend ships a debounced PUT after each interaction. |
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

**Phase 3** drops the old Networks → Devices hierarchy in favor of a
single unified page (`/`):

* **Table mode** — left/right split with a draggable divider built
  on `react-resizable-panels`. The left pane lists pending
  (unassigned) clients; the right pane is one card per virtual
  network. DnD via `@dnd-kit/core`: **drag a client between
  panes/cards** to (re-)assign — the server sends `Frame::AssignNet`
  to the live conn and the client tears down its TAP and re-joins
  the new target. Optimistic cache update lands the card in its new
  home in the same frame the user releases (no fly-back animation —
  `<DragOverlay dropAnimation={null}>`). Every cross-network move
  clears `admitted` and `tap_ip` per spec, so the user always
  re-confirms. Pending cards expose only `name` and `lan_subnets`
  (no IP, no throughput); admitted rows get the full set plus a
  per-row throughput cell with a 60-sample sparkline. The
  bottom-left FAB toggles the left pane's `collapsible` state via
  the panel's `ImperativePanelHandle`, so the dragged size survives
  collapse/expand round-trips; both width ratio and collapse flag
  persist via `/api/ui-layout`.
* **Graph mode** — React Flow canvas with one solid-bordered group
  frame per network containing its Hub card and admitted clients.
  Pending clients float free outside any frame. Both Hub and Client
  cards are richly editable (admit Switch, name, segmented TAP IP,
  LAN subnet chips, throughput) — only the wide top header strip is
  the drag handle, so editing inputs doesn't grab the node.
  Interactions:
  - **Drag a client into a frame** ⇒ assign; **drag out of every
    frame** ⇒ detach to pending. The card stays exactly where the
    user dropped it (drop position is computed in the destination
    frame's coordinate system before the assign mutation fires).
  - **Right-click a Hub card** ⇒ "Delete network" (clients fall
    back to the pending pool).
  - **Right-click the canvas blank** ⇒ "Create new network"
    (the new frame lands at the cursor via `screenToFlowPosition`).
  - Frames **auto-grow on all four sides** to encompass their
    children — Hub plus admitted clients — and **don't overlap**:
    if growing one frame would intersect another, an iterative
    bbox-collision resolver pushes the unpinned (default-positioned)
    one apart along the smaller-overlap axis.
  - Edges use a **`FloatingEdge`** custom renderer that, on every
    drag tick, picks the closest pair of side midpoints between the
    client card and the hub card. The card's inner div uses
    `h-full w-full flex-col` so the wrapper bbox coincides with the
    visible card border — endpoints land on the border, not in the
    padding.
  - Frame x/y/w/h, hub/client positions all persist server-side
    via `/api/ui-layout`.
* **IP-segment pickers** — bridge IPs use a four-octet input with a
  click-to-toggle `/16 ↔ /24` button (replaces a native `<select>`,
  which used to lose focus on dropdown open) plus an explicit `ok`
  commit button. Client TAP IPs lock the octets pinned by the
  bridge's prefix (e.g. bridge `10.0.0.1/24` ⇒ client picker shows
  `10.0.0.[__]/24`); inline collision detection rejects both
  duplicates with other clients **and** equality with the bridge IP
  itself before submit.
* **`Push routes` per card** — the per-network card's button
  recomputes routes and sends `SetRoutes` to its peers. After any
  `lan_subnets` edit, an info toast pops AND the button switches to
  amber + soft pulse + trailing `•` so the user knows to push;
  cleared on success.
* **Layout save chip** — in the toolbar; flips between
  *saved* / *unsaved* / *saving* as the debounced PUT round-trips.
* **WS status badge** — `live` / `connecting` / `offline`, driven
  by the WebSocket connection state.
* Live throughput on every admitted device — fed by 1 Hz
  `metrics.tick` events.

---

## Wire protocol (v2)

Frame format:

```
┌────────────────────────┬──────────────────────────────────┐
│  u32 BE  payload_len   │  postcard-encoded `Frame`        │
└────────────────────────┴──────────────────────────────────┘
```

`Frame` is a 13-variant enum. A summary:

| Variant | Direction | Purpose |
|---|---|---|
| `Hello` / `HelloAck` | C↔S | First-frame handshake; carries `version` and `caps` bits. |
| `Join` / `JoinOk` / `JoinDeny` | C→S / S→C | Network membership negotiation. |
| `Eth(Vec<u8>)` | bidirectional | Data plane: a single Ethernet frame. |
| `AssignNet { net_uuid }` | S→C | **v2** — server-driven re-assignment. `Some(uuid)` switches the client to that network; `None` detaches to idle. The client tears down any existing TAP and (if `Some`) issues a fresh `Join`. |
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
│  ├─ src/views/                   #   UnifiedView (table) + UnifiedGraphView
│  ├─ src/components/
│  │  ├─ Layout, InlineEdit, Sparkline, ThroughputCell, Toaster
│  │  ├─ IpSegmentInput              #   /16 vs /24 octet-locked picker
│  │  ├─ SaveStatusChip              #   layout: saved / unsaved / saving
│  │  └─ ui/                       #   Button, Card, Badge, Switch, Table
│  └─ src/lib/                     #   api / ws / types / metrics /
│                                    #   useUiLayout / eventInvalidator /
│                                    #   toast / format / cn
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

### Completed (Phase 1.0–1.6 — WebUI v1, Phase 2.0 — multi-tenant L2, Phase 3.0 — unified WebUI + server-driven assignment)

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
* **Server-authoritative assignment + protocol v2 (Phase 3.0).**
  A new `pending_clients` table tracks connected-but-unassigned
  clients persistently; new `Frame::AssignNet { net_uuid }` lets
  the server move a client between networks (the client tears down
  its TAP and re-joins the new target). One client = at most one
  network at a time, enforced by Hub state-machine + a Phase-2 →
  Phase-3 config migration. New endpoints: `GET /api/clients`,
  `PATCH /api/clients/:cid`, `POST /api/clients/:cid/assign`.
  `PATCH /api/networks/:nid` extended with `bridge_ip` (limited to
  `/16` or `/24`; prefix changes auto-rewrite every member's
  `tap_ip`). `delete_net` no longer deletes devices — they detach
  to the pending pool. New CLI verb: `assign <client> <net|none>`.
* **Unified WebUI (Phase 3.0).** Single page replaces the old
  Networks index + per-network detail pages. Drag-and-drop
  (`@dnd-kit/core`) in Table mode + a single React Flow canvas in
  Graph mode (with bordered group frames per network and right-
  click context menus). Segment-locked IP pickers; single-file
  `ui-layout.json` replaces the per-network layout files (with a
  one-shot migration that folds old files in).
* **187 tests passing**, clippy-clean with `-D warnings`.

### Completed (Phase 3.1 — data-plane perf + WebUI polish)

* **Bulk-throughput rebuild.** A combination of small fixes lifted
  single-stream upload through a real production xray tunnel from the
  user's original 26 KB/s to 4.1 MB/s (~150×) and on a gigabit LAN
  testbed from 222 Mbps to 361 Mbps. Headline changes:
  * `TCP_NODELAY` on every data-plane socket (server `accept`, client
    `connect`, both directions of the SOCKS5-wrapped path) so per-frame
    writes don't sit in Nagle's queue waiting for the previous segment
    to ACK.
  * Bounded `SO_SNDBUF` (256 KB) on the same sockets so the auto-tuned
    multi-MB buffer can't hide downstream tunnel congestion from the
    inner TCP riding on bifrost. `bifrost-net::set_send_buffer_size`
    is the helper.
  * TAP and per-network bridge MTU set to **1400** so a 1500-byte
    inner Ethernet frame plus our 4-byte length prefix + postcard
    tags + outer TCP/IP header still fits under the underlying 1500
    physical MTU — without this, every full-size frame fragmented or
    dropped through nested TCP tunnels.
  * `framed.send` per frame on both ends. An earlier "feed all queued
    frames + one flush" optimization was a real regression here: it
    accumulated megabyte-sized writes that overran a slow downstream
    proxy (xray-core), drove its RWND to 0, and reset the outer cwnd.
  * Wider data-plane mpsc channels (`128 → 1024`) so brief socket
    stalls don't immediately backpressure the TAP-read loop.
* **Zero-alloc Frame encoding.** `FrameCodec::encode` was 10 % of all
  CPU cycles on the upload hot path. Two fixes inside `bifrost-proto`:
  a custom `BytesMutFlavor` that lets postcard serialize straight into
  the destination `BytesMut` (no `to_allocvec` round-trip); and
  `#[serde(with = "serde_bytes")]` on `Frame::Eth(Vec<u8>)` and
  `Frame::File::data` so postcard treats the payload as bytes
  (one `try_extend(slice)` call) instead of as a sequence of `u8`
  (1500 individual `try_push` calls per Ethernet frame). Wire format
  unchanged. After both fixes `FrameCodec::encode` falls out of the
  top 25 hotspots entirely.
* **Live `bridge_ip` updates** via `PATCH /api/networks/:nid` now
  push the new address through netlink onto the kernel bridge
  (`Bridge::set_ip` on the trait + a netlink `flush_addrs` /
  `add_addr` impl in `LinuxBridge`). Previously the WebUI/API edit
  only updated the on-disk config and required a server restart for
  the kernel to see the change.
* **Table view** — per-row card style (rounded border, soft shadow,
  hover state) so adjacent rows have a visible boundary; a fixed grid
  template shared with the column-header strip so `name` / `tap IP` /
  `LAN subnets` / `throughput` / `uuid` line up perfectly across
  rows; `dnd-kit` collision detection switched to `pointerWithin` so
  drops on the narrow PENDING pane are decided by cursor position
  alone (the wider Networks pane no longer "wins" near the top of
  the pending pane).
* **Graph view** — per-client card height now scales with LAN-subnet
  count (`base + (lan_rows - 1) * 22 px`) so a client with 5 subnets
  no longer pushes its throughput chart out of the React Flow
  wrapper; the IP-segment editor's input width grew (`w-9 → w-12`)
  so 3-digit octets fit; the throughput value column got `w-20` +
  `whitespace-nowrap` so `99.9 GB/s` stays on one line.

### Completed (Phase 3.2 — bulk throughput in a controlled LAN testbed)

Single-stream measurements between an aarch64 Cortex-A55 client and
an x86_64 server over gigabit LAN (direct LAN baseline: 940 Mbps):

| Path                                | upload   | download |
|-------------------------------------|----------|----------|
| bifrost direct                       | 497 Mbps | 446 Mbps |
| bifrost + xray-core (VLESS Reality) | 316 Mbps | 335 Mbps |

Three layered changes on top of the Phase 3.1 baseline:

* **`scripts/tune-host.sh`** — RPS / RFS / XPS configuration helper
  for hosts with a single-queue NIC (USB-Ethernet adapters and most
  embedded ARM SBCs). Without it `NET_RX` softirq pins to whichever
  CPU first handled the IRQ; the script distributes that work in
  software across all cores. Lifted single-stream upload 361 Mbps
  → 451 Mbps in the testbed.

* **Bounded batched send (32 frames or 32 KB, whichever first).**
  Replaces per-frame `framed.send` on both ends. With per-frame
  writes the kernel TCP layer can't TSO-coalesce — every 1.4 KB
  Ethernet frame became its own MSS-sized segment; an intermediate
  proxy (xray-core) also paid full per-write VLESS-framing-and-crypto
  cost for each tiny chunk. The bounded batch produces one ~30 KB
  userspace write per flush — small enough to stay well under any
  reasonable receive buffer (xray's autotuned ~256 KB), big enough
  that the NIC's TSO splits it into ~22 wire packets per syscall and
  xray sees one read instead of 22. Re-bounds the scheme that an
  earlier "drain whole channel into one flush" version got wrong:
  unbounded batching had crashed RWND to 0 on a long-RTT VPS xray
  tunnel; the 32 KB cap keeps that safe.

* **Live `bridge_ip` updates** (carry-over from 3.1's bug fix).
  `Bridge::set_ip` on the trait + matching netlink push from
  `Hub::handle_set_net_bridge_ip` so a WebUI/API edit takes effect
  without a server restart.

Diagnostic notes worth keeping (full perf trace in commit log):

* The remaining gap to LAN line rate (940 → 497 Mbps for bifrost
  alone, ~50 % loss) is fundamental for a user-space VPN: 6 stack
  traversals per inner Ethernet frame vs the 1 that direct iperf3
  pays, and per-frame outer TCP writes that can't be amortized into
  TSO-size SKBs without breaking proxy receive buffers.

* Going past ~500 Mbps on this hardware needs an architectural
  change — multi-connection per client, GSO-style super-frames, or
  io_uring batched I/O. Each comes with non-trivial protocol-side
  changes; deferred.

### Completed (Phase 3.x — small follow-ups)

* **Server-driven `routes.dirty` signal.** The hub now tracks the
  per-network "needs push?" state explicitly: a `last_pushed_routes`
  in-memory snapshot is updated by `device_push` and compared to the
  current derived route table after every config-mutating
  handler (admit, kick, edit `lan_subnets`, assign across networks,
  delete). On every transition it emits `HubEvent::RoutesDirty
  { network, dirty }` and `HubSnapshot::routes_dirty` carries the
  current set so a freshly-loaded WebUI tab paints the right pulse
  state without polling. The `Network` API row gains a
  `routes_dirty: bool` field; the WebUI's Table and Graph views
  drive the amber pulse from it (with a small optimistic-overlay
  set so a save+pulse round-trip feels immediate). Closes the
  long-standing case where admitting a brand-new client with
  `lan_subnets` left existing peers in the network silently
  unaware until someone hand-clicked "push routes".

* **`mknet --ip <cidr>` CLI flag.** `bifrost-server admin mknet
  <name> --ip 10.0.0.1/24` now creates the network *and* sets the
  host-side bridge IP in one step, validated to `/16` or `/24` to
  match the WebUI segment-locked picker. The kernel bridge gets the
  address via netlink at creation; no follow-up `PATCH` (and no
  `server.toml` hand-edit) required. Same `ip=<cidr>` syntax in
  the in-process REPL. The admin protocol's `MakeNet` request and
  the `NetEntry` snapshot row now both carry `bridge_ip`; an
  invalid CIDR fails fast and leaves no half-created network.

* **Phase-3 stale-config join race fixed.** Previously a client
  whose toml carried a `joined_network` from a previous server
  would auto-`Join` that stale UUID right after `HelloAck`,
  racing the server's `AssignNet` and producing
  `JoinDeny: unknown_network` followed by
  `WARN JoinOk without prior Join — ignoring`, leaving the
  session permanently stuck. The client no longer auto-joins
  from cache — the server's `AssignNet` is the single source of
  truth, with REPL/admin `join <net>` traffic carried separately
  in a `pending_user_join` slot.

### Roadmap

| Phase | Item | Notes |
|---|---|---|
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
