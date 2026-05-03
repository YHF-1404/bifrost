# Bifrost

[English](README.md) · **中文**

> 基于 TCP + 自定义二进制协议封装以太网帧的虚拟局域网工具
>
> Rust 实现 · Linux only · 与 Xray-core / V2Ray / SOCKS5 隧道天然契合

把若干分散在公网、NAT 后的 Linux 主机虚拟地拉进同一个 L2 广播域。
Server 在 Linux 上建网桥；client 在本地建 TAP；以太网帧用一个紧凑
postcard 帧格式封装后通过 TCP（可选 SOCKS5）双向转发。

---

## Features

- **L2 over TCP** — 透传以太网帧，ARP / DHCP / 任意 IP 协议都在虚拟 LAN 内可用
- **SOCKS5 出口** — 客户端可走代理穿透到服务器，便于在受限网络部署
- **协议带版本号** — `Hello/HelloAck` 强制版本协商，未来加密升级（Noise / TLS）已经预留 `caps` bit
- **断线复用** — Session 跨重连保 TAP，可配置 disconnect timeout 之后才回收
- **三套控制面同源** — daemon 默认起 admin Unix socket + 本机 HTTP/WS（默认 `127.0.0.1:8080`），可选前台 REPL；三者背后是同一个 `HubHandle`
- **WebUI（v0.1 只读）** — `web/` 下的 React + Vite + Tailwind 应用；浏览网络 / 设备 / 在线状态。后续阶段加流量图表、就地编辑、节点拓扑视图
- **路由表自动派生** — 每台设备声明背后的 `lan_subnets`，server 端 `device push` 时聚合下发，不再手维护 `[[routes]]`
- **零外部 `ip` 命令依赖** — Linux 后端走 rtnetlink + ioctl 直连内核
- **跨编译开箱即用** — `cross.toml` + Docker 一行编译 x86_64 / aarch64

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
│                                     │         │  ├─ admin  /run/bifrost/server.sock │
│                                     │         │  └─ WebUI HTTP / WS  (127.0.0.1:8080)
└────────────────┬────────────────────┘         └──────────────────┬──────────────────┘
                 │                                                  │
                 │     postcard-framed wire protocol over TCP       │
                 │     (optionally tunnelled through SOCKS5)        │
                 └──────────────────────────────────────────────────┘
```

**关键设计**：

- **Hub 单 actor**：所有控制态（networks / approved_clients / sessions / pending / conns）由一个 `tokio::select!` 任务独占，外部只能通过 `mpsc<HubCmd>` 发命令；不再需要锁。
- **数据面 0 hop**：批准 join 时，Hub 把 `session_cmd_tx` 通过 `bind_tx` 推给 ConnTask，此后 ETH 帧 `socket → ConnTask → SessionTask → TAP` 直连，**不经 Hub**。
- **Session 状态机**：`Joined → Disconnected → Dead`，server 端有 disconnect timeout，client 端用 `None` 表示"永不超时由用户控制"。
- **路由派生而非配置**：每台 admitted client 声明 `lan_subnets`，`device push` 时由 server 聚合得出路由表，去掉了 `[[routes]]` 这个独立维度。
- **两个控制面同源**：admin Unix socket（`bifrost-server admin <cmd>`）和本地 HTTP API（`/api/...`）背后都是 `HubHandle` 的方法；WebUI 只是另一个 consumer。

---

## Build

### 本地开发（macOS / Linux）

```bash
cargo build --workspace
cargo test  --workspace      # 134 个单测 / 集成测试
cargo clippy --workspace --all-targets -- -D warnings

# 前端（只在动 WebUI 时需要）
cd web && npm install && npm run build
```

macOS 上的二进制可以跑 daemon、admin RPC、HTTP/WS API、所有协议层逻辑（用 `NullPlatform`），但 `create_tap` 会运行时报 `Unsupported`——TAP / bridge 仅 Linux 支持。

### 交叉编译生产二进制（Linux 目标）

需要 Docker + `cross`：

```bash
cargo install cross --git https://github.com/cross-rs/cross
./scripts/build-cross.sh
# 产物：
#   target/x86_64-unknown-linux-gnu/release/bifrost-server
#   target/aarch64-unknown-linux-gnu/release/bifrost-client
```

`Cross.toml` 锁定的镜像基于 Debian glibc，与 Arch / Ubuntu 24.04 等主流发行版兼容。

---

## Deployment

### 一次性：建立 root 免密 SSH

```bash
# 假设你已经能 ssh 普通用户 + 该用户能免密 sudo
ssh-copy-id linuxuser@<server-ip>
PUBKEY=$(cat ~/.ssh/id_ed25519.pub)
ssh linuxuser@<server-ip> "sudo bash -c 'mkdir -p /root/.ssh && \
  chmod 700 /root/.ssh && \
  echo \"$PUBKEY\" >> /root/.ssh/authorized_keys && \
  chmod 600 /root/.ssh/authorized_keys'"
