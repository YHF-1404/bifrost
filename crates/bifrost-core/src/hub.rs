//! `Hub` — single-actor control plane.
//!
//! The hub owns:
//!   * the server config (mutable copy),
//!   * a registry of live `ConnEntry`s (one per accepted TCP connection),
//!   * a registry of live `SessionEntry`s, keyed by `(client_uuid, net_uuid)`,
//!   * pending-approval requests.
//!
//! It does **not** carry data-plane Ethernet frames — those go directly
//! from the connection task into the bound session via the
//! `bind_tx → SessionCmd::EthIn` path that this module wires up at
//! approval time. The hub's `run` loop is therefore a low-rate command
//! dispatcher: registering conns, deciding approve/deny, persisting
//! membership, and propagating `SessionEvt::Died`.
//!
//! See `tests/hub_lifecycle.rs` for a behaviour-level walkthrough.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bifrost_net::{Bridge, Platform};
use bifrost_proto::{Frame, RouteEntry as WireRoute};
use ipnet::IpNet;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::{ApprovedClient, NetRecord, ServerConfig, WireRoute as CfgRoute};
use crate::ids::{ConnId, IdAllocator, SessionId};
use crate::session::{DeathReason, SessionCmd, SessionEvt, SessionTask};

// ─── Public types ─────────────────────────────────────────────────────────

/// Channels the hub uses to talk back to a single connection task.
#[derive(Clone, Debug)]
pub struct ConnLink {
    /// Outbound frames the conn task should write to its socket.
    pub frame_tx: mpsc::Sender<Frame>,
    /// Tells the conn task which session (if any) to forward Ethernet
    /// frames to. `Some(tx)` = bound; `None` = unbound (drop frames).
    pub bind_tx: mpsc::Sender<Option<mpsc::Sender<SessionCmd>>>,
}

/// Snapshot of hub state for `list`-style introspection.
#[derive(Debug, Clone)]
pub struct HubSnapshot {
    pub networks: Vec<NetRecord>,
    pub sessions: Vec<SessionInfo>,
    pub pending: Vec<PendingInfo>,
    pub routes: Vec<CfgRoute>,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub sid: SessionId,
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
    pub tap_name: String,
    pub tap_ip: Option<String>,
    pub bound_conn: Option<ConnId>,
}

#[derive(Debug, Clone)]
pub struct PendingInfo {
    pub sid: SessionId,
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
    pub conn: ConnId,
}

/// Outcome of a [`HubHandle::set_client_ip`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetClientIpResult {
    /// IP was updated in config; `live` indicates whether it was also
    /// pushed to a currently-bound conn (false if the client is offline).
    Ok { client_uuid: Uuid, live: bool },
    /// No approved-clients entry matched the given UUID prefix.
    NotFound,
    /// More than one entry matched — caller must disambiguate.
    Ambiguous(Vec<Uuid>),
    /// The provided string was not a valid IP or CIDR.
    InvalidIp,
}

/// Commands accepted by the hub.
#[derive(Debug)]
pub enum HubCmd {
    // ── conn-task originated ──────────────────────────────────────────
    NewConn {
        addr: String,
        link: ConnLink,
        ack: oneshot::Sender<ConnId>,
    },
    Hello {
        conn: ConnId,
        client_uuid: Uuid,
        version: u16,
    },
    Join {
        conn: ConnId,
        net_uuid: Uuid,
    },
    Disconnect {
        conn: ConnId,
    },

    // ── REPL originated ───────────────────────────────────────────────
    MakeNet {
        name: String,
        ack: oneshot::Sender<Uuid>,
    },
    Approve {
        sid: SessionId,
        ack: oneshot::Sender<bool>,
    },
    Deny {
        sid: SessionId,
        ack: oneshot::Sender<bool>,
    },
    List {
        ack: oneshot::Sender<HubSnapshot>,
    },
    SetClientIp {
        prefix: String,
        ip: String,
        ack: oneshot::Sender<SetClientIpResult>,
    },
    RouteAdd {
        dst: String,
        via: String,
        ack: oneshot::Sender<Result<(), String>>,
    },
    RouteDel {
        dst: String,
        ack: oneshot::Sender<bool>,
    },
    /// Push the current route table to every bound conn. Returns the
    /// number of clients reached.
    RoutePush {
        ack: oneshot::Sender<usize>,
    },
    BroadcastText {
        msg: String,
        ack: oneshot::Sender<usize>,
    },
    BroadcastFile {
        name: String,
        data: Vec<u8>,
        ack: oneshot::Sender<usize>,
    },

