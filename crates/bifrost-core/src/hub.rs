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
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bifrost_net::{Bridge, Platform};
use bifrost_proto::admin::DeviceEntry;
use bifrost_proto::{Frame, RouteEntry as WireRoute};
use ipnet::IpNet;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::{ApprovedClient, NetRecord, ServerConfig};
use crate::events::{HubEvent, MetricsSample, RouteRow};
use crate::ids::{ConnId, IdAllocator, SessionId};
use crate::routes::{derive_routes_for_network, filter_for_peer};
use crate::session::{DeathReason, SessionCmd, SessionEvt, SessionTask};

/// 1 Hz sampling cadence. Subscribers (the WebUI) plot deltas; this
/// constant doubles as the divisor when bps_in / bps_out are being
/// interpreted as "bytes per second" downstream.
const METRICS_TICK: Duration = Duration::from_secs(1);

/// Buffer depth for the events broadcast. A slow subscriber (e.g. a
/// browser tab in the background) drops oldest events instead of
/// stalling the Hub.
const EVENTS_CAPACITY: usize = 256;

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

/// Snapshot of hub state for `list`-style introspection. As of v0.1
/// the snapshot no longer carries a global routes table — routes are
/// derived per-network on demand via `device_push`.
#[derive(Debug, Clone)]
pub struct HubSnapshot {
    pub networks: Vec<NetRecord>,
    pub sessions: Vec<SessionInfo>,
    pub pending: Vec<PendingInfo>,
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
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
    pub conn: ConnId,
}

/// Field-level update bag for [`HubHandle::device_set`].
///
/// Every field is `Option<_>`: `None` means "leave unchanged", `Some`
/// means "replace with this value". An empty string in `tap_ip` clears
/// the IP; an empty `Vec` in `lan_subnets` clears the list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceUpdate {
    pub name: Option<String>,
    pub admitted: Option<bool>,
    pub tap_ip: Option<String>,
    pub lan_subnets: Option<Vec<String>>,
}

/// Outcome of a [`HubHandle::device_set`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSetResult {
    /// Update applied; full updated record returned.
    Ok(DeviceEntry),
    /// No approved client matches `(client_uuid, net_uuid)`. The
    /// hub creates the row only when at least one of `admitted=true`
    /// or `tap_ip=Some(non-empty)` is provided; otherwise this is
    /// returned.
    NotFound,
    /// `tap_ip` is syntactically invalid.
    InvalidIp,
    /// `tap_ip` collides with another device in the same network.
    Conflict { msg: String },
}

/// Outcome of a [`HubHandle::device_push`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePushResult {
    /// Routes that were derived and applied locally / sent to peers.
    pub routes: Vec<WireRoute>,
    /// Number of currently-bound peers that received the push.
    pub count: u64,
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
    List {
        ack: oneshot::Sender<HubSnapshot>,
    },
    /// List all devices, optionally filtered by network.
    DeviceList {
        net_uuid: Option<Uuid>,
        ack: oneshot::Sender<Vec<DeviceEntry>>,
    },
    /// Mutate one approved-client row. See [`DeviceUpdate`].
    DeviceSet {
        client_uuid: Uuid,
        net_uuid: Uuid,
        update: DeviceUpdate,
        ack: oneshot::Sender<DeviceSetResult>,
    },
    /// Re-derive routes for `net_uuid`, install them locally on the
    /// bridge, and push to every bound conn in that network.
    DevicePush {
        net_uuid: Uuid,
        ack: oneshot::Sender<DevicePushResult>,
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
    events: broadcast::Sender<HubEvent>,
}