```

### 部署 server / client

```bash
# 默认目标主机：
#   SERVER_HOST=root@64.176.40.25
#   CLIENT_HOST=root@192.168.200.1
# 通过环境变量覆盖。

SERVER_HOST=root@<server-ip>  ./scripts/deploy-server.sh
CLIENT_HOST=root@<router-ip>  ./scripts/deploy-client.sh
```

脚本会自动：

1. `systemctl stop` 旧 daemon（避免 ETXTBSY）
2. `scp` 二进制 → `/usr/local/bin/`
3. `scp` 示例 toml + systemd unit（**不会覆盖已有 `client.toml` / `server.toml`**）
4. `systemctl daemon-reload && systemctl enable --now`

### Bootstrap 第一次连接

```bash
# 服务端创建虚拟网络
ssh root@<server-ip> 'bifrost-server admin mknet hml-net'
# → network created  uuid=<NET_UUID>

# 客户端发起 join（会进入 pending 状态）
ssh root@<router-ip> 'bifrost-client admin join <NET_UUID>'

# 服务端审批
ssh root@<server-ip> 'bifrost-server admin list'           # 看 sid
ssh root@<server-ip> 'bifrost-server admin approve <SID>'

# 配置该设备：TAP IP、显示名、它背后的 LAN 网段
CLIENT_UUID=$(ssh root@<router-ip> \
  'grep ^uuid /etc/bifrost/client.toml | cut -d\" -f2')
ssh root@<server-ip> "bifrost-server admin device set $CLIENT_UUID \
  --name router --ip 10.0.0.2/24 --lan 192.168.200.0/24"

# 重新派生路由表并下发给所有成员
ssh root@<server-ip> "bifrost-server admin device push <NET_UUID>"

# 验证
ssh root@<server-ip> 'ping -c 3 10.0.0.2'
```

之后每次 client 重启都会用持久化的 `joined_network` 字段自动重连，server 端的 `approved_clients` 也已落盘 → 全部走自动批准路径。

可选：开 `http://127.0.0.1:8080`（通过 SSH `-L` 转发）拿到同一份信息的可视化视图。详见下文 [WebUI](#webui)。

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
ip = "10.0.0.1/24"           # 空字符串 = 不给 bridge 加地址
disconnect_timeout = 60       # 秒：socket 断开后 TAP 保留多久

[admin]
socket = "/run/bifrost/server.sock"

[web]                         # 默认只绑 lo，要远程访问请走 SSH -L 隧道
enabled = true
listen = "127.0.0.1:8080"

[metrics]                     # 暂未启用，预留
enabled = false
listen = "127.0.0.1:9090"

# 以下两段由 daemon 自动填充，无需手编
# [[networks]]          → mknet
# [[approved_clients]]  → approve / device set
#
# 每个 [[approved_clients]] 行的字段：
#   client_uuid    = "..."           # daemon 写入
#   net_uuid       = "..."           # daemon 写入
#   tap_ip         = "10.0.0.2/24"   # device set --ip
#   display_name   = "router"        # device set --name
#   lan_subnets    = ["192.168.200.0/24"]  # device set --lan
#
# 服务端的路由表是从 lan_subnets 派生出来的，不再有独立的 [[routes]] 段。
```

### `client.toml`

```toml
[client]
uuid = ""                     # 首次启动自动生成
host = "<server-ip>"
port = 8888
save_dir = "/var/lib/bifrost/received"
retry_interval = 5            # 秒：断线重试间隔
joined_network = ""           # daemon 自动写：当前 join 的网络 uuid

[proxy]
enabled = true
host = "127.0.0.1"
port = 10808                  # SOCKS5 端口

