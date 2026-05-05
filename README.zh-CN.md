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

### 可选：内核调优以提高吞吐

如果 server 或 client 跑在**单队列 NIC** 上（USB 以太网适配器、多数 ARM SBC、嵌入式板子等），bifrost 的批量吞吐上限被 `NET_RX` softirq 钉在单核上。要把这部分工作分散到多核，每台机器开机后跑一次：

```bash
sudo scripts/tune-host.sh             # 自动选默认路由 NIC
sudo scripts/tune-host.sh end0        # 或者手动指定
```

脚本启用 RPS / RFS / XPS。本项目 LAN 测试床（Cortex-A55 四核 + 单队列千兆 NIC）上，单流上行从 361 Mbps 涨到 451 Mbps（约 +25%）。设置是运行时的，重启后失效 —— 写进 `/etc/rc.local` / `systemd-tmpfiles` drop-in / udev 规则可持久化。

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
| `mknet <name> [--ip <cidr>]` | 创建虚拟网络，返回 UUID。`--ip` 可选，给虚拟网桥配主机侧 gateway IP（如 `--ip 10.0.0.1/24`），仅接受 `/16` 或 `/24` 前缀（和 WebUI 段位拣选器一致）；不带 `--ip` 时网桥不带 host 地址，可后续通过 WebUI / `PATCH /api/networks/:nid` 设置 |
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

- **Table 视图** —— `react-resizable-panels` 提供的左右分栏 + 可拖拽分隔条；左栏列 pending（未入网）client，右栏一张卡片一个虚拟网络。`@dnd-kit/core` 实现拖拽：**把 client 卡片在两栏 / 不同网络卡片之间拖动**就是 assign，服务端推 `Frame::AssignNet` 让 client 销毁 TAP 切到新网；TanStack Query 的 `onMutate` 立即写 cache，drop 当帧卡片就到位（无 fly-back 反向动画——`<DragOverlay dropAnimation={null}>`）。每次跨网拖完 admit 自动归零、TAP IP 自动清空——按规范用户得重新设置。Pending 卡片只显示 `name` 和 `lan_subnets`（不显示 IP 和吞吐）；admitted 行有完整字段 + 60 采样 sparkline。左下角 FAB 通过 `ImperativePanelHandle.collapse()` / `.expand()` 折叠/展开左栏，pane 不卸载所以拖宽尺寸跨折叠保留；分隔比例和折叠状态由 `/api/ui-layout` 持久化。
- **Graph 视图** —— React Flow 一张画布展示所有网络，每个网络是一个**实线框**容纳自己的 Hub 和已 admitted client；pending client 是画布上自由浮动节点。Hub 和 Client 卡片都富编辑（admit Switch / 名字 / 段位 IP / LAN 子网 chips / 吞吐），仅顶部宽 header 是 drag handle，编辑控件不会触发拖拽。
  - **拖 client 进框** = 入网；**拖出所有框** = 拆回 pending pool。卡片留在用户松手的位置上（drop 位置在目标 frame 的坐标系内换算后再触发 assign mutation）。
  - **右键 Hub 卡片** → "Delete network"（client 落到 pending pool 不删除）。
  - **右键画布空白** → "Create new network"（`screenToFlowPosition` 把新框落在鼠标位置）。
  - 框 **四面自动伸张**容纳 Hub + 所有 admitted client；多个框 **不重叠**：迭代式 AABB 碰撞解算器沿较小重叠轴推开未 pin 的（无用户保存位置的）那一个。
  - 连线用自定义 **`FloatingEdge`**：每帧用 `useInternalNode` 拿两端节点的位置和尺寸，挑距离最近的一对边中点画 bezier，线段端点精确落在卡片可见边框上（卡片内部用 `h-full w-full flex-col` 让内容贴合 React Flow wrapper，不留 padding 空隙）。
  - 框 x/y/w/h、Hub/client 位置都通过 `/api/ui-layout` 服务端持久化。
- **IP 段位拣选器** —— 网桥 IP 用四 octet 输入框 + 一个**点击切换** `/16 ↔ /24` 的按钮（取代原生 `<select>`，那个一打开就丢焦点退出编辑）+ 一个 "ok" 显式提交按钮。client TAP IP 把网桥前缀对应的 octets 锁住（如网桥 `10.0.0.1/24` ⇒ client 拣选器显示 `10.0.0.[__]/24`）；行内冲突检测同时拦截同网内重复 IP **以及** 与网桥 IP 相同。
- **每张卡片各自的 Push routes 按钮** —— LAN 子网改完后弹 info toast，按钮变琥珀色 + 微脉冲 + 末尾加 `•` 提示需要点击下发；push 成功清掉。
- **toolbar 上的 saving/saved 状态 chip** —— 反映 `/api/ui-layout` 异步保存状态。
- **WS 状态徽章**：`live` / `connecting` / `offline`，由 WebSocket 状态驱动。
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

