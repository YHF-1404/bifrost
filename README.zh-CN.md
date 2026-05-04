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
- **三套控制面同源** — daemon 默认起 admin Unix socket + 本机 HTTP/WS（代码默认 `127.0.0.1:8080`，部署示例配置改成 `0.0.0.0:8080`），可选前台 REPL；三者背后是同一个 `HubHandle`
- **WebUI（v3 - 统一视图）** — `web/` 下的 React + Vite + Tailwind 应用。**单页**统一展示所有网络与设备：左右分栏 + 拖拽分隔条；左栏列 pending（未入网）client，右栏一张卡片一个虚拟网络；**拖拽 client 跨栏 / 跨网**即可触发 `assign_client`，每次拖拽完 admit 自动归零、TAP IP 自动清空。**Graph 视图**单画布展示所有网络（每个网络一个实线框），拖到框内 = 入网，右键 Hub 删网络，右键空白处建网络。每个网桥 IP 用段位拣选器（仅支持 `/16` 或 `/24`），client TAP IP 锁住网桥前缀对应的 octets，禁止冲突
- **路由表自动派生** — 每台设备声明背后的 `lan_subnets`，server 端 `device push` 时聚合下发，不再手维护 `[[routes]]`
- **零外部 `ip` 命令依赖** — Linux 后端走 rtnetlink + ioctl 直连内核
- **跨编译开箱即用** — `cross.toml` + Docker 一行编译 x86_64 / aarch64

---

## Architecture

```
┌─ Client (e.g. router, aarch64) ─────┐         ┌─ Server (Linux, x86_64) ────────────┐
│  bifrost-client (daemon)            │         │  bifrost-server (daemon)             │
│  ├─ TAP   tapXXXX  (10.0.0.2/24)    │         │  ├─ Bridge × N  (一个网桥一个虚拟网) │
│  ├─ ConnTask  (TCP / SOCKS5)        │         │  ├─ TAP × M    (一台已 admitted)    │
│  ├─ App  (state machine)            │         │  ├─ Hub        (single actor)        │
│  └─ admin  /run/bifrost/client.sock │         │  ├─ ConnTask × N                     │
│                                     │         │  ├─ SessionTask × N                  │
│                                     │         │  ├─ admin   /run/bifrost/server.sock │
│                                     │         │  └─ WebUI HTTP / WS  (0.0.0.0:8080)  │
└────────────────┬────────────────────┘         └──────────────────┬───────────────────┘
                 │                                                  │
                 │     postcard-framed wire protocol over TCP       │
                 │     (optionally tunnelled through SOCKS5)        │
                 └──────────────────────────────────────────────────┘
```

**关键设计**：

- **Hub 单 actor**：所有控制态（networks / approved_clients / sessions / pending / conns）由一个 `tokio::select!` 任务独占，外部只能通过 `mpsc<HubCmd>` 发命令；不再需要锁。
- **数据面 0 hop**：批准 join 时，Hub 把 `session_cmd_tx` 通过 `bind_tx` 推给 ConnTask，此后 ETH 帧 `socket → ConnTask → SessionTask → TAP` 直连，**不经 Hub**。
- **每个虚拟网一座网桥（Phase 2）**：每个 `[[networks]]` 行拥有自己的 Linux bridge（默认从 UUID 派生 `bf-<8-hex>`，可在 `NetRecord.bridge_name` 自定义）。`mknet` 建网桥，`delete_net` 拆网桥。已 admit 设备的 TAP 只挂到自己网络的桥上——网络之间不共享广播域、ARP、MAC 表、路由表。
- **Session 状态机**：`Joined → Disconnected → Dead`，server 端有 disconnect timeout，client 端用 `None` 表示"永不超时由用户控制"。
- **路由派生而非配置**：每台 admitted client 声明 `lan_subnets`，`device push` 时按网络聚合 → 安装到 **该网络的桥** 上（不是全局），去掉了 `[[routes]]` 这个独立维度。
- **服务端权威分配（Phase 3）**：一个 client 同一时间最多在一个网络。**服务端**决定它在哪个网络（管理员通过 WebUI 拖拽或 `assign` CLI 指令触发），新连入的 client 落到 pending pool，未被分配前 `Join` 会被拒（`unassigned`）；分配变化时服务端推 `Frame::AssignNet { net_uuid }`，client 收到后销毁 TAP 并向新网络重新 `Join`。
- **两个控制面同源**：admin Unix socket（`bifrost-server admin <cmd>`）和 HTTP API（`/api/...`）背后都是 `HubHandle` 的方法；WebUI 只是另一个 consumer。

