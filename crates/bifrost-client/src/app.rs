//! `App` — the client-side controller.
//!
//! Drives a single client through its full lifecycle:
//!
//! ```text
//!                ┌── ConnEvent ──┐
//!  ConnTask ────►│               │            ┌─── SessionCmd ──►  SessionTask (owns local TAP)
//!                │     App       │            │
//!  REPL    ────►│               │────────────┘
//!                │  joining_net  │            ◄── SessionEvt ───── (death events)
//!                │  joined_net   │            ◄── Frame::Eth ────  (server frames)
//!                │  hello_acked  │
//!                └───────────────┘
//! ```
//!
//! State variables:
//!
//! * `joining_net` — UUID we last sent (or persisted intent for) `Join`.
//!   Survives reconnects so the next HelloAck triggers an automatic
//!   re-Join.
//! * `joined_net` — UUID we currently have an active `SessionTask` for.
//! * `hello_acked` — true between HelloAck and the next Disconnected.
//!   Gates whether `Frame::Join` may be emitted immediately or must
//!   wait for the next handshake.
//!
//! The controller persists `client.toml` whenever it accepts new state
//! from the server (`SetIp`, `SetRoutes`, `JoinOk`, `JoinDeny`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::{ClientConfig, WireRoute as CfgWireRoute};
use bifrost_core::{SessionCmd, SessionEvt, SessionId, SessionTask};
use bifrost_net::Platform;
use bifrost_proto::{Frame, PROTOCOL_VERSION};
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::conn::ConnEvent;
use crate::repl::{ClientStatusSnapshot, UserCmd};

/// Channels and config the controller is built from.
pub struct AppPorts {
    pub cfg: ClientConfig,
    pub cfg_path: PathBuf,
    pub platform: Arc<dyn Platform>,
    /// Sink of frames the controller pushes outbound (handed to ConnTask).
    pub out_tx: mpsc::Sender<Frame>,
    /// Stream of connection events from ConnTask.
    pub events_rx: mpsc::Receiver<ConnEvent>,
    /// Stream of REPL commands.
    pub user_rx: mpsc::Receiver<UserCmd>,
}

pub struct App {
    cfg: ClientConfig,
    cfg_path: PathBuf,
    platform: Arc<dyn Platform>,

    out_tx: mpsc::Sender<Frame>,
    events_rx: mpsc::Receiver<ConnEvent>,
    user_rx: mpsc::Receiver<UserCmd>,

    session_tx: Option<mpsc::Sender<SessionCmd>>,
    session_evt_rx: mpsc::Receiver<SessionEvt>,
    session_evt_tx: mpsc::Sender<SessionEvt>,

    joining_net: Option<Uuid>,
    joined_net: Option<Uuid>,
    /// Set when a session task is alive (we created a local TAP).
    joined_tap_name: Option<String>,
    /// Cached IP of the local TAP, mirrors the on-disk `[tap] ip = ...`.
    joined_tap_ip: Option<String>,
    hello_acked: bool,
}

impl App {
    pub fn new(ports: AppPorts) -> Self {
        let (evt_tx, evt_rx) = mpsc::channel(8);

        // If the previous run was joined, schedule a re-Join on first HelloAck.
        let joining_net = (!ports.cfg.client.joined_network.is_empty())
            .then(|| ports.cfg.client.joined_network.parse::<Uuid>().ok())
            .flatten();

        Self {
            cfg: ports.cfg,
            cfg_path: ports.cfg_path,
            platform: ports.platform,
            out_tx: ports.out_tx,
            events_rx: ports.events_rx,
            user_rx: ports.user_rx,
            session_tx: None,
            session_evt_rx: evt_rx,
            session_evt_tx: evt_tx,
            joining_net,
            joined_net: None,
            joined_tap_name: None,
            joined_tap_ip: None,
            hello_acked: false,
        }
    }