### 已完成（Phase 3.1 — 数据面性能 + WebUI 抛光）

- ✅ **批量传输性能重做**。一组小修复让单流上行从用户原始 26 KB/s（生产 xray 隧道）涨到 4.1 MB/s（约 150×），千兆 LAN 测试床上从 222 Mbps 涨到 361 Mbps。要点：
  - 数据面 socket 全部启用 `TCP_NODELAY`（server `accept`、client `connect`、SOCKS5 包裹后的内层 TcpStream），消除 Nagle 等待上一段 ACK 的卡延迟。
  - 同样 socket 上 `SO_SNDBUF` 限到 256 KB，避免内核自动调到几 MB 之后把下游隧道的拥塞从内层 TCP 那里完全屏蔽（CUBIC 找不到丢包就一直涨 cwnd 直到 bufferbloat 把吞吐压垮）。helper 是 `bifrost-net::set_send_buffer_size`。
  - TAP 与每张虚拟桥的 MTU 设为 **1400**，1500 字节内层以太帧 + 4 字节长度前缀 + postcard tag + 外层 TCP/IP header 还能塞进底层 1500 物理 MTU；不设的话嵌套 TCP 隧道下整帧会分片或丢弃。
  - 两端都改回每帧一次 `framed.send`。前一版"feed 通道里所有帧 + 单次 flush"看似省 syscall，实测是性能反优化：把 1 MB 量级写入塞进慢下游代理（xray-core），把它的 RWND 砸到 0，外层 cwnd 被打回 10。
  - 数据面 mpsc 通道扩容（`128 → 1024`），让短暂的 socket stall 不会立刻把 TAP 读循环反压住。
- ✅ **零分配 Frame 编码**。`FrameCodec::encode` 在 perf 上吃掉了 10% 的 CPU。`bifrost-proto` 里两处修复：自定义 `BytesMutFlavor` 让 postcard 直接序列化进目标 `BytesMut`（去掉 `to_allocvec` 中转 Vec）；`#[serde(with = "serde_bytes")]` 标在 `Frame::Eth(Vec<u8>)` 和 `Frame::File::data` 上，让 postcard 把 payload 当字节串处理（一次 `try_extend(slice)`），而不是按序列里 1500 个独立 `u8` 调 `try_push`。线格式不变。两处修复后 `FrameCodec::encode` 直接跌出 perf top 25。
- ✅ **`bridge_ip` 实时更新**。`PATCH /api/networks/:nid` 改桥 IP 现在会通过 netlink 推到 kernel 桥上（`Bridge::set_ip` 加进 trait + `LinuxBridge` 实现 `flush_addrs` / `add_addr`）。之前只更新了内存配置和持久化，要重启 server 才生效。
- ✅ **Table 视图**：每行升级成圆角卡片（细边框 + 浅阴影 + hover 高亮），相邻行有清晰边界；列模板和顶部表头条共享同一份 grid，`name` / `tap IP` / `LAN subnets` / `throughput` / `uuid` 永远纵向对齐；`dnd-kit` 改用 `pointerWithin` 碰撞检测，PENDING 区任何位置（包括紧贴顶部）都能放进去 —— 之前默认 `rectIntersection` 让 200px 宽的拖动预览框跟更宽的 Networks 区相交面积更大，落点会被吃掉。
- ✅ **Graph 视图**：客户端卡片高度按 LAN 子网数量自动算（`base + (lan_rows - 1) * 22 px`），5 个 LAN 也不会把流量图挤出 React Flow 包装；IP 段位拣选器输入框宽度从 `w-9 → w-12`，3 位数（`255`）也宽松；流量数值列改 `w-20 + whitespace-nowrap`，`99.9 GB/s` 也能单行。

### 已完成（Phase 3.2 — LAN 测试床上的批量吞吐）

aarch64 Cortex-A55 client → x86_64 server，千兆 LAN 单流（直连 LAN 基线 940 Mbps）：

| 路径 | 上行 | 下行 |
|---|---|---|
| bifrost 直连 | 497 Mbps | 446 Mbps |
| bifrost + xray-core (VLESS Reality) | 316 Mbps | 335 Mbps |

在 Phase 3.1 基础上新增三层改动：