---

## Build

### 本地开发（macOS / Linux）

```bash
cargo build --workspace
cargo test  --workspace      # 187 个单测 / 集成测试
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

# 客户端 daemon 自启即连入服务端，被自动登记到 pending pool。
# Phase 3 起服务端决定 client 归属哪个网络，用 `assign` 把它分到 NET_UUID：
CLIENT_UUID=$(ssh root@<router-ip> \
  'grep ^uuid /etc/bifrost/client.toml | cut -d\" -f2')
ssh root@<server-ip> "bifrost-server admin assign $CLIENT_UUID <NET_UUID>"
# (服务端会发 Frame::AssignNet 给 client，client 自动 Join 新网，
#  此时 admitted=false，等下一步配 IP 并打开 admit)

# 一次性 admit + 配置：name / IP / LAN 子网。每个 flag 独立，缺省 = 不动。
ssh root@<server-ip> "bifrost-server admin device set $CLIENT_UUID \
  --admit true --name router --ip 10.0.0.2/24 --lan 192.168.200.0/24"

# 后续要把已经 admit 的设备踢回 pending（保留行不删）：
#   ssh root@<server-ip> "bifrost-server admin device set $CLIENT_UUID --admit false"
# 或彻底拆出该网络回到 pending pool：
#   ssh root@<server-ip> "bifrost-server admin assign $CLIENT_UUID none"

# 重新派生路由表并下发给所有成员
ssh root@<server-ip> "bifrost-server admin device push <NET_UUID>"

# 验证
ssh root@<server-ip> 'ping -c 3 10.0.0.2'
```

之后每次 client 重启都会用持久化的 `joined_network` 字段自动重连，server 端的 `approved_clients` 也已落盘 → 全部走自动批准路径。