impl HubHandle {
    /// Subscribe to the Hub's broadcast event stream. New subscribers
    /// only see events emitted from this point forward — there is no
    /// backfill. Lagging subscribers receive
    /// `RecvError::Lagged(n)` and should treat that as "skip n events,
    /// keep going."
    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }
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

    pub async fn list(&self) -> Option<HubSnapshot> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(HubCmd::List { ack: tx }).await.ok()?;
        rx.await.ok()
    }

    /// List devices, optionally filtered by network.
    pub async fn device_list(&self, net_uuid: Option<Uuid>) -> Vec<DeviceEntry> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::DeviceList { net_uuid, ack: tx })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Mutate one approved-client row. The row is identified by
    /// `(client_uuid, net_uuid)` — the WebUI knows both because every
    /// device URL is path-scoped under a network. See [`DeviceUpdate`].
    pub async fn device_set(
        &self,
        client_uuid: Uuid,
        net_uuid: Uuid,
        update: DeviceUpdate,
    ) -> DeviceSetResult {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::DeviceSet {
                client_uuid,
                net_uuid,
                update,
                ack: tx,
            })
            .await
            .is_err()
        {
            return DeviceSetResult::NotFound;
        }
        rx.await.unwrap_or(DeviceSetResult::NotFound)
    }

    /// Re-derive and push routes for one network.
    pub async fn device_push(&self, net_uuid: Uuid) -> DevicePushResult {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::DevicePush { net_uuid, ack: tx })
            .await
            .is_err()
        {
            return DevicePushResult {
                routes: Vec::new(),
                count: 0,
            };
        }
        rx.await.unwrap_or(DevicePushResult {
            routes: Vec::new(),
            count: 0,
        })
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
    /// Live byte counters shared with the matching `SessionTask`.
    /// Sampled by the Hub's metrics tick every `METRICS_TICK`.
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
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
    /// Conns that sent `Join` for a `(client, net)` row whose
    /// `admitted=false`. The conn is held open silently; flipping the
    /// switch to `admitted=true` promotes it into a real session.
    pending: HashMap<(Uuid, Uuid), ConnId>,

    ids: IdAllocator,

    cmd_rx: mpsc::Receiver<HubCmd>,
    evt_tx: mpsc::Sender<SessionEvt>,
    evt_rx: mpsc::Receiver<SessionEvt>,

    disconnect_timeout: Duration,

    /// Broadcast channel for pushed events (metrics ticks today,
    /// device.* events in 1.3). Held in `Arc`-style `Sender` form;
    /// receivers are minted via `Sender::subscribe`.
    events_tx: broadcast::Sender<HubEvent>,
    /// Previous (in, out) byte counters per session, for delta
    /// computation in the metrics tick.
    metrics_prev: HashMap<SessionId, (u64, u64)>,
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
        let (events_tx, _) = broadcast::channel(EVENTS_CAPACITY);
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
            events_tx: events_tx.clone(),
            metrics_prev: HashMap::new(),
        };
        (
            hub,
            HubHandle {
                cmd_tx,
                events: events_tx,
            },
        )
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

    /// Re-install the host's kernel routes from every network's
    /// derived table.
    ///
    /// Without this, hosts behind a client (e.g. a LAN reachable
    /// through `via 10.0.0.2`) cannot be reached *from* the server
    /// side: the kernel has no idea those destinations live behind
    /// the bridge. We rebuild the full table from
    /// [`derive_routes_for_network`] and shove it through
    /// `Bridge::apply_routes`, which uses a flush-and-reapply strategy.
    ///
    /// Failures are logged and swallowed — daemon startup must not
    /// abort just because rtnetlink hiccupped.
    async fn sync_local_routes(&self) {
        let mut all = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for net in &self.cfg.networks {
            for r in derive_routes_for_network(&self.cfg, net.uuid) {
                if !seen.insert(r.dst.clone()) {
                    // Cross-network dst collision; skip the later one.
                    continue;
                }
                all.push(r);
            }
        }
        let parsed: Vec<bifrost_net::RouteEntry> = all
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

        let mut sampler = tokio::time::interval(METRICS_TICK);
        // Hub is the bottleneck for these — if we ever fall behind
        // (very long config save?), skip the missed beats rather than
        // burst-emit a backlog the WS subscribers don't want.
        sampler.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(HubCmd::Shutdown) => break,
                    Some(c) => self.handle_cmd(c).await,
                    None => break,
                },
                Some(evt) = self.evt_rx.recv() => match evt {
                    SessionEvt::Died { sid, reason } => self.handle_session_died(sid, reason).await,
                },
                _ = sampler.tick() => self.emit_metrics_tick(),
            }
        }

        self.shutdown_all().await;
        info!("hub stop");
    }

    /// Best-effort send. `broadcast::send` errors only when there are
    /// zero receivers — the common case (no WebUI tab open) — and is
    /// silently fine. Centralising the call keeps emission sites tidy.
    fn emit(&self, evt: HubEvent) {
        let _ = self.events_tx.send(evt);
    }

    /// Snapshot every joined session's byte counters, compute deltas
    /// against the previous tick, and broadcast a `MetricsTick` event.
    /// Skips emission when there are no sessions (no subscribers want
    /// empty arrays).
    fn emit_metrics_tick(&mut self) {
        if self.sessions.is_empty() {
            // Drop stale prev entries so a re-joining client starts
            // from zero-delta on first emit.
            self.metrics_prev.clear();
            return;
        }
        let mut samples = Vec::with_capacity(self.sessions.len());
        let mut alive = std::collections::HashSet::with_capacity(self.sessions.len());
        for s in self.sessions.values() {
            alive.insert(s.sid);
            let in_now = s.bytes_in.load(Ordering::Relaxed);
            let out_now = s.bytes_out.load(Ordering::Relaxed);
            let (in_prev, out_prev) = self
                .metrics_prev
                .get(&s.sid)
                .copied()
                .unwrap_or((in_now, out_now));
            samples.push(MetricsSample {
                network: s.net_uuid,
                client_uuid: s.client_uuid,
                bps_in: in_now.saturating_sub(in_prev),
                bps_out: out_now.saturating_sub(out_prev),
                total_in: in_now,
                total_out: out_now,
            });
            self.metrics_prev.insert(s.sid, (in_now, out_now));
        }
        // GC dead sessions out of the prev map.
        self.metrics_prev.retain(|sid, _| alive.contains(sid));

        self.emit(HubEvent::MetricsTick { samples });
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
            HubCmd::List { ack } => self.handle_list(ack),
            HubCmd::DeviceList { net_uuid, ack } => self.handle_device_list(net_uuid, ack),
            HubCmd::DeviceSet {
                client_uuid,
                net_uuid,
                update,
                ack,
            } => self.handle_device_set(client_uuid, net_uuid, update, ack).await,
            HubCmd::DevicePush { net_uuid, ack } => self.handle_device_push(net_uuid, ack).await,
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

        // If this conn was holding a pending row, release it. The row
        // itself stays — the device just goes "offline + pending" in
        // the UI until the client reconnects.
        let dropped: Vec<(Uuid, Uuid)> = self
            .pending
            .iter()
            .filter(|(_, c)| **c == conn)
            .map(|(k, _)| *k)
            .collect();
        for k in dropped {
            self.pending.remove(&k);
            self.emit(HubEvent::DeviceOffline {
                network: k.1,
                client_uuid: k.0,
            });
        }
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
            let sid = session.sid.0;
            let tap_name = session.tap_name.clone();
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
            self.send_routes(&frame_tx, net_uuid, tap_ip.as_deref())
                .await;
            // Reconnect re-binds an existing SessionTask. From the
            // WebUI's "online == has-a-session" perspective the
            // device may already have been online (during the brief
            // unbound window the session is still alive), but a fresh
            // DeviceOnline is the cleanest signal that the device
            // changed state — subscribers can treat it as idempotent.
            self.emit(HubEvent::DeviceOnline {
                network: net_uuid,
                client_uuid,
                sid,
                tap_name,
            });
            return;
        }

        // ── New (or returning) join ───────────────────────────────
        // Look for an existing row, or create one with admitted=false
        // so the device is now visible in the device list (in pending
        // state) even before any admin action.
        let key = (client_uuid, net_uuid);
        let row_idx = self
            .cfg
            .approved_clients
            .iter()
            .position(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid);

        let (admitted, fresh_row) = match row_idx {
            Some(i) => (self.cfg.approved_clients[i].admitted, false),
            None => {
                self.cfg.approved_clients.push(ApprovedClient {
                    client_uuid,
                    net_uuid,
                    tap_ip: String::new(),
                    display_name: String::new(),
                    lan_subnets: Vec::new(),
                    admitted: false,
                });
                self.persist().await;
                (false, true)
            }
        };

        if admitted {
            // Row says auto-admit. Spin up a real session.
            let sid = self.ids.next_session();
            self.do_approve(sid, conn, client_uuid, net_uuid).await;
            return;
        }

        // Row says pending. Hold the conn quietly. NO JoinDeny — the
        // client is meant to be left waiting until an admin flips
        // admitted to true (or until it disconnects on its own).
        let was_already_pending = self.pending.contains_key(&key);
        self.pending.insert(key, conn);
        info!(?conn, %client_uuid, %net_uuid, "join awaiting admission");

        // Emit DevicePending only when the row is genuinely new to the
        // WebUI — either freshly created here, or it existed but had no
        // pending conn before. (Reconnect of a known-pending device:
        // emit DeviceChanged instead so the existing row's online
        // state flips back to true.)
        if fresh_row || !was_already_pending {
            let device = DeviceEntry {
                client_uuid,
                net_uuid,
                display_name: self
                    .cfg
                    .approved_clients
                    .iter()
                    .find(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid)
                    .map(|a| a.display_name.clone())
                    .unwrap_or_default(),
                admitted: false,
                tap_ip: None,
                lan_subnets: self
                    .cfg
                    .approved_clients
                    .iter()
                    .find(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid)
                    .map(|a| a.lan_subnets.clone())
                    .unwrap_or_default(),
                online: true,
                sid: None,
                tap_name: None,
            };
            if fresh_row {
                self.emit(HubEvent::DevicePending {
                    network: net_uuid,
                    device,
                });
            } else {
                self.emit(HubEvent::DeviceChanged {
                    network: net_uuid,
                    device,
                });
            }
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
        let bytes_in = Arc::new(AtomicU64::new(0));
        let bytes_out = Arc::new(AtomicU64::new(0));
        let task = SessionTask::new(
            sid,
            client_uuid,
            net_uuid,
            tap,
            sess_cmd_rx,
            self.evt_tx.clone(),
            Some(self.disconnect_timeout),
            bytes_in.clone(),
            bytes_out.clone(),
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
                bytes_in,
                bytes_out,
            },
        );
        self.sessions_by_id.insert(sid, (client_uuid, net_uuid));

        if let Some(c) = self.conns.get_mut(&conn) {
            c.bound_session = Some(sid);
            let _ = c.link.bind_tx.send(Some(sess_cmd_tx)).await;
        }

        // Make sure the row is admitted — it may have been freshly
        // created with admitted=false in handle_join (this code path
        // also runs on the admit-flip-on-pending path in
        // handle_device_set, where the row is always already true).
        if let Some(row) = self
            .cfg
            .approved_clients
            .iter_mut()
            .find(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid)
        {
            if !row.admitted {
                row.admitted = true;
                self.persist().await;
            }
        }
        // Belt-and-braces: if no row exists at all (shouldn't happen —
        // handle_join always creates one — but be defensive), insert
        // an admitted row.
        if approved_entry.is_none()
            && !self
                .cfg
                .approved_clients
                .iter()
                .any(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid)
        {
            self.cfg.approved_clients.push(ApprovedClient {
                client_uuid,
                net_uuid,
                tap_ip: String::new(),
                display_name: String::new(),
                lan_subnets: Vec::new(),
                admitted: true,
            });
            self.persist().await;
        }

        let _ = frame_tx
            .send(Frame::JoinOk {
                tap_suffix: strip_tap_prefix(&tap_name),
                ip: tap_ip_str.clone(),
            })
            .await;
        self.send_routes(&frame_tx, net_uuid, tap_ip_str.as_deref())
            .await;

        info!(?sid, ?conn, %client_uuid, tap = %tap_name, "session joined");

        // Tell subscribers about the new live device. This is the
        // first-time-admit and the auto-approve path; the reconnect
        // path emits its own DeviceOnline in handle_join.
        self.emit(HubEvent::DeviceOnline {
            network: net_uuid,
            client_uuid,
            sid: sid.0,
            tap_name: tap_name.clone(),
        });
    }

    /// Push the derived route table for `net_uuid` to a freshly-bound
    /// conn, omitting any route whose `via` equals the conn's own TAP
    /// IP (would loop).
    async fn send_routes(
        &self,
        frame_tx: &mpsc::Sender<Frame>,
        net_uuid: Uuid,
        tap_ip: Option<&str>,
    ) {
        let routes = derive_routes_for_network(&self.cfg, net_uuid);
        if routes.is_empty() {
            return;
        }
        let filtered = filter_for_peer(&routes, tap_ip);
        if !filtered.is_empty() {
            let _ = frame_tx.send(Frame::SetRoutes(filtered)).await;
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

    fn handle_device_list(
        &self,
        net_uuid: Option<Uuid>,
        ack: oneshot::Sender<Vec<DeviceEntry>>,
    ) {
        let _ = ack.send(self.collect_devices(net_uuid));
    }

    /// Build the device list from `approved_clients` rows. Each row is
    /// either admitted or pending; live runtime state (active session
    /// or pending conn) layers on top of the persistent record.
    fn collect_devices(&self, filter_net: Option<Uuid>) -> Vec<DeviceEntry> {
        self.cfg
            .approved_clients
            .iter()
            .filter(|ac| filter_net.is_none_or(|w| ac.net_uuid == w))
            .map(|ac| {
                let live = self.sessions.get(&(ac.client_uuid, ac.net_uuid));
                let pending_conn = self.pending.contains_key(&(ac.client_uuid, ac.net_uuid));
                DeviceEntry {
                    client_uuid: ac.client_uuid,
                    net_uuid: ac.net_uuid,
                    display_name: ac.display_name.clone(),
                    admitted: ac.admitted,
                    tap_ip: if ac.tap_ip.is_empty() {
                        None
                    } else {
                        Some(ac.tap_ip.clone())
                    },
                    lan_subnets: ac.lan_subnets.clone(),
                    // "Online" means the client is connected. That's
                    // either a real session (admitted) or a pending
                    // conn awaiting admission.
                    online: live.is_some() || pending_conn,
                    sid: live.map(|s| s.sid.0),
                    tap_name: live.map(|s| s.tap_name.clone()),
                }
            })
            .collect()
    }

    async fn handle_device_set(
        &mut self,
        client_uuid: Uuid,
        net_uuid: Uuid,
        update: DeviceUpdate,
        ack: oneshot::Sender<DeviceSetResult>,
    ) {
        // Validate `tap_ip` early so we don't half-mutate state.
        if let Some(ip) = update.tap_ip.as_deref() {
            if !ip.is_empty()
                && ip.parse::<IpNet>().is_err()
                && ip.parse::<std::net::IpAddr>().is_err()
            {
                let _ = ack.send(DeviceSetResult::InvalidIp);
                return;
            }
            // Conflict check: nobody else in the same network has it.
            if !ip.is_empty() {
                if let Some(other) = self.cfg.approved_clients.iter().find(|a| {
                    a.net_uuid == net_uuid && a.client_uuid != client_uuid && a.tap_ip == *ip
                }) {
                    let _ = ack.send(DeviceSetResult::Conflict {
                        msg: format!(
                            "tap_ip {} already used by {}",
                            ip,
                            short_uuid(&other.client_uuid)
                        ),
                    });
                    return;
                }
            }
        }

        // Validate every lan_subnet syntactically before persisting.
        if let Some(subs) = update.lan_subnets.as_deref() {
            for s in subs {
                if IpNet::from_str(s).is_err() {
                    let _ = ack.send(DeviceSetResult::InvalidIp);
                    return;
                }
            }
        }

        // Row must already exist — handle_join created one with
        // admitted=false on first contact, and admin actions only target
        // visible devices. There's no "create from nothing" UX anymore.
        let idx = match self
            .cfg
            .approved_clients
            .iter()
            .position(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid)
        {
            Some(i) => i,
            None => {
                let _ = ack.send(DeviceSetResult::NotFound);
                return;
            }
        };

        // Apply non-admit field updates first (these are independent
        // of session state).
        let row = &mut self.cfg.approved_clients[idx];
        let prev_admitted = row.admitted;
        if let Some(name) = update.name {
            row.display_name = name;
        }
        let ip_changed = if let Some(ip) = update.tap_ip {
            if row.tap_ip != ip {
                row.tap_ip = ip;
                true
            } else {
                false
            }
        } else {
            false
        };
        let lan_changed = if let Some(subs) = update.lan_subnets {
            if row.lan_subnets != subs {
                row.lan_subnets = subs;
                true
            } else {
                false
            }
        } else {
            false
        };

        // Admit toggle handling. `None` = leave admitted as-is.
        let new_admitted = update.admitted.unwrap_or(prev_admitted);
        row.admitted = new_admitted;

        // Capture cloned scalars for use after we drop the &mut row.
        let display_name = row.display_name.clone();
        let tap_ip_str = row.tap_ip.clone();
        let lan_subnets = row.lan_subnets.clone();

        self.persist().await;

        // Live SET_IP push. Routes are NOT auto-pushed on lan_subnets
        // change — caller must `device_push` to commit.
        if ip_changed {
            if let Some(s) = self.sessions.get_mut(&(client_uuid, net_uuid)) {
                s.tap_ip = if tap_ip_str.is_empty() {
                    None
                } else {
                    Some(tap_ip_str.clone())
                };
                if let Some(conn_id) = s.bound_conn {
                    if let Some(c) = self.conns.get(&conn_id) {
                        let payload = if tap_ip_str.is_empty() {
                            None
                        } else {
                            Some(tap_ip_str.clone())
                        };
                        let _ = c.link.frame_tx.send(Frame::SetIp { ip: payload }).await;
                    }
                }
            }
            // ip change affects the bridge's view of which `via`s are
            // valid for this network — re-sync server-side routes.
            self.sync_local_routes().await;
        }
        let _ = lan_changed; // currently no immediate side effect

        // Now act on the admit transition.
        match (prev_admitted, new_admitted) {
            (false, true) => {
                // Flip ON: if there's a pending conn, promote it to a
                // real session right now. Otherwise the next join from
                // this client lands directly in `do_approve`.
                if let Some(conn) = self.pending.remove(&(client_uuid, net_uuid)) {
                    let sid = self.ids.next_session();
                    self.do_approve(sid, conn, client_uuid, net_uuid).await;
                }
            }
            (true, false) => {
                // Flip OFF (kick): kill the live session and drop the
                // bound conn. The client's reconnect loop fires; on
                // reconnect, handle_join sees admitted=false and lands
                // it back in pending — which is what the user expects.
                let kick_target = self
                    .sessions
                    .get(&(client_uuid, net_uuid))
                    .map(|s| (s.cmd_tx.clone(), s.bound_conn));
                if let Some((cmd_tx, bound_conn)) = kick_target {
                    let _ = cmd_tx.send(SessionCmd::Kill).await;
                    if let Some(c_id) = bound_conn {
                        // Removing the entry drops its frame_tx, so
                        // ConnTask exits cleanly.
                        self.conns.remove(&c_id);
                    }
                }
                // Drop any pending conn too, so the new model is
                // fully off:
                if let Some(conn_id) = self.pending.remove(&(client_uuid, net_uuid)) {
                    self.conns.remove(&conn_id);
                }
            }
            // No transition.
            _ => {}
        }

        let live = self.sessions.get(&(client_uuid, net_uuid));
        let entry = DeviceEntry {
            client_uuid,
            net_uuid,
            display_name,
            admitted: new_admitted,
            tap_ip: if tap_ip_str.is_empty() {
                None
            } else {
                Some(tap_ip_str)
            },
            lan_subnets,
            online: live.is_some() || self.pending.contains_key(&(client_uuid, net_uuid)),
            sid: live.map(|s| s.sid.0),
            tap_name: live.map(|s| s.tap_name.clone()),
        };
        // Broadcast the post-update view so other tabs / the same tab's
        // other components see it without waiting for the next poll.
        self.emit(HubEvent::DeviceChanged {
            network: net_uuid,
            device: entry.clone(),
        });
        let _ = ack.send(DeviceSetResult::Ok(entry));
    }

    async fn handle_device_push(
        &mut self,
        net_uuid: Uuid,
        ack: oneshot::Sender<DevicePushResult>,
    ) {
        let routes = derive_routes_for_network(&self.cfg, net_uuid);

        // Apply the full host-wide set on the bridge (covers all networks
        // in case multiple coexist later; in v1 there's at most one).
        self.sync_local_routes().await;

        // Push to bound conns *in this network*, filtering self-loops.
        let bound: Vec<(ConnId, Option<String>)> = self
            .sessions
            .values()
            .filter(|s| s.net_uuid == net_uuid)
            .filter_map(|s| s.bound_conn.map(|c| (c, s.tap_ip.clone())))
            .collect();
        let mut count: u64 = 0;
        for (conn_id, tap_ip) in bound {
            let filtered = filter_for_peer(&routes, tap_ip.as_deref());
            if filtered.is_empty() {
                continue;
            }
            if let Some(c) = self.conns.get(&conn_id) {
                if c.link
                    .frame_tx
                    .send(Frame::SetRoutes(filtered))
                    .await
                    .is_ok()
                {
                    count += 1;
                }
            }
        }
        // Broadcast so the WebUI can refresh whatever route table it's
        // displaying without re-querying.
        self.emit(HubEvent::RoutesChanged {
            network: net_uuid,
            routes: routes
                .iter()
                .map(|r| RouteRow {
                    dst: r.dst.clone(),
                    via: r.via.clone(),
                })
                .collect(),
            count,
        });
        let _ = ack.send(DevicePushResult { routes, count });
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
                .iter()
                .map(|((cuid, nuid), conn)| PendingInfo {
                    client_uuid: *cuid,
                    net_uuid: *nuid,
                    conn: *conn,
                })
                .collect(),
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

        self.emit(HubEvent::DeviceOffline {
            network: entry.net_uuid,
            client_uuid: entry.client_uuid,
        });
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

fn short_uuid(u: &Uuid) -> String {
    u.simple().to_string()[..8].to_owned()
}