    /// Run until the REPL signals quit or all input streams close.
    pub async fn run(mut self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                Some(evt) = self.events_rx.recv() => self.on_conn_event(evt).await?,
                Some(cmd) = self.user_rx.recv() => {
                    if matches!(cmd, UserCmd::Quit) { break; }
                    self.on_user_cmd(cmd).await?;
                }
                Some(sevt) = self.session_evt_rx.recv() => self.on_session_evt(sevt),
                else => break,
            }
        }

        // Clean teardown: tell session to die and briefly wait for it.
        if let Some(tx) = self.session_tx.take() {
            let _ = tx.send(SessionCmd::Kill).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), self.session_evt_rx.recv()).await;
        }
        Ok(())
    }

    // ── ConnTask events ──────────────────────────────────────────────

    async fn on_conn_event(&mut self, evt: ConnEvent) -> anyhow::Result<()> {
        match evt {
            ConnEvent::Connected => {
                self.hello_acked = false;
                let client_uuid: Uuid = self.cfg.client.uuid.parse()?;
                self.out_tx
                    .send(Frame::Hello {
                        version: PROTOCOL_VERSION,
                        client_uuid,
                        caps: 0,
                    })
                    .await?;
            }
            ConnEvent::Disconnected => {
                self.hello_acked = false;
                if let Some(tx) = &self.session_tx {
                    let _ = tx.send(SessionCmd::UnbindConn).await;
                }
                println!("[-] disconnected, retrying...");
            }
            ConnEvent::FrameIn(frame) => self.on_server_frame(frame).await?,
        }
        Ok(())
    }

    // ── Server frames ────────────────────────────────────────────────

    async fn on_server_frame(&mut self, frame: Frame) -> anyhow::Result<()> {
        match frame {
            Frame::HelloAck { version, .. } => {
                if version != PROTOCOL_VERSION {
                    anyhow::bail!(
                        "server speaks protocol v{version}, this client speaks v{PROTOCOL_VERSION}"
                    );
                }
                self.hello_acked = true;
                if let Some(net) = self.joining_net {
                    self.out_tx.send(Frame::Join { net_uuid: net }).await?;
                }
            }
            Frame::JoinOk { tap_suffix, ip } => self.on_join_ok(tap_suffix, ip).await?,
            Frame::JoinDeny { reason } => self.on_join_deny(reason).await,
            Frame::Eth(bytes) => {
                if let Some(tx) = &self.session_tx {
                    let _ = tx.send(SessionCmd::EthIn(bytes)).await;
                }
            }
            Frame::SetIp { ip } => self.on_set_ip(ip).await,
            Frame::SetRoutes(routes) => self.on_set_routes(routes).await,
            Frame::Text(msg) => println!("[server] > {msg}"),
            Frame::File { name, data } => match save_received_file(&self.cfg.client.save_dir, &name, &data).await {
                Ok(path) => println!(
                    "[server] file {name:?} ({} B) → {}",
                    data.len(),
                    path.display()
                ),
                Err(e) => warn!(error = %e, "save file failed"),
            },
            Frame::Ping(nonce) => {
                let _ = self.out_tx.send(Frame::Pong(nonce)).await;
            }
            Frame::Pong(_) => {}
            // The server should never originate a Hello/Join.
            f @ (Frame::Hello { .. } | Frame::Join { .. }) => {
                warn!(?f, "unexpected client→server frame from server");
            }
        }
        Ok(())
    }

    async fn on_join_ok(&mut self, tap_suffix: String, ip: Option<String>) -> anyhow::Result<()> {
        let net = match self.joining_net {
            Some(n) => n,
            None => {
                warn!("JoinOk without prior Join — ignoring");
                return Ok(());
            }
        };

        // Reconnect path: existing session for the same network → just rebind.
        if self.joined_net == Some(net) {
            if let Some(tx) = &self.session_tx {
                let _ = tx.send(SessionCmd::BindConn(self.out_tx.clone())).await;
                if let Some(ip_str) = ip.clone() {
                    let _ = tx.send(SessionCmd::SetIp(Some(ip_str))).await;
                }
            }
            println!("[+] rejoined network {}", net_short(&net));
            return Ok(());
        }

        // Fresh session: create local TAP, spawn SessionTask.
        let tap_name = format!("tap{tap_suffix}");
        let ip_parsed = ip.as_deref().and_then(|s| s.parse().ok());
        let tap = match self.platform.create_tap(&tap_name, ip_parsed).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[!] create_tap failed: {e}");
                self.joining_net = None;
                self.cfg.client.joined_network.clear();
                let _ = self.cfg.save(&self.cfg_path).await;
                return Ok(());
            }
        };

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        // Client-side counters are unused (the server samples its own
        // side of the conversation); supply fresh atomics so the type
        // checks. They are dropped when the session ends.
        let bytes_in = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let bytes_out = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let task = SessionTask::new(
            SessionId(0), // sid is server-side concept; on client we don't track
            self.cfg.client.uuid.parse().unwrap_or(Uuid::nil()),
            net,
            tap,
            cmd_rx,
            self.session_evt_tx.clone(),
            None, // no disconnect timeout — the user controls TAP lifetime
            bytes_in,
            bytes_out,
        );
        tokio::spawn(task.run(self.out_tx.clone()));
        self.session_tx = Some(cmd_tx);
        self.joined_net = Some(net);
        self.joined_tap_name = Some(tap_name.clone());
        self.joined_tap_ip = ip.clone();

        // Persist
        self.cfg.client.joined_network = net.to_string();
        if let Some(ip_str) = ip.clone() {
            self.cfg.tap.ip = ip_str;
        }
        let _ = self.cfg.save(&self.cfg_path).await;

        println!("[+] joined network {} (TAP={tap_name})", net_short(&net));
        Ok(())
    }

    async fn on_join_deny(&mut self, reason: String) {
        eprintln!("[!] join denied: {reason}");
        if let Some(tx) = self.session_tx.take() {
            let _ = tx.send(SessionCmd::Kill).await;
        }
        self.joining_net = None;
        self.joined_net = None;
        self.cfg.client.joined_network.clear();
        let _ = self.cfg.save(&self.cfg_path).await;
    }

    async fn on_set_ip(&mut self, ip: Option<String>) {
        if let Some(tx) = &self.session_tx {
            let _ = tx.send(SessionCmd::SetIp(ip.clone())).await;
        }
        // Mirror the new value into both the cached state (so admin
        // status reflects it immediately) and the on-disk config.
        self.joined_tap_ip = ip.clone();
        self.cfg.tap.ip = ip.clone().unwrap_or_default();
        let _ = self.cfg.save(&self.cfg_path).await;
        println!("[*] TAP IP updated: {}", ip.unwrap_or_else(|| "(cleared)".into()));
    }

    async fn on_set_routes(&mut self, routes: Vec<bifrost_proto::RouteEntry>) {
        if let Some(tx) = &self.session_tx {
            let _ = tx.send(SessionCmd::SetRoutes(routes.clone())).await;
        }
        self.cfg.routes = routes
            .iter()
            .map(|r| CfgWireRoute {
                dst: r.dst.clone(),
                via: r.via.clone(),
            })
            .collect();
        let _ = self.cfg.save(&self.cfg_path).await;
        println!("[*] {} route(s) received", routes.len());
    }

    // ── REPL ─────────────────────────────────────────────────────────

    async fn on_user_cmd(&mut self, cmd: UserCmd) -> anyhow::Result<()> {
        match cmd {
            UserCmd::Join(net) => {
                if self.session_tx.is_some() {
                    eprintln!("[!] already joined; type 'leave' first");
                    return Ok(());
                }
                self.joining_net = Some(net);
                if self.hello_acked {
                    self.out_tx.send(Frame::Join { net_uuid: net }).await?;
                    println!("[*] join sent, awaiting approval...");
                } else {
                    println!("[*] not yet handshaked; will join after the next HelloAck");
                }
            }
            UserCmd::Leave => {
                if let Some(tx) = self.session_tx.take() {
                    let _ = tx.send(SessionCmd::Kill).await;
                }
                self.joining_net = None;
                self.joined_net = None;
                self.joined_tap_name = None;
                self.joined_tap_ip = None;
                self.cfg.client.joined_network.clear();
                let _ = self.cfg.save(&self.cfg_path).await;
                println!("[*] left");
            }
            UserCmd::SendText(s) => {
                self.out_tx.send(Frame::Text(s)).await?;
            }
            UserCmd::SendFile { name, data } => {
                self.out_tx.send(Frame::File { name, data }).await?;
            }
            UserCmd::Status(reply) => {
                let snap = ClientStatusSnapshot {
                    client_uuid: self.cfg.client.uuid.parse().unwrap_or(Uuid::nil()),
                    connected: self.hello_acked,
                    joined_network: self.joined_net,
                    tap_name: self.joined_tap_name.clone(),
                    tap_ip: self.joined_tap_ip.clone(),
                };
                let _ = reply.send(snap);
            }
            UserCmd::Quit => unreachable!("handled in run loop"),
        }
        Ok(())
    }

    fn on_session_evt(&mut self, evt: SessionEvt) {
        let SessionEvt::Died { reason, .. } = evt;
        info!(?reason, "session ended");
        self.session_tx = None;
        self.joined_net = None;
        self.joined_tap_name = None;
        self.joined_tap_ip = None;
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn net_short(u: &Uuid) -> String {
    u.simple().to_string()[..8].to_owned()
}

/// Save `data` under `dir/name`, picking a `_1`, `_2`, … suffix to avoid
/// clobbering an existing file. Mirrors the Python reference behavior.
async fn save_received_file(
    dir: &str,
    name: &str,
    data: &[u8],
) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let mut path = PathBuf::from(dir).join(name);
    if !path.exists() {
        tokio::fs::write(&path, data).await?;
        return Ok(path);
    }

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut i: u32 = 1;
    loop {
        let candidate = if ext.is_empty() {
            format!("{stem}_{i}")
        } else {
            format!("{stem}_{i}.{ext}")
        };
        path = PathBuf::from(dir).join(candidate);
        if !path.exists() {
            tokio::fs::write(&path, data).await?;
            return Ok(path);
        }
        i += 1;
    }
}