可选：开 `http://127.0.0.1:8080`（通过 SSH `-L` 转发）拿到同一份信息的可视化视图——直接在左栏把那张 pending 卡片拖到右栏的网络卡片上，等同于上面 4-5 步。详见下文 [WebUI](#webui)。

---

## Configuration

### `server.toml`

```toml
[server]
host = "0.0.0.0"
port = 8888
save_dir = "/var/lib/bifrost/received"

[bridge]                      # 遗留全局段：仅 `disconnect_timeout` 仍生效；
                              # `name` / `ip` 不再权威——但从 Phase 1 升级时
                              # 会被读取一次（见下方"迁移"小节）。
name = "br-bifrost"
ip = "10.0.0.1/24"
disconnect_timeout = 60       # 秒：socket 断开后 TAP 保留多久

[admin]
socket = "/run/bifrost/server.sock"

[web]                         # `deploy/server.toml.example` 默认 `0.0.0.0:8080`，
                              # 方便防火墙后的 VPS 直接访问；代码内默认
                              # `127.0.0.1:8080`。按部署需要改即可。
enabled = true
listen = "0.0.0.0:8080"

[metrics]                     # 预留，目前没有 consumer
enabled = false
listen = "127.0.0.1:9090"

# 以下两段由 daemon 自动填充，无需手编
# [[networks]]          → mknet
# [[approved_clients]]  → device set
#
# 每个 [[networks]] 行拥有自己的内核网桥（Phase 2）：
#   name         = "hml-net"
#   uuid         = "..."
#   bridge_name  = "br-bifrost"        # 新建网络默认从 UUID 派生 `bf-<8-hex>`，
#                                      # 升级时第一个网络会继承遗留 [bridge].name
#   bridge_ip    = "10.0.0.1/24"       # 仅接受 /16 或 /24 (Phase 3)；空 = 纯 L2
#
# 每个 [[approved_clients]] 行的字段：
#   client_uuid  = "..."               # daemon 写入（Phase 3 起每个 client 最多一行）
#   net_uuid     = "..."               # daemon 写入
#   tap_ip       = "10.0.0.2/24"       # device set --ip
#   display_name = "router"            # device set --name
#   lan_subnets  = ["192.168.200.0/24"]  # device set --lan
#   admitted     = true                # device set --admit
#
# Phase 3 — 已连入但还没被分配到任何网络的 client 在另外一段：
# [[pending_clients]]
#   client_uuid  = "..."
#   display_name = ""                  # pending 时也可编辑
#   lan_subnets  = []                  # pending 时也可编辑
#
# 每个网络的路由表是 **从其成员的 `lan_subnets` 派生** 的，
# 只装到该网络的桥上，不再有独立的 [[routes]] 段。
```

#### 从 Phase 1（单一全局桥）迁移

`ServerConfig::load` 会跑一次自动迁移：对每个 `bridge_name` 为空的
`[[networks]]` 行，**第一个** 会继承遗留的 `[bridge].name` /
`[bridge].ip`（你原本的内核桥继续归这个网络所有，无需手工干预），
后续的网络从 UUID 派生 `bf-<8-hex>`。改写后的配置落盘后下次加载是 no-op。

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
| `rename <net-uuid> <new-name>` | 重命名网络 |
| `rmnet <net-uuid>` | **Phase 3** —— 删网络。下属 client 不删，**统一搬到 pending pool**（保留 display_name 和 lan_subnets），同时拆掉内核网桥并重派生路由 |
| `assign <client-uuid> <net-uuid|none>` | **Phase 3** —— 把 client 分配到某网络（`none` 则拆出去回到 pending pool）。服务端发 `Frame::AssignNet` 让 client 销毁 TAP 重新 Join；分到新网后 admitted=false、tap_ip="" 需要再 `device set --ip ... --admit true` |
| `device list [<net-uuid>]` | 列设备。不带 `<net-uuid>` 时同时包含 pending pool 里的 client（行 net 显示为 `-`） |
| `device set <client-uuid> [--name X] [--ip Y/CIDR] [--admit BOOL] [--lan A,B,…]` | 改一台设备的字段。每个 flag 独立：缺省 = 不动；`--ip ""` 清空；`--lan ""` 清空 LAN 列表。`--admit true` 把 pending 升级为 admit（建 TAP + 发 JoinOk），`--admit false` 把在线 session 踢回 pending（杀 socket，行保留）。在线设备会立即收到 `SET_IP` |
| `device push <net-uuid>` | 重新从所有成员的 `lan_subnets` 派生路由表，本机 bridge 上 apply 一遍，并下发 `SetRoutes` 给该网络内所有 joined 客户端 |
| `list` | networks / sessions / pending 全量 snapshot |
| `send <msg>` | 向所有连入客户端广播文本 |
| `sendfile <path>` | 把本地文件广播给所有客户端 |
| `shutdown` | 让 daemon 优雅退出 |

> 旧版本里的 `approve <sid>` / `deny <sid>` / `setip` / `route add/del/push` 已经全部移除。**Admit/踢人统一收敛到 `device set --admit true|false`**：被踢的客户端留在 pending 行（不删），重连重发 `Join` 落回 pending。`device set --ip` 取代 `setip`；`route` 被 per-device `lan_subnets` + `device push` 取代。alpha 阶段，没有迁移路径——直接手工删除老 `server.toml` 里的 `[[routes]]` 段，然后 `device set --lan` 重建。
>
> **Phase 3** 加了 `assign` 一个新动作；`rmnet` 语义改为"拆掉网桥，client 留在 pending pool"。线上协议从 v1 升级到 v2（新增 `Frame::AssignNet`），老版本 client 在 Hello 时会被 `version_mismatch` 拒掉，需要随服务端一起升级。

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

server daemon 自带 HTTP + WebSocket 服务，同一端口同时跑 SPA + REST + `/ws`。前端在 `web/`，React + Vite + Tailwind。

### 监听地址

代码默认 **`127.0.0.1:8080`**，安全前提是只能本机访问，从外面来要走 SSH `-L`：

```bash
ssh -L 8080:127.0.0.1:8080 root@<server-ip>
# 浏览器打开 http://127.0.0.1:8080
```

部署用的 `deploy/server.toml.example` 直接给的是 `listen = "0.0.0.0:8080"`——前提是 VPS 那一层有外部防火墙挡 8080，浏览器才能直接 `http://<server-ip>:8080` 用，**不要在没防火墙的公网上裸跑**（WebUI 没鉴权）。运行时也支持 `--web-listen <addr>` / `--no-web` 命令行覆盖。

### Endpoints

| Method | Path | 行为 |
|---|---|---|
| `GET` | `/api/networks` | 网络列表 + 每网的 `device_count` / `online_count` / `bridge_name` / `bridge_ip` |
| `POST` | `/api/networks` | 建网络。Body `{ "name": "..." }` |
| `PATCH` | `/api/networks/:nid` | 改 `name` 和/或 `bridge_ip`。**Phase 3：** `bridge_ip` 必须是 `/16` 或 `/24` 否则 400；前缀长度变化会自动改写本网络所有 client 的 `tap_ip`（保留 octet）并下发 `SetIp` 给在线 session |
| `DELETE` | `/api/networks/:nid` | **Phase 3** —— 拆该网络的内核网桥；下属 client **不删**，搬到 pending pool 保留 `display_name` 和 `lan_subnets` |
| `GET` | `/api/networks/:nid/devices` | 单个网络下 admitted + pending-admit 设备合并视图 |
| `PATCH` | `/api/networks/:nid/devices/:cid` | 改 device 字段：`name` / `tap_ip` / `lan_subnets` / **`admitted`**（true 升级 pending，false 踢回 pending） |
| `POST` | `/api/networks/:nid/routes/push` | 重新派生路由表 + 下发 `SetRoutes` 到所有 joined peers |
| `GET` | `/api/clients` | **Phase 3** —— 一次性列出所有已知 client（已分配 + pending pool），统一 WebUI 用这个 |
| `PATCH` | `/api/clients/:cid` | **Phase 3** —— 改 client 的 `name` 和/或 `lan_subnets`；不论它在 pending 还是 admitted |
| `POST` | `/api/clients/:cid/assign` | **Phase 3** —— 拖拽分配。Body `{ "net_uuid": <uuid> | null }`，`null` 拆回 pending pool。同网无操作。会推 `Frame::AssignNet` 给 conn；跨网移动每次都重置 `admitted` 与 `tap_ip` |
| `GET` | `/api/ui-layout` | **Phase 3** —— 单文件统一布局：`{ table: { left_ratio, left_collapsed }, graph: { positions, frames } }`，取代旧的 per-network layout 文件 |
| `PUT` | `/api/ui-layout` | **Phase 3** —— 整体替换写盘，前端 debounce 后批量 PUT |
| `GET` | `/ws` | WebSocket：推 `metrics.tick`（1Hz 吞吐采样）/ `device.{online,offline,changed,pending,removed}` / `routes.changed` / `network.{created,changed,deleted}`，25s 心跳 ping |

错误统一信封 `{"error": "..."}`：4xx 是用户可修复的（未知网络、CIDR 格式错、IP 冲突），5xx 留给 hub 故障。

### 生产部署：单二进制

`bifrost-server` 通过 [`rust-embed`](https://crates.io/crates/rust-embed)
在编译期把 `web/dist/` 烤进二进制。先 build 前端再 build 后端：

```bash
cd web && npm install && npm run build && cd ..
cargo build --release -p bifrost-server          # 或 scripts/build-cross.sh
```

`scripts/build-cross.sh` 会自动跑 `npm run build`；想跳过用
`--skip-web`。装好之后，`bifrost-server` 在同一个端口同时服务 API +
WS + SPA：哈希过的资产走 `Cache-Control: immutable`，`index.html`
不缓存，深链接（`/networks/:nid` 等）回退到 `index.html` 让 React
Router 接管。

如果 `web/dist/` 不存在，`build.rs` 会写一个占位 `index.html`，
`cargo build` 仍能通过——但 WebUI 显示 "WebUI not built"
直到你真的跑过 `npm run build`。

### 前端 dev 工作流

只动 React 时不必每次重 build Rust，开 Vite 在前面挡：

```bash
cd web
npm install
npm run dev        # http://127.0.0.1:5173；/api 与 /ws 自动 proxy 到 8080
```

如果 backend 不在 `127.0.0.1:8080`，设 `BIFROST_BACKEND=http://host:port`。HMR 正常工作。

**Phase 3** 起整个 WebUI 收敛成一个页面（`/`），不再有 Networks → Devices 的层级：

- **Table 视图** —— 左右可拖拽分割条，左栏列 pending（未入网）client，右栏一张卡片一个虚拟网络。**用鼠标把 client 卡片在两栏 / 不同网络卡片之间拖动**就是 assign，服务端会推 `Frame::AssignNet` 让 client 销毁 TAP 切到新网。每次跨网拖完 admit 自动归零，TAP IP 自动清空——按规范用户得重新设置。Pending 卡片只显示 `name` 和 `lan_subnets`（不显示 IP 和吞吐）；admitted 行有完整字段 + 60 采样 sparkline。左下角 FAB 折叠/展开左栏；分隔比例和折叠状态由 `/api/ui-layout` 持久化。
- **Graph 视图** —— React Flow 一张画布展示所有网络，每个网络是一个**实线框**容纳自己的 Hub 和已 admitted client；pending client 是画布上自由浮动节点。**拖动 client 进框** = 入网；**拖出所有框** = 拆出回到 pending。**右键 Hub 卡片** → "Delete network"（client 落到 pending pool 不删除）。**右键画布空白** → "Create new network"（新框落在鼠标位置）。框 x/y/w/h、Hub/client 位置都服务端持久化。
- **IP 段位拣选器** —— 网桥 IP 用四 octet 输入框 + `/16 ↔ /24` 切换；client TAP IP 把网桥前缀对应的 octets 锁住（如网桥 `10.0.0.1/24` ⇒ client 拣选器显示 `10.0.0.[__]/24`）；行内冲突检测拦截同网重复 IP。
- **每张卡片各自的 Push routes 按钮** —— 该网络的 LAN 子网改完后弹 toast 提醒用户点击。
- **画布右上角 saving/saved/error 状态 chip** —— 反映 `/api/ui-layout` 异步保存状态。
- 顶栏连接状态徽章：live / connecting / offline，由 WebSocket 状态驱动。
- 每个 admitted 设备的实时吞吐由 1 Hz `metrics.tick` 事件驱动。

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
│  ├─ src/views/                   #   UnifiedView (table) + UnifiedGraphView
│  ├─ src/components/
│  │  ├─ Layout、InlineEdit、Sparkline、ThroughputCell、Toaster
│  │  ├─ IpSegmentInput              #   /16 与 /24 段位拣选器
│  │  ├─ SaveStatusChip              #   layout: saved / unsaved / saving
│  │  └─ ui/                       #   Button、Card、Badge、Switch、Table
│  └─ src/lib/                     #   api / ws / types / metrics /
│                                    #   useUiLayout / eventInvalidator /
│                                    #   toast / format / cn
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

## Wire protocol (v2)

帧格式：

```
┌────────────────────────┬──────────────────────────────────┐
│  u32 BE  payload_len   │  postcard-encoded `Frame`        │
└────────────────────────┴──────────────────────────────────┘
```

`Frame` 是个 13-variant enum：

| Variant | 方向 | 时机 |
|---|---|---|
| `Hello / HelloAck` | C↔S | 第一帧握手；带 `version` 与 `caps` |
| `Join / JoinOk / JoinDeny` | C→S / S→C | 加入虚拟网络 |
| `Eth(Vec<u8>)` | 双向 | 数据面以太帧 |
| `AssignNet { net_uuid }` | S→C | **v2** —— 服务端推送的网络分配。`Some` 把 client 切到该网络，client 销毁 TAP 重新 `Join`；`None` 拆回 idle |
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

### 已完成（Phase 1.0–1.6 — WebUI v1，Phase 2.0 — 多租户 L2，Phase 3.0 — 统一 WebUI + 服务端权威分配）

- ✅ **per-device `lan_subnets`** 取代全局 `[[routes]]`；路由表按需派生。CLI 改为 `device list/set/push`。
- ✅ **单一 admit 开关**取代 approve / deny / kick：CLI 用 `device set --admit true|false`，HTTP 用 `PATCH … {admitted:…}`；被踢的设备保留为 pending 行，重连重发 `Join` 落回 pending —— 不需要协议层改动。
- ✅ **WebUI 网络 CRUD** —— 列表页和 Graph 视图的 Hub 卡片都能新建 / 改名 / 删除网络；后端 `POST/PATCH/DELETE /api/networks[/:nid]` 配合三个新事件 `network.{created,changed,deleted}`。
- ✅ **`bifrost-web` crate** —— axum HTTP/WS，代码默认 `127.0.0.1:8080`，部署示例改 `0.0.0.0:8080`。提供完整 CRUD + `/api/.../layout`（graph 节点位置）+ `/api/.../routes/push` + WS 事件流。错误统一 `{"error": "..."}`。
- ✅ **`web/` SPA** —— React + Vite + TS + Tailwind。两种可切换视图（per-tab 持久化）；行内编辑乐观更新 + 失败回滚 + 校验失败弹 toast；admit 开关；"Push routes" 按钮在 LAN 改了之后琥珀色脉冲。
- ✅ **Graph 视图（React Flow）**：Hub 卡片网络名 InlineEdit；**连线浮动吸附**到两节点最近的一对边中点；**节点位置服务端持久化** + 画布右上角 saving/saved/error 状态 chip；fitView 首次居中；带 minimap。
- ✅ **per-session 字节计数 + 1 Hz 采样**：每台设备同时显示数字 bps + 60 采样 sparkline。WS 事件驱动 TanStack Query invalidate，UI 不轮询，30 s 兜底刷一次。
- ✅ **单二进制部署**：`rust-embed` 把 `web/dist/` 编进 `bifrost-server`；同端口服务 SPA + API + WS；深链接回退 `index.html`；哈希资产 immutable cache；`<save_dir>/layouts/<nid>.json` 存 per-network UI 状态。
- ✅ **每个虚拟网一座 Linux bridge（Phase 2.0）**：`mknet` 建网桥，`delete_net` 拆网桥；admit 设备的 TAP 只挂到自己网络的桥上。`NetRecord` 新增 `bridge_name`（默认 `bf-<8-hex>`）和 `bridge_ip`（host 侧网关地址，可空）。从 Phase 1 升级时，第一个网络自动继承遗留 `[bridge]` 段。网络之间不共享广播域、ARP、MAC 表、路由表。
- ✅ **服务端权威分配 + 协议 v2（Phase 3.0）**：新增 `pending_clients` 表跟踪已连未入网的 client；新帧 `Frame::AssignNet` 让服务端把 client 在网络间挪动（client 端销毁 TAP 重新 Join）。一个 client 同时只属于一个网络（Phase 2 → 3 自动迁移）。新增 `GET /api/clients`、`PATCH /api/clients/:cid`、`POST /api/clients/:cid/assign`；`PATCH /api/networks/:nid` 加上 `bridge_ip`（仅接受 `/16` 或 `/24`，前缀变化时自动改写所有 client 的 `tap_ip` 保留 octet）。`delete_net` 不再级联删 client，改为搬到 pending pool。新增 CLI 子命令 `assign <client> <net|none>`。
- ✅ **统一 WebUI（Phase 3.0）**：把原本的 Networks 列表 + 单网详情合并为一页。Table 模式用 `@dnd-kit/core` 做拖拽分配；Graph 模式用 React Flow 单画布展示所有网络（每个网络一个实线框 + 右键菜单）。段位拣选器锁定 IP；单文件 `ui-layout.json` 替代旧的 per-network layout（启动一次性合并旧文件）。
- ✅ **187 个测试**通过，clippy `-D warnings` 干净。

### Roadmap（按阶段）

| 阶段 | 内容 | 备注 |
|---|---|---|
| 3.x | `mknet` CLI 加 `--ip <cidr>` flag | Phase 3.0 已经在 WebUI + `PATCH /api/networks/:nid` 加上了网桥 IP 段位拣选器，但 `mknet` CLI 还得手编 `server.toml` 给新网络配 IP。 |
| —   | **Noise XX 加密 transport** | `Hello.caps` 已经预留 bit；上 `snow` crate 实现 `Transport` trait 即可，业务代码无需改动 |
| —   | **Prometheus metrics** | `metrics-exporter-prometheus`；per-session 字节 / 帧 / 丢包（1.2 帮它打地基；`[metrics]` 配置段已存在但目前是空挂） |
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
| 浏览器打开 `http://<server-ip>:8080` 看到 connection reset | 代码默认 `[web] listen = "127.0.0.1:8080"`，`ssh -L 8080:127.0.0.1:8080 root@<server-ip>` 转发即可。`deploy/server.toml.example` 改成了 `0.0.0.0:8080`，**前提是 VPS 那一层有外部防火墙挡 8080**，否则不要公开监听 |
| 浏览器透过代理打 `http://<server-ip>:8080` 慢且 WS 一直 connecting | xray-core HTTP inbound 默认对 plain-HTTP 做 forward-proxy 并按 RFC 2616 把 `Connection: Upgrade` 当 hop-by-hop 头剥掉，axum 拒绝 400。对策：daed/客户端的代理出口换成 SOCKS5（xray 同时开 socks-inbound 即可），SOCKS5 是纯 TCP 隧道不动 HTTP 头，WS Upgrade 能通过 |
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