[tap]
ip = ""                       # daemon 自动写：服务端推送的 TAP IP

[admin]
socket = "/run/bifrost/client.sock"
```

---

## Admin CLI

两个 daemon 都是默认只起一个 admin Unix socket，REPL 不打开（systemd 友好）。
所有运维通过 `admin <subcommand>` 子命令一次性 RPC 完成。

### `bifrost-server admin`

| 命令 | 行为 |
|---|---|
| `mknet <name>` | 创建虚拟网络，返回 UUID |
| `approve <sid>` | 批准 pending join；建 TAP + 入桥 + 发 JoinOk |
| `deny <sid>` | 拒绝 pending join；发 JoinDeny |
| `device list [<net-uuid>]` | 列设备（已 admit 的 + 当前 pending 的），可按 net 过滤 |
| `device set <client-uuid> [--name X] [--ip Y/CIDR] [--admit BOOL] [--lan A,B,…]` | 改一台设备的字段。每个 flag 独立：缺省 = 不动；`--ip ""` 清空；`--lan ""` 清空 LAN 列表。在线设备会立即收到 `SET_IP` |
| `device push <net-uuid>` | 重新从所有成员的 `lan_subnets` 派生路由表，本机 bridge 上 apply 一遍，并下发 `SetRoutes` 给该网络内所有 joined 客户端 |
| `list` | networks / sessions / pending 全量 snapshot |
| `send <msg>` | 向所有连入客户端广播文本 |
| `sendfile <path>` | 把本地文件广播给所有客户端 |
| `shutdown` | 让 daemon 优雅退出 |

> 旧版本里的 `setip` 与 `route add/del/push` 已经全部移除。`device set --ip` 取代 `setip`；`route` 被 per-device `lan_subnets` + `device push` 取代。alpha 阶段，没有迁移路径——直接手工删除老 `server.toml` 里的 `[[routes]]` 段，然后 `device set --lan` 重建。

### `bifrost-client admin`

| 命令 | 行为 |
|---|---|
| `join <net-uuid>` | 申请加入网络（等待服务端批准） |
| `leave` | 离开当前网络（销毁本地 TAP） |
| `status` | 当前连接 / TAP / 配置状态 |
| `send <msg>` | 向服务器发送文本（服务器 stdout 回显） |
| `sendfile <path>` | 把本地文件发到服务器 `save_dir` |
| `shutdown` | 让 daemon 优雅退出 |

> `--socket <path>` 可以覆盖默认 socket（用于多实例 / 调试）；
> `--config <path>` 影响默认 socket 路径。

### 想要交互式 REPL？

```bash
bifrost-server --repl     # 不通过 systemd，直接前台跑，stdin 是 REPL
bifrost-client --repl
```

REPL 命令与 admin 子命令对齐。

---

## WebUI

server daemon 自带一个本机 HTTP + WebSocket 服务，前端在 `web/`，
用 React + Vite + Tailwind 写。默认只绑 **`127.0.0.1:8080`**——这是
鉴权模型本身。要从别的机器访问，请走 SSH 端口转发：

```bash
ssh -L 8080:127.0.0.1:8080 root@<server-ip>
# 浏览器打开 http://127.0.0.1:8080
```

`server.toml` 的 `[web]` 段可以改 listen 或彻底关掉；命令行也支持
`--web-listen <addr>` / `--no-web` 临时覆盖。

### Endpoints（v0.1 只读）

| Method | Path | 返回 |
|---|---|---|
| `GET` | `/api/networks` | 网络列表 + 每网的 `device_count` / `online_count` |
| `GET` | `/api/networks/:nid/devices` | admitted + pending 设备的合并视图，含在线状态、TAP 名等 |
| `GET` | `/ws` | WebSocket。v0.1 仅做 25 秒 keepalive；后续阶段会推 `metrics.tick` / `device.online` / `device.changed` |

写入端点（就地编辑、admit 开关、`routes/push` 按钮）下一阶段添加。

### 前端 dev 工作流

```bash
cd web
npm install
npm run dev        # http://127.0.0.1:5173；/api 与 /ws 自动 proxy 到 8080
```

如果 backend 不在 `127.0.0.1:8080`，设 `BIFROST_BACKEND=http://host:port`。

```bash
npm run build      # → web/dist/
```