- ✅ **`scripts/tune-host.sh`** —— 单队列 NIC 主机的 RPS / RFS / XPS 配置脚本（USB 以太网适配器、多数嵌入式 ARM 板都属于此类）。不开启时 `NET_RX` softirq 钉死在第一次处理 IRQ 的那个核上；脚本把这部分活在软件层面分散到所有核。LAN 测试床上单流上行从 361 Mbps 涨到 451 Mbps。

- ✅ **有界批量发送（每次最多 32 帧或 32 KB，谁先到为准）**。两端都从单帧 `framed.send` 改成累一批后再 flush。单帧写入时内核 TCP 没法 TSO 合并 —— 每个 1.4 KB 的以太帧只能变成一个 MSS 段；中间的代理（xray-core）也得对每 1.4 KB 跑一遍完整的 VLESS framing + 加密。有界批量后每次 flush 出去 ~30 KB —— 远小于任何合理的接收 buffer（xray 自调到 ~256 KB），但又大到让 NIC TSO 把它一次拆成 ~22 个网线包，xray 看到的是一次大读而不是 22 次小读。这次有界版修了之前"无界一把梭"的翻车 —— 之前在长 RTT 的 VPS xray 隧道上把 RWND 砸到 0，cwnd 直接重置到 10；32 KB 的上限把这条路也保住。

- ✅ **`bridge_ip` 实时更新**（继承自 3.1 的 bug 修复）。`Bridge::set_ip` 加进 trait + `Hub::handle_set_net_bridge_ip` 经 netlink 推到内核桥，WebUI/API 改桥 IP 不用再重启 server。

诊断笔记（commit log 里有完整 perf 痕迹）：

- 还差 LAN 线速的那部分（940 → 497 Mbps，bifrost 单跑丢一半）是用户态 VPN 的根本代价：每个内层以太帧得跨 6 次内核网络栈（直连 iperf3 只跨 1 次），而且单帧 outer TCP 写法没法在不打爆代理 recv buffer 的前提下被 TSO 合并。
- 突破 ~500 Mbps 单流要架构改动 —— per-client 多连接、GSO 风格的 super-frame、或者 io_uring 批量 I/O。每一项都涉及协议级改动，先押后。

### 已完成（Phase 3.x — 零碎收尾）

- ✅ **服务端驱动的 `routes.dirty` 信号**。hub 现在显式跟踪每个网络的"是否需要 push?"状态：用一个内存里的 `last_pushed_routes` 快照，由 `device_push` 更新；每次任何配置变更（admit / kick / 改 `lan_subnets` / 跨网迁移 / 删除）后跟当前 `derive_routes_for_network` 比较一次。状态翻转时发 `HubEvent::RoutesDirty { network, dirty }`，`HubSnapshot::routes_dirty` 带上当前集合，新打开的 WebUI 不需要轮询就能画出正确的脉冲状态。`Network` API 行新增 `routes_dirty: bool` 字段；Table 和 Graph 两种视图都从这里驱动琥珀色脉冲（额外保留一个本地 optimistic 集合，让保存和脉冲之间的 round-trip 感觉是即时的）。修复了长期存在的一个场景：admit 一个带 `lan_subnets` 的新 client 时，网络里其他 peer 默默地不知道有这些子网，要靠人手点 "push routes"。

- ✅ **`mknet --ip <cidr>` CLI flag**。`bifrost-server admin mknet <name> --ip 10.0.0.1/24` 一次性建网络 + 配桥 IP，校验只允许 `/16` 或 `/24`（和 WebUI 段位拣选器一致）。kernel 桥在创建时就经 netlink 装上 IP，不需要再 `PATCH`，更不用手编 `server.toml`。in-process REPL 上是 `ip=<cidr>` 同义语法。admin 协议的 `MakeNet` 请求和 `NetEntry` snapshot 行都新增了 `bridge_ip` 字段；非法 CIDR 会立即报错且不留半截创建好的网络。

- ✅ **Phase 3 stale-config Join 竞态**修复。之前 client 配置文件里残留旧 server 的 `joined_network` 时，`HelloAck` 之后会立刻按这个旧 UUID 发 `Join`，与服务端的 `AssignNet` 抢跑，结果是 `JoinDeny: unknown_network` 然后是 `WARN JoinOk without prior Join — ignoring`，session 永久卡死。现在 client 不再从 cache 自动 Join —— 服务端 `AssignNet` 是唯一的 source of truth，REPL/admin 的 `join <net>` 走单独的 `pending_user_join` 字段不会和缓存值打架。

### Roadmap（按阶段）

| 阶段 | 内容 | 备注 |
|---|---|---|
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
