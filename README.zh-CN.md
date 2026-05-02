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
- **REPL + Admin Unix socket** — daemon 默认无 REPL（适合 systemd），通过 `bifrost-{server,client} admin <cmd>` 子命令做一次性 RPC
- **集中式 IP/路由分发** — 服务端 `setip` / `route` 命令热更新，自动落盘 + 推给在线客户端
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
│                                     │         │  ├─ SessionTask × N (per session)   │
└────────────────┬────────────────────┘         │  └─ admin  /run/bifrost/server.sock │
                 │                               └──────────────────┬──────────────────┘
                 │     postcard-framed wire protocol over TCP       │
                 │     (optionally tunnelled through SOCKS5)        │
                 └──────────────────────────────────────────────────┘
```

**关键设计**：

- **Hub 单 actor**：所有控制态（networks / approved_clients / routes / sessions / pending / conns）由一个 `tokio::select!` 任务独占，外部只能通过 `mpsc<HubCmd>` 发命令；不再需要锁。
- **数据面 0 hop**：批准 join 时，Hub 把 `session_cmd_tx` 通过 `bind_tx` 推给 ConnTask，此后 ETH 帧 `socket → ConnTask → SessionTask → TAP` 直连，**不经 Hub**。
- **Session 状态机**：`Joined → Disconnected → Dead`，server 端有 disconnect timeout，client 端用 `None` 表示"永不超时由用户控制"。

---

## Build

### 本地开发（macOS / Linux）

```bash
cargo build --workspace
cargo test  --workspace      # 120 个单测 / 集成测试
cargo clippy --workspace --all-targets -- -D warnings
```

macOS 上的二进制可以跑 daemon、admin RPC、所有协议层逻辑（用 `NullPlatform`），但 `create_tap` 会运行时报 `Unsupported`——TAP / bridge 仅 Linux 支持。

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

# 给客户端分配 TAP IP（需要在 server 端 admin）
CLIENT_PREFIX=$(ssh root@<router-ip> \
  'grep ^uuid /etc/bifrost/client.toml | cut -d\" -f2 | cut -c1-8')
ssh root@<server-ip> "bifrost-server admin setip $CLIENT_PREFIX 10.0.0.2/24"

# 验证
ssh root@<server-ip> 'ping -c 3 10.0.0.2'
```

之后每次 client 重启都会用持久化的 `joined_network` 字段自动重连，server 端的 `approved_clients` 也已落盘 → 全部走自动批准路径。

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

[metrics]                     # 暂未启用，预留
enabled = false
listen = "127.0.0.1:9090"

# 以下三段由 daemon 自动填充，无需手编
# [[networks]]          → mknet
# [[approved_clients]]  → approve / setip
# [[routes]]            → route add / route del
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
| `setip <prefix> <ip>` | 按 client UUID 前缀更新 TAP IP；在线客户端立即收到 SetIp |
| `route add <dst/cidr> via <gw>` | 加路由（落盘） |
| `route del <dst>` | 删路由 |
| `route list` | 等同 `list` 中的 routes 段 |
| `route push` | 把当前路由表推给所有 bound 客户端 |
| `list` | networks / sessions / pending / routes 全量 snapshot |
| `send <msg>` | 向所有连入客户端广播文本 |
| `sendfile <path>` | 把本地文件广播给所有客户端 |
| `shutdown` | 让 daemon 优雅退出 |

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
│  │                                #   纯逻辑，跨平台编译，覆盖率最高
│  │
│  ├─ bifrost-server/              # 二进制 + lib：accept_loop / ConnTask / admin / repl
│  └─ bifrost-client/              # 二进制 + lib：ConnTask 重连 / App / admin / repl
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

### 已完成（P0）

- ✅ `bifrost-proto` — Frame + Codec + admin RPC types（24 单测）
- ✅ `bifrost-net` — Tap / Bridge trait + mock + null + Linux 后端（rtnetlink）
- ✅ `bifrost-core` — Hub actor + Session 状态机 + config 持久化
- ✅ `bifrost-server` / `bifrost-client` — daemon + admin Unix socket + 可选 REPL
- ✅ 跨编译：x86_64-linux-gnu + aarch64-linux-gnu via `cross`
- ✅ systemd unit + 部署脚本，已在生产 Arch + Ubuntu aarch64 跑通端到端
- ✅ 测试：120 通过 + 1 ignored doctest

### Roadmap（按优先级）

- ⏳ **Noise XX 加密 transport** — `Hello.caps` 已经预留 bit；上 `snow` crate 实现 `Transport` trait 即可，业务代码无需改动
- ⏳ **Prometheus metrics endpoint** — `metrics-exporter-prometheus`，命名规则见 `docs`（暂未恢复）
- ⏳ **per-session pcap dump** — `pcap-file` crate，`SessionCmd::PcapStart/Stop` 已经定义，只缺实现
- ⏳ **macOS / Windows 客户端** — `bifrost-net::macos::utun`（IP-only，需协议层补 L3 fallback）；`bifrost-net::windows::wintun`
- ⏳ **`route list` 单独输出** — 当前 admin 复用 List 拿全量 snapshot，UX 上 `route list` 应该只打路由
- ⏳ **`SetIp` 后客户端 `App.joined_tap_ip` 没更新** — `admin status` 显示 `tap_ip: -`，但内核里其实正确

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
| `bifrost-server admin ...` 报 `[*] generated client uuid:` | 旧版本 bug，admin 路径会写 config。升级 client 二进制 |
| `scp: dest open ...: Failure` | ETXTBSY，二进制正在跑。deploy 脚本应该已经处理；如果手动覆盖记得先 `systemctl stop` |
| `WARN Specified IFLA_INET6_CONF NLA attribute holds more...` | `netlink-packet-route 0.19` 的良性兼容警告，新 kernel 加了字段。可忽略 |

---

## License

MIT OR Apache-2.0（任选）

---

## Why "Bifrost"

北欧神话中连接 Asgard 与 Midgard 的彩虹桥——把分散的世界拉到同一张网里，
正好对应这个工具的功能。也方便记。