当前 SPA 是单独 serve 的；`rust-embed` 把产物嵌进 `bifrost-server`
单二进制是 1.5 阶段的事。当前能用的页面：

- `/networks`：虚拟网络列表，5 秒轮询
- `/networks/:nid`：该网下所有设备，行内显示状态徽章（online / offline / pending）、名字、TAP IP、LAN 子网、短 UUID
- 顶栏的连接状态徽章：live / connecting / offline，由 WebSocket 状态驱动

---

## Project layout

```
bifrost/
├─ Cargo.toml / Cargo.lock         # workspace 总入口
├─ Cross.toml                      # cross 镜像配置
│
├─ crates/
│  ├─ bifrost-proto/               # 协议层：Frame / FrameCodec / admin RPC types
│  │                                #   纯 IO-free，可单元测试覆盖
│  │
│  ├─ bifrost-net/                 # 平台抽象：Tap / Bridge / Platform trait
│  │  ├─ src/null.rs                #   NullPlatform（链接占位，非 Linux 跑会报 Unsupported）
│  │  ├─ src/mock.rs                #   MockPlatform（feature = "mock"，给下游测试用）
│  │  └─ src/linux/                 #   #[cfg(target_os = "linux")] Linux 实现
│  │                                #     LinuxTap   = open + ioctl + AsyncFd
│  │                                #     LinuxBridge = rtnetlink
│  │
│  ├─ bifrost-core/                # 控制面：Hub + Session + Config + transport
│  │  └─ src/routes.rs              #   per-network 路由派生（lan_subnets → RouteEntry）
│  │
│  ├─ bifrost-web/                 # axum HTTP/WS 服务，给前端用
│  ├─ bifrost-server/              # 二进制 + lib：accept_loop / ConnTask / admin / repl
│  └─ bifrost-client/              # 二进制 + lib：ConnTask 重连 / App / admin / repl
│
├─ web/                            # React + Vite + TS + Tailwind 前端
│  ├─ src/views/                   #   NetworkList、DeviceTable
│  ├─ src/lib/                     #   api / ws / types / cn
│  └─ src/components/ui/           #   Button、Card、Badge、Table
│
├─ deploy/
│  ├─ server.toml.example
│  ├─ client.toml.example
│  └─ systemd/
│     ├─ bifrost-server.service
│     └─ bifrost-client.service
│
└─ scripts/
   ├─ build-cross.sh               # cross build 两端 release
   ├─ deploy-server.sh              # x86_64 → SERVER_HOST
   └─ deploy-client.sh              # aarch64 → CLIENT_HOST
```

### Crate 依赖图

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
                                     linux:: { LinuxTap, LinuxBridge, LinuxPlatform }