    Shutdown,
}

/// Cheaply-cloneable handle to a running [`Hub`].
#[derive(Clone)]
pub struct HubHandle {
    cmd_tx: mpsc::Sender<HubCmd>,
}

impl HubHandle {
    /// Register a freshly-accepted TCP connection. Returns the assigned
    /// [`ConnId`] which the conn task uses for all subsequent commands.
    pub async fn register_conn(&self, addr: String, link: ConnLink) -> Option<ConnId> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(HubCmd::NewConn {
                addr,
                link,
                ack: tx,
            })
            .await
            .ok()?;
        rx.await.ok()
    }

    pub async fn hello(&self, conn: ConnId, client_uuid: Uuid, version: u16) {
        let _ = self
            .cmd_tx
            .send(HubCmd::Hello {
                conn,
                client_uuid,
                version,
            })
            .await;
    }

    pub async fn join(&self, conn: ConnId, net_uuid: Uuid) {
        let _ = self
            .cmd_tx
            .send(HubCmd::Join { conn, net_uuid })
            .await;
    }

    pub async fn disconnect(&self, conn: ConnId) {
        let _ = self.cmd_tx.send(HubCmd::Disconnect { conn }).await;
    }

    pub async fn make_net(&self, name: String) -> Option<Uuid> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(HubCmd::MakeNet { name, ack: tx })
            .await
            .ok()?;
        rx.await.ok()
    }

    pub async fn approve(&self, sid: SessionId) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::Approve { sid, ack: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub async fn deny(&self, sid: SessionId) -> bool {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(HubCmd::Deny { sid, ack: tx }).await.is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub async fn list(&self) -> Option<HubSnapshot> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(HubCmd::List { ack: tx }).await.ok()?;
        rx.await.ok()
    }

    /// Set the persisted TAP IP for a client matched by UUID prefix.
    ///
    /// If the client is currently bound to a conn, also pushes a
    /// `Frame::SetIp` so the new address takes effect immediately.
    pub async fn set_client_ip(&self, prefix: String, ip: String) -> SetClientIpResult {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::SetClientIp {
                prefix,
                ip,
                ack: tx,
            })
            .await
            .is_err()
        {
            return SetClientIpResult::NotFound;
        }
        rx.await.unwrap_or(SetClientIpResult::NotFound)
    }

    pub async fn route_add(&self, dst: String, via: String) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::RouteAdd { dst, via, ack: tx })
            .await
            .is_err()
        {
            return Err("hub gone".into());
        }
        rx.await.unwrap_or_else(|_| Err("hub dropped reply".into()))
    }

    pub async fn route_del(&self, dst: String) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::RouteDel { dst, ack: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub async fn route_push(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::RoutePush { ack: tx })
            .await
            .is_err()
        {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    pub async fn broadcast_text(&self, msg: String) -> usize {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::BroadcastText { msg, ack: tx })
            .await
            .is_err()
        {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    pub async fn broadcast_file(&self, name: String, data: Vec<u8>) -> usize {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::BroadcastFile {
                name,
                data,
                ack: tx,
            })
            .await
            .is_err()
        {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(HubCmd::Shutdown).await;
    }
}

// ─── Internal bookkeeping ────────────────────────────────────────────────

struct ConnEntry {
    addr: String,
    link: ConnLink,
    /// Set after a successful Hello.
    client_uuid: Option<Uuid>,
    /// Set while bound to a session. Cleared on disconnect or session death.
    bound_session: Option<SessionId>,
}

struct SessionEntry {
    sid: SessionId,
    client_uuid: Uuid,
    net_uuid: Uuid,
    cmd_tx: mpsc::Sender<SessionCmd>,
    tap_name: String,
    tap_ip: Option<String>,
    /// Conn currently forwarding into this session.
    bound_conn: Option<ConnId>,
}

struct PendingApproval {
    sid: SessionId,
    client_uuid: Uuid,
    net_uuid: Uuid,
    conn: ConnId,
}

// ─── Hub ─────────────────────────────────────────────────────────────────

pub struct Hub {
    cfg: ServerConfig,
    /// Optional path the hub persists to after every config-mutating
    /// command. `None` is used in tests.
    cfg_path: Option<PathBuf>,

    platform: Arc<dyn Platform>,
    bridge: Arc<dyn Bridge>,

    conns: HashMap<ConnId, ConnEntry>,
    sessions: HashMap<(Uuid, Uuid), SessionEntry>,
    sessions_by_id: HashMap<SessionId, (Uuid, Uuid)>,
    pending: HashMap<SessionId, PendingApproval>,

    ids: IdAllocator,

    cmd_rx: mpsc::Receiver<HubCmd>,
    evt_tx: mpsc::Sender<SessionEvt>,
    evt_rx: mpsc::Receiver<SessionEvt>,

    disconnect_timeout: Duration,
}

impl Hub {
    pub fn new(
        cfg: ServerConfig,
        cfg_path: Option<PathBuf>,
        platform: Arc<dyn Platform>,
        bridge: Arc<dyn Bridge>,
    ) -> (Self, HubHandle) {
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (evt_tx, evt_rx) = mpsc::channel(64);
        let disconnect_timeout = Duration::from_secs(cfg.bridge.disconnect_timeout);
        let hub = Self {
            cfg,
            cfg_path,
            platform,
            bridge,
            conns: HashMap::new(),
            sessions: HashMap::new(),
            sessions_by_id: HashMap::new(),
            pending: HashMap::new(),
            ids: IdAllocator::starting_at(1),
            cmd_rx,
            evt_tx,
            evt_rx,
            disconnect_timeout,
        };
        (hub, HubHandle { cmd_tx })
    }

    /// Atomically write the in-memory config to disk, if a path is set.
    /// Failures are logged but never propagated — REPL responsiveness
    /// always comes first.
    async fn persist(&self) {
        if let Some(p) = &self.cfg_path {
            if let Err(e) = self.cfg.save(p).await {
                warn!(error = %e, "config save failed");
            }
        }
    }

    /// Replay the configured route table into the host's kernel
    /// routing table via the bridge. Without this, hosts behind a
    /// client (e.g. a LAN reachable through `via 10.0.0.2`) cannot be
    /// reached *from* the server side: the kernel has no idea those
    /// destinations live behind the bridge.
    ///
    /// Bad rows are dropped silently — `RouteEntry::parse` enforces
    /// validity at `route add` time, so this is just defensive.
    async fn sync_local_routes(&self) {
        let parsed: Vec<bifrost_net::RouteEntry> = self
            .cfg
            .routes
            .iter()
            .filter_map(|r| bifrost_net::RouteEntry::parse(&r.dst, &r.via).ok())
            .collect();
        if let Err(e) = self.bridge.apply_routes(&parsed).await {
            warn!(error = %e, "bridge.apply_routes failed");
        }
    }

    pub async fn run(mut self) {
        info!(
            bridge = self.bridge.name(),
            disconnect_timeout_s = self.disconnect_timeout.as_secs(),
            "hub start"
        );

        // Replay any persisted routes into the kernel — covers the
        // "daemon restart" path. Without this, traffic from the
        // server toward LANs behind a client would 'no route to host'
        // until someone re-ran `route push`.
        self.sync_local_routes().await;

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(HubCmd::Shutdown) => break,
                    Some(c) => self.handle_cmd(c).await,
                    None => break,
                },
                Some(evt) = self.evt_rx.recv() => match evt {
                    SessionEvt::Died { sid, reason } => self.handle_session_died(sid, reason).await,
                }
            }
        }

        self.shutdown_all().await;
        info!("hub stop");
    }

    // ── Top-level dispatch ───────────────────────────────────────────

    async fn handle_cmd(&mut self, cmd: HubCmd) {
        match cmd {
            HubCmd::NewConn { addr, link, ack } => self.handle_new_conn(addr, link, ack),
            HubCmd::Hello {
                conn,
                client_uuid,
                version,
            } => self.handle_hello(conn, client_uuid, version),
            HubCmd::Join { conn, net_uuid } => self.handle_join(conn, net_uuid).await,
            HubCmd::Disconnect { conn } => self.handle_disconnect(conn).await,
            HubCmd::MakeNet { name, ack } => self.handle_make_net(name, ack).await,
            HubCmd::Approve { sid, ack } => self.handle_approve(sid, ack).await,
            HubCmd::Deny { sid, ack } => self.handle_deny(sid, ack).await,
            HubCmd::List { ack } => self.handle_list(ack),
            HubCmd::SetClientIp { prefix, ip, ack } => {
                self.handle_set_client_ip(prefix, ip, ack).await
            }
            HubCmd::RouteAdd { dst, via, ack } => self.handle_route_add(dst, via, ack).await,
            HubCmd::RouteDel { dst, ack } => self.handle_route_del(dst, ack).await,
            HubCmd::RoutePush { ack } => self.handle_route_push(ack).await,
            HubCmd::BroadcastText { msg, ack } => self.handle_broadcast_text(msg, ack).await,
            HubCmd::BroadcastFile { name, data, ack } => {
                self.handle_broadcast_file(name, data, ack).await
            }
            HubCmd::Shutdown => unreachable!("Shutdown handled in run()"),
        }
    }

    // ── Conn lifecycle ───────────────────────────────────────────────

    fn handle_new_conn(&mut self, addr: String, link: ConnLink, ack: oneshot::Sender<ConnId>) {
        let id = self.ids.next_conn();
        debug!(?id, %addr, "new conn");
        self.conns.insert(
            id,
            ConnEntry {
                addr,
                link,
                client_uuid: None,
                bound_session: None,
            },
        );
        let _ = ack.send(id);
    }

    fn handle_hello(&mut self, conn: ConnId, client_uuid: Uuid, version: u16) {
        if version != bifrost_proto::PROTOCOL_VERSION {
            warn!(?conn, version, "unsupported version");
            // Future: send a JoinDeny-like rejection. For v1 we're permissive.
        }
        if let Some(entry) = self.conns.get_mut(&conn) {
            entry.client_uuid = Some(client_uuid);
            debug!(?conn, %client_uuid, "hello accepted");
        }
    }

    async fn handle_disconnect(&mut self, conn: ConnId) {
        let Some(entry) = self.conns.remove(&conn) else {
            return;
        };
        debug!(?conn, addr = %entry.addr, "disconnect");

        if let Some(sid) = entry.bound_session {
            if let Some(&key) = self.sessions_by_id.get(&sid) {
                if let Some(session) = self.sessions.get_mut(&key) {
                    if session.bound_conn == Some(conn) {
                        session.bound_conn = None;
                    }
                    let _ = session.cmd_tx.send(SessionCmd::UnbindConn).await;
                }
            }
        }

        // Drop any pending approvals tied to this conn.
        self.pending.retain(|_, p| p.conn != conn);
    }

    // ── Join flow ────────────────────────────────────────────────────

    async fn handle_join(&mut self, conn: ConnId, net_uuid: Uuid) {
        let (frame_tx, client_uuid) = {
            let Some(entry) = self.conns.get(&conn) else {
                return;
            };
            let Some(client_uuid) = entry.client_uuid else {
                let _ = entry
                    .link
                    .frame_tx
                    .send(Frame::JoinDeny {
                        reason: "no_hello".into(),
                    })
                    .await;
                return;
            };
            (entry.link.frame_tx.clone(), client_uuid)
        };

        if !self.cfg.networks.iter().any(|n| n.uuid == net_uuid) {
            let _ = frame_tx
                .send(Frame::JoinDeny {
                    reason: "unknown_network".into(),
                })
                .await;
            return;
        }

        // ── Reconnect path ────────────────────────────────────────
        let key = (client_uuid, net_uuid);
        if let Some(session) = self.sessions.get_mut(&key) {
            // Tell the previously-bound conn (if any) to unbind first.
            if let Some(old_conn) = session.bound_conn.replace(conn) {
                if let Some(old) = self.conns.get_mut(&old_conn) {
                    old.bound_session = None;
                    let _ = old.link.bind_tx.send(None).await;
                }
            }

            let cmd_tx = session.cmd_tx.clone();
            let suffix = strip_tap_prefix(&session.tap_name);
            let tap_ip = session.tap_ip.clone();
            let _ = cmd_tx.send(SessionCmd::BindConn(frame_tx.clone())).await;

            if let Some(c) = self.conns.get_mut(&conn) {
                c.bound_session = Some(session.sid);
                let _ = c.link.bind_tx.send(Some(cmd_tx)).await;
            }

            let _ = frame_tx
                .send(Frame::JoinOk {
                    tap_suffix: suffix,
                    ip: tap_ip.clone(),
                })
                .await;
            self.send_routes(&frame_tx, tap_ip.as_deref()).await;
            return;
        }

        // ── New session ───────────────────────────────────────────
        let sid = self.ids.next_session();
        let approved = self
            .cfg
            .approved_clients
            .iter()
            .any(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid);

        if approved {
            self.do_approve(sid, conn, client_uuid, net_uuid).await;
        } else {
            self.pending.insert(
                sid,
                PendingApproval {
                    sid,
                    client_uuid,
                    net_uuid,
                    conn,
                },
            );
            info!(?sid, %client_uuid, %net_uuid, "join awaiting approval");
        }
    }

    async fn do_approve(
        &mut self,
        sid: SessionId,
        conn: ConnId,
        client_uuid: Uuid,
        net_uuid: Uuid,
    ) {
        let frame_tx = match self.conns.get(&conn) {
            Some(c) => c.link.frame_tx.clone(),
            None => {
                warn!(?sid, ?conn, "conn dropped before approval");
                return;
            }
        };

        let tap_name = tap_name_from_uuid(&client_uuid);
        let approved_entry = self
            .cfg
            .approved_clients
            .iter()
            .find(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid)
            .cloned();
        let tap_ip_str = approved_entry
            .as_ref()
            .map(|a| a.tap_ip.clone())
            .filter(|s| !s.is_empty());

        // Server-side TAPs are L2 bridge ports — they MUST NOT carry
        // an IP. The bridge interface (`br-bifrost`) holds the gateway
        // address; this TAP only forwards Ethernet frames between the
        // socket and the bridge. The configured `tap_ip` belongs to
        // the *client*: it travels in `JoinOk` (and via `SetIp`) and
        // is applied on the client's local TAP, not here.
        let tap = match self.platform.create_tap(&tap_name, None).await {
            Ok(t) => t,
            Err(e) => {
                error!(error = %e, "create_tap failed");
                let _ = frame_tx
                    .send(Frame::JoinDeny {
                        reason: format!("server_error:{e}"),
                    })
                    .await;
                return;
            }
        };

        if let Err(e) = self.bridge.add_tap(&tap_name).await {
            error!(error = %e, "bridge.add_tap failed");
            let _ = tap.destroy().await;
            let _ = frame_tx
                .send(Frame::JoinDeny {
                    reason: format!("bridge_error:{e}"),
                })
                .await;
            return;
        }

        let (sess_cmd_tx, sess_cmd_rx) = mpsc::channel(64);
        let task = SessionTask::new(
            sid,
            client_uuid,
            net_uuid,
            tap,
            sess_cmd_rx,
            self.evt_tx.clone(),
            Some(self.disconnect_timeout),
        );
        tokio::spawn(task.run(frame_tx.clone()));

        self.sessions.insert(
            (client_uuid, net_uuid),
            SessionEntry {
                sid,
                client_uuid,
                net_uuid,
                cmd_tx: sess_cmd_tx.clone(),
                tap_name: tap_name.clone(),
                tap_ip: tap_ip_str.clone(),
                bound_conn: Some(conn),
            },
        );
        self.sessions_by_id.insert(sid, (client_uuid, net_uuid));

        if let Some(c) = self.conns.get_mut(&conn) {
            c.bound_session = Some(sid);
            let _ = c.link.bind_tx.send(Some(sess_cmd_tx)).await;
        }

        // Persist whitelist entry so subsequent reconnects auto-approve.
        if approved_entry.is_none() {
            self.cfg.approved_clients.push(ApprovedClient {
                client_uuid,
                net_uuid,
                tap_ip: String::new(),
            });
            self.persist().await;
        }

        let _ = frame_tx
            .send(Frame::JoinOk {
                tap_suffix: strip_tap_prefix(&tap_name),
                ip: tap_ip_str.clone(),
            })
            .await;
        self.send_routes(&frame_tx, tap_ip_str.as_deref()).await;

        info!(?sid, ?conn, %client_uuid, tap = %tap_name, "session joined");
    }

    /// Push the configured route table to a freshly-bound conn, omitting
    /// any route whose `via` equals the conn's own TAP IP (would loop).
    async fn send_routes(&self, frame_tx: &mpsc::Sender<Frame>, tap_ip: Option<&str>) {
        if self.cfg.routes.is_empty() {
            return;
        }
        let host = tap_ip
            .and_then(|s| s.split('/').next())
            .filter(|s| !s.is_empty());
        let routes: Vec<WireRoute> = self
            .cfg
            .routes
            .iter()
            .filter(|r| host.is_none_or(|h| r.via != h))
            .map(|r| WireRoute {
                dst: r.dst.clone(),
                via: r.via.clone(),
            })
            .collect();
        if !routes.is_empty() {
            let _ = frame_tx.send(Frame::SetRoutes(routes)).await;
        }
    }

    // ── REPL handlers ────────────────────────────────────────────────

    async fn handle_make_net(&mut self, name: String, ack: oneshot::Sender<Uuid>) {
        let uuid = Uuid::new_v4();
        self.cfg.networks.push(NetRecord {
            name: name.clone(),
            uuid,
        });
        self.persist().await;
        info!(%name, %uuid, "network created");
        let _ = ack.send(uuid);
    }

    async fn handle_set_client_ip(
        &mut self,
        prefix: String,
        ip: String,
        ack: oneshot::Sender<SetClientIpResult>,
    ) {
        // Reject syntactically-invalid IP/CIDR — empty string allowed (= clear).
        if !ip.is_empty()
            && ip.parse::<IpNet>().is_err()
            && ip.parse::<std::net::IpAddr>().is_err()
        {
            let _ = ack.send(SetClientIpResult::InvalidIp);
            return;
        }

        let matches: Vec<usize> = self
            .cfg
            .approved_clients
            .iter()
            .enumerate()
            .filter(|(_, a)| a.client_uuid.simple().to_string().starts_with(&prefix))
            .map(|(i, _)| i)
            .collect();
        match matches.len() {
            0 => {
                let _ = ack.send(SetClientIpResult::NotFound);
                return;
            }
            1 => {}
            _ => {
                let uuids = matches
                    .iter()
                    .map(|i| self.cfg.approved_clients[*i].client_uuid)
                    .collect();
                let _ = ack.send(SetClientIpResult::Ambiguous(uuids));
                return;
            }
        }
        let idx = matches[0];
        let client_uuid = self.cfg.approved_clients[idx].client_uuid;
        let net_uuid = self.cfg.approved_clients[idx].net_uuid;
        self.cfg.approved_clients[idx].tap_ip = ip.clone();
        self.persist().await;

        // Try to push to a live, bound conn.
        let live = if let Some(s) = self.sessions.get_mut(&(client_uuid, net_uuid)) {
            s.tap_ip = if ip.is_empty() { None } else { Some(ip.clone()) };
            if let Some(conn_id) = s.bound_conn {
                if let Some(c) = self.conns.get(&conn_id) {
                    let payload = if ip.is_empty() { None } else { Some(ip) };
                    c.link
                        .frame_tx
                        .send(Frame::SetIp { ip: payload })
                        .await
                        .is_ok()
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        let _ = ack.send(SetClientIpResult::Ok { client_uuid, live });
    }

    async fn handle_route_add(
        &mut self,
        dst: String,
        via: String,
        ack: oneshot::Sender<Result<(), String>>,
    ) {
        if let Err(e) = bifrost_net::RouteEntry::parse(&dst, &via) {
            let _ = ack.send(Err(e.to_string()));
            return;
        }
        if self.cfg.routes.iter().any(|r| r.dst == dst) {
            let _ = ack.send(Err(format!("route to {dst} already exists")));
            return;
        }
        self.cfg.routes.push(CfgRoute {
            dst: dst.clone(),
            via: via.clone(),
        });
        self.persist().await;
        self.sync_local_routes().await;
        info!(%dst, %via, "route added");
        let _ = ack.send(Ok(()));
    }

    async fn handle_route_del(&mut self, dst: String, ack: oneshot::Sender<bool>) {
        let before = self.cfg.routes.len();
        self.cfg.routes.retain(|r| r.dst != dst);
        let removed = self.cfg.routes.len() < before;
        if removed {
            self.persist().await;
            self.sync_local_routes().await;
            info!(%dst, "route removed");
        }
        let _ = ack.send(removed);
    }

    async fn handle_route_push(&mut self, ack: oneshot::Sender<usize>) {
        // Snapshot the (conn_id, tap_ip) pairs so we don't hold a
        // borrow on `self.sessions` across the awaits below.
        let bound: Vec<(ConnId, Option<String>)> = self
            .sessions
            .values()
            .filter_map(|s| s.bound_conn.map(|c| (c, s.tap_ip.clone())))
            .collect();
        let mut count = 0;
        for (conn_id, tap_ip) in bound {
            let frame_tx = self.conns.get(&conn_id).map(|c| c.link.frame_tx.clone());
            if let Some(tx) = frame_tx {
                self.send_routes(&tx, tap_ip.as_deref()).await;
                count += 1;
            }
        }
        let _ = ack.send(count);
    }

    async fn handle_broadcast_text(&self, msg: String, ack: oneshot::Sender<usize>) {
        let mut count = 0;
        for c in self.conns.values() {
            if c.link
                .frame_tx
                .send(Frame::Text(msg.clone()))
                .await
                .is_ok()
            {
                count += 1;
            }
        }
        let _ = ack.send(count);
    }

    async fn handle_broadcast_file(
        &self,
        name: String,
        data: Vec<u8>,
        ack: oneshot::Sender<usize>,
    ) {
        let mut count = 0;
        for c in self.conns.values() {
            let frame = Frame::File {
                name: name.clone(),
                data: data.clone(),
            };
            if c.link.frame_tx.send(frame).await.is_ok() {
                count += 1;
            }
        }
        let _ = ack.send(count);
    }

    async fn handle_approve(&mut self, sid: SessionId, ack: oneshot::Sender<bool>) {
        let Some(p) = self.pending.remove(&sid) else {
            let _ = ack.send(false);
            return;
        };
        self.do_approve(p.sid, p.conn, p.client_uuid, p.net_uuid).await;
        let _ = ack.send(true);
    }

    async fn handle_deny(&mut self, sid: SessionId, ack: oneshot::Sender<bool>) {
        let Some(p) = self.pending.remove(&sid) else {
            let _ = ack.send(false);
            return;
        };
        if let Some(c) = self.conns.get(&p.conn) {
            let _ = c
                .link
                .frame_tx
                .send(Frame::JoinDeny {
                    reason: "denied_by_admin".into(),
                })
                .await;
        }
        let _ = ack.send(true);
    }

    fn handle_list(&self, ack: oneshot::Sender<HubSnapshot>) {
        let snap = HubSnapshot {
            networks: self.cfg.networks.clone(),
            sessions: self
                .sessions
                .values()
                .map(|s| SessionInfo {
                    sid: s.sid,
                    client_uuid: s.client_uuid,
                    net_uuid: s.net_uuid,
                    tap_name: s.tap_name.clone(),
                    tap_ip: s.tap_ip.clone(),
                    bound_conn: s.bound_conn,
                })
                .collect(),
            pending: self
                .pending
                .values()
                .map(|p| PendingInfo {
                    sid: p.sid,
                    client_uuid: p.client_uuid,
                    net_uuid: p.net_uuid,
                    conn: p.conn,
                })
                .collect(),
            routes: self.cfg.routes.clone(),
        };
        let _ = ack.send(snap);
    }

    // ── Session lifecycle ────────────────────────────────────────────

    async fn handle_session_died(&mut self, sid: SessionId, reason: DeathReason) {
        debug!(?sid, ?reason, "session died");
        let Some(key) = self.sessions_by_id.remove(&sid) else {
            return;
        };
        let Some(entry) = self.sessions.remove(&key) else {
            return;
        };

        if let Some(conn_id) = entry.bound_conn {
            if let Some(c) = self.conns.get_mut(&conn_id) {
                if c.bound_session == Some(sid) {
                    c.bound_session = None;
                }
                let _ = c.link.bind_tx.send(None).await;
            }
        }
        let _ = self.bridge.remove_tap(&entry.tap_name).await;
    }

    async fn shutdown_all(&mut self) {
        let kills: Vec<_> = self.sessions.values().map(|s| s.cmd_tx.clone()).collect();
        for tx in &kills {
            let _ = tx.send(SessionCmd::Kill).await;
        }
        // Drain death events with a global timeout so a stuck session
        // can't hang shutdown forever.
        let n = kills.len();
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..n {
                if self.evt_rx.recv().await.is_none() {
                    break;
                }
            }
        })
        .await;
        let _ = self.bridge.destroy().await;
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────

fn tap_name_from_uuid(client_uuid: &Uuid) -> String {
    let s = client_uuid.simple().to_string();
    format!("tap{}", &s[..8])
}

fn strip_tap_prefix(name: &str) -> String {
    name.strip_prefix("tap").unwrap_or(name).to_string()
}