```

---

## Wire protocol (v1)

帧格式：

```
┌────────────────────────┬──────────────────────────────────┐
│  u32 BE  payload_len   │  postcard-encoded `Frame`        │
└────────────────────────┴──────────────────────────────────┘
```

`Frame` 是个 12-variant enum：

| Variant | 方向 | 时机 |
|---|---|---|
| `Hello / HelloAck` | C↔S | 第一帧握手；带 `version` 与 `caps` |
| `Join / JoinOk / JoinDeny` | C→S / S→C | 加入虚拟网络 |
| `Eth(Vec<u8>)` | 双向 | 数据面以太帧 |
| `SetIp { ip }` | S→C | 在线热更 TAP IP |
| `SetRoutes(Vec<RouteEntry>)` | S→C | 路由表下发（自动过滤 self-via） |
| `Text(String)` | 双向 | REPL `send` |
| `File { name, data }` | 双向 | REPL `sendfile` |
| `Ping(u64) / Pong(u64)` | 双向 | 心跳预留 |

Admin RPC 是另一份独立协议：

```
[u32 BE total_len][postcard ServerAdminReq | ClientAdminReq]
```

仅在 Unix socket 上传输，daemon 一次接受一个 `Req → Resp → close`。

---

## Status

### 已完成（P0 — core）

- ✅ `bifrost-proto` — Frame + Codec + admin RPC types
- ✅ `bifrost-net` — Tap / Bridge trait + mock + null + Linux 后端（rtnetlink）
- ✅ `bifrost-core` — Hub actor + Session 状态机 + config 持久化
- ✅ `bifrost-server` / `bifrost-client` — daemon + admin Unix socket + 可选 REPL
- ✅ 跨编译：x86_64-linux-gnu + aarch64-linux-gnu via `cross`
- ✅ systemd unit + 部署脚本，已在生产 Arch + Ubuntu aarch64 跑通端到端

### 已完成（Phase 1.0–1.1 — WebUI 基础）

- ✅ **per-device `lan_subnets`** 取代全局 `[[routes]]`；路由表按需派生。CLI 改为 `device list/set/push`。
- ✅ **`bifrost-web` crate** — axum HTTP/WS server，默认 `127.0.0.1:8080`，提供 `GET /api/networks` 与 `GET /api/networks/:nid/devices`，外加 `/ws` 心跳。
- ✅ **`web/` SPA** — React + Vite + TypeScript + Tailwind，NetworkList + DeviceTable，顶栏连接状态实时反映 WebSocket 状态。
- ✅ **134 个测试**通过，clippy `-D warnings` 干净。

### Roadmap（按阶段）

| 阶段 | 内容 | 备注 |
|---|---|---|
| 1.2 | per-session 流量计数 + sparkline | `SessionTask` 加 `AtomicU64` 字节计数；1 Hz `metrics.tick` 经 WS 下发；表格里画 mini 图 |
| 1.3 | 写入侧：`PATCH device`、admit 开关、就地编辑、`POST routes/push` | HTTP 端点 + 前端 mutation |
| 1.4 | 节点图视图（React Flow），与表格视图互通 | 自带就地编辑，乐观更新 |
| 1.5 | `web/dist/` 嵌进 `bifrost-server` 单二进制 | `rust-embed` + SPA fallback |
| 2.x | 多虚拟网：`HubManager`、per-net `HubHandle`、网络 CRUD | URL 已经是 `/api/networks/:nid/...`，主要工作是 actor 拆分 |
| —   | **Noise XX 加密 transport** | `Hello.caps` 已经预留 bit；上 `snow` crate 实现 `Transport` trait 即可，业务代码无需改动 |
| —   | **Prometheus metrics** | `metrics-exporter-prometheus`；per-session 字节 / 帧 / 丢包（1.2 帮它打地基） |
| —   | **per-session pcap dump** | `SessionCmd::PcapStart/Stop` 已经定义，只缺实现 |
| —   | **macOS / Windows 客户端** | `bifrost-net::macos::utun`（IP-only）；`bifrost-net::windows::wintun`（完整 L2） |

### 不做

- 多 server 联邦 / mesh —— 设计目标是 hub-and-spoke，不打算演化成 mesh VPN
- UDP transport —— TCP + SOCKS5 是有意为之（穿透友好）
- 内置 NAT / DHCP 服务 —— Linux bridge 自己就支持，bifrost 不重造轮子

---

## Troubleshooting

| 现象 | 原因 / 处理 |
|---|---|
| `bifrost-server.service: status=1/FAILURE` 启动后 `create bridge: No such device` | 内核没加载 `bridge` 模块。`modprobe bridge` 然后 `systemctl restart bifrost-server` |
| `[!] connect failed: Connection refused` 客户端反复重试 | 服务端没起 / 防火墙 / SOCKS5 代理路径不通；`bifrost-client admin status` 看 `connected` |
| 浏览器打开 `http://<server-ip>:8080` 看到 connection reset | WebUI 默认只绑 `127.0.0.1`。`ssh -L 8080:127.0.0.1:8080 root@<server-ip>` 转发即可，或者改 `[web].listen` |
| `scp: dest open ...: Failure` | ETXTBSY，二进制正在跑。deploy 脚本应该已经处理；如果手动覆盖记得先 `systemctl stop` |
| `WARN Specified IFLA_INET6_CONF NLA attribute holds more...` | `netlink-packet-route 0.19` 的良性兼容警告，新 kernel 加了字段。可忽略 |
| 升级后老 `server.toml` 里仍有 `[[routes]]` | 这一段已被删除，新 daemon 加载时直接忽略；下一次保存会丢弃。要保留之前的路由信息，对照其 `via`，在对应 client 的行加 `lan_subnets = [...]` |

---

## License

MIT OR Apache-2.0（任选）

---

## Why "Bifrost"

北欧神话中连接 Asgard 与 Midgard 的彩虹桥——把分散的世界拉到同一张网里，
正好对应这个工具的功能。也方便记。
