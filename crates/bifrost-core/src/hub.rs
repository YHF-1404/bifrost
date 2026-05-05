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

use crate::config::{ApprovedClient, NetRecord, PendingClient, ServerConfig};
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
    /// Networks whose currently-derived routes don't match what was
    /// last broadcast via `device_push`. The WebUI uses this to
    /// pulse the hub-card "push routes" button amber on first page
    /// load (the per-event `routes.dirty` WS message handles updates
    /// after that).
    pub routes_dirty: std::collections::HashSet<Uuid>,
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

/// Outcome of a [`HubHandle::make_net`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MakeNetResult {
    /// Network created; UUID returned.
    Ok(Uuid),
    /// `bridge_ip` was supplied but is malformed or has an unsupported
    /// prefix (Phase 3 constrains to `/16` or `/24` only). The network
    /// was NOT created — admins re-run with a valid value.
    InvalidBridgeIp(String),
}

/// Outcome of a [`HubHandle::set_net_bridge_ip`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetNetBridgeIpResult {
    /// Update applied; full updated network record returned.
    Ok(NetRecord),
    /// `net_uuid` is unknown.
    NotFound,
    /// `bridge_ip` is malformed or has an unsupported prefix (Phase 3
    /// constrains to `/16` or `/24` only).
    Invalid(String),
}

/// Outcome of a [`HubHandle::assign_client`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignClientResult {
    /// Move applied; full updated record returned (with the new
    /// `net_uuid`, or `None` if detached to the pending pool).
    Ok(DeviceEntry),
    /// `client_uuid` is unknown to the server (not in either
    /// `approved_clients` or `pending_clients`, and no live conn
    /// has Hello'd as this UUID).
    NotFound,
    /// `net_uuid = Some(nid)` but `nid` is not in `cfg.networks`.
    UnknownNetwork,
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
        bridge_ip: Option<String>,
        ack: oneshot::Sender<MakeNetResult>,
    },
    /// Rename a network. Returns `true` on success, `false` if the
    /// network UUID is unknown.
    RenameNet {
        net_uuid: Uuid,
        name: String,
        ack: oneshot::Sender<bool>,
    },
    /// **Phase 3.** Update a network's bridge IP/CIDR. Empty string
    /// clears it (pure-L2 mode). Only `/16` and `/24` prefixes are
    /// accepted. When the prefix length changes, every admitted
    /// client's `tap_ip` in this network has its prefix rewritten in
    /// place (address octets unchanged) and a fresh `SetIp` is pushed
    /// to live sessions.
    SetNetBridgeIp {
        net_uuid: Uuid,
        bridge_ip: String,
        ack: oneshot::Sender<SetNetBridgeIpResult>,
    },
    /// Cascade-delete a network and every approved-client row in it.
    /// Active sessions are killed; pending conns are dropped.
    DeleteNet {
        net_uuid: Uuid,
        ack: oneshot::Sender<bool>,
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
    /// **Phase 3.** Move a client into a network (or detach to pending).
    /// `net_uuid = None` puts the client in the pending pool;
    /// `Some(nid)` makes it a (admitted=false, tap_ip="") member of
    /// that network, which the admin then admits via `DeviceSet`.
    AssignClient {
        client_uuid: Uuid,
        net_uuid: Option<Uuid>,
        ack: oneshot::Sender<AssignClientResult>,
    },
    /// **Phase 3.** Edit metadata of a pending (unassigned) client.
    /// Returns `None` if the client isn't in the pending pool.
    PatchPendingClient {
        client_uuid: Uuid,
        name: Option<String>,
        lan_subnets: Option<Vec<String>>,
        ack: oneshot::Sender<Option<DeviceEntry>>,
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

    /// Create a virtual network. `bridge_ip` is an optional CIDR for
    /// the host-side gateway address (e.g. `"10.0.0.1/24"`); `None`
    /// leaves the bridge address-less. Validation happens in the hub
    /// task; an invalid `bridge_ip` returns `InvalidBridgeIp(_)` and
    /// the network is NOT persisted (callers can prompt the admin
    /// to retry with a fixed value).
    pub async fn make_net(
        &self,
        name: String,
        bridge_ip: Option<String>,
    ) -> Option<MakeNetResult> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(HubCmd::MakeNet {
                name,
                bridge_ip,
                ack: tx,
            })
            .await
            .ok()?;
        rx.await.ok()
    }

    /// Rename `net_uuid` → `name`. Returns `false` if the network was
    /// not found.
    pub async fn rename_net(&self, net_uuid: Uuid, name: String) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::RenameNet {
                net_uuid,
                name,
                ack: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Cascade-delete `net_uuid` along with every device row in it.
    /// Active sessions are killed; conns are dropped. Returns `false`
    /// when the network is unknown.
    pub async fn delete_net(&self, net_uuid: Uuid) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::DeleteNet { net_uuid, ack: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
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

    /// **Phase 3.** Update a network's bridge IP/CIDR.
    pub async fn set_net_bridge_ip(
        &self,
        net_uuid: Uuid,
        bridge_ip: String,
    ) -> SetNetBridgeIpResult {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::SetNetBridgeIp {
                net_uuid,
                bridge_ip,
                ack: tx,
            })
            .await
            .is_err()
        {
            return SetNetBridgeIpResult::NotFound;
        }
        rx.await.unwrap_or(SetNetBridgeIpResult::NotFound)
    }

    /// **Phase 3.** Edit display_name and/or lan_subnets of a pending
    /// (unassigned) client. Returns the post-update [`DeviceEntry`],
    /// or `None` if the client isn't in the pending pool. For admitted
    /// clients use [`HubHandle::device_set`] instead.
    pub async fn patch_pending_client(
        &self,
        client_uuid: Uuid,
        name: Option<String>,
        lan_subnets: Option<Vec<String>>,
    ) -> Option<DeviceEntry> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::PatchPendingClient {
                client_uuid,
                name,
                lan_subnets,
                ack: tx,
            })
            .await
            .is_err()
        {
            return None;
        }
        rx.await.ok().flatten()
    }

    /// **Phase 3.** Move a client into a network (or detach it to the
    /// pending pool). Sends `AssignNet` to the client's live conn (if
    /// any) so it tears down its TAP and re-joins the new target.
    pub async fn assign_client(
        &self,
        client_uuid: Uuid,
        net_uuid: Option<Uuid>,
    ) -> AssignClientResult {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HubCmd::AssignClient {
                client_uuid,
                net_uuid,
                ack: tx,
            })
            .await
            .is_err()
        {
            return AssignClientResult::NotFound;
        }
        rx.await.unwrap_or(AssignClientResult::NotFound)
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
    /// One Linux bridge per virtual network (Phase 2). The map is
    /// populated lazily during [`Hub::run`] startup from `cfg.networks`
    /// and on every [`handle_make_net`]; entries are removed and
    /// destroyed by [`handle_delete_net`]. Lookups in
    /// [`do_approve`] / [`handle_session_died`] / [`sync_local_routes`]
    /// route every TAP add/remove and every route push to the bridge
    /// that owns the relevant network — this is what gives each
    /// network its own L2 broadcast domain.
    bridges: HashMap<Uuid, Arc<dyn Bridge>>,

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

    /// Snapshot of the route table last broadcast to all bound peers
    /// of each network via [`Self::handle_device_push`]. The
    /// `routes.dirty` event compares this against
    /// [`derive_routes_for_network`] after every config-mutating
    /// handler — when they diverge (e.g. an admin admitted a new
    /// device whose `lan_subnets` aren't yet known to existing
    /// peers), the WebUI's "push routes" button starts pulsing.
    /// In-memory only: at startup [`Self::run`] seeds it from the
    /// derived set so a clean restart is `dirty=false`.
    last_pushed_routes: HashMap<Uuid, Vec<WireRoute>>,
    /// Mirror of the WebUI's "needs push?" boolean per network. Used
    /// to avoid re-emitting `routes.dirty` for unchanged states (we
    /// want a transition signal, not a stream of identical events).
    routes_dirty: HashMap<Uuid, bool>,
}

impl Hub {
    pub fn new(
        cfg: ServerConfig,
        cfg_path: Option<PathBuf>,
        platform: Arc<dyn Platform>,
    ) -> (Self, HubHandle) {
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (evt_tx, evt_rx) = mpsc::channel(64);
        let (events_tx, _) = broadcast::channel(EVENTS_CAPACITY);
        let disconnect_timeout = Duration::from_secs(cfg.bridge.disconnect_timeout);
        let hub = Self {
            cfg,
            cfg_path,
            platform,
            bridges: HashMap::new(),
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
            last_pushed_routes: HashMap::new(),
            routes_dirty: HashMap::new(),
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
        // Each network's routes go to its OWN bridge — one of the
        // big wins of Phase 2 is that two networks with overlapping
        // destinations don't collide on a single global table. We no
        // longer need cross-network dedup.
        for net in &self.cfg.networks {
            let Some(bridge) = self.bridges.get(&net.uuid) else {
                continue;
            };
            let raw = derive_routes_for_network(&self.cfg, net.uuid);
            let parsed: Vec<bifrost_net::RouteEntry> = raw
                .iter()
                .filter_map(|r| bifrost_net::RouteEntry::parse(&r.dst, &r.via).ok())
                .collect();
            if let Err(e) = bridge.apply_routes(&parsed).await {
                warn!(network = %net.uuid, error = %e, "bridge.apply_routes failed");
            }
        }
    }

    /// Re-evaluate whether `net_uuid`'s derived route set still
    /// matches what was last pushed to peers. Emits a
    /// `HubEvent::RoutesDirty` only on state TRANSITIONS — repeated
    /// no-op edits don't produce a flurry of identical events.
    ///
    /// Order-insensitive equality: routes are sorted by `(dst, via)`
    /// before comparison so two equivalent tables (postcard might
    /// not preserve insertion order across joins) compare equal.
    fn recheck_routes_dirty(&mut self, net_uuid: Uuid) {
        // Skip nets we don't manage (e.g. one was just deleted and
        // the caller is wrapping cleanup).
        if !self.cfg.networks.iter().any(|n| n.uuid == net_uuid) {
            return;
        }
        let mut derived = derive_routes_for_network(&self.cfg, net_uuid);
        derived.sort_by(|a, b| (a.dst.cmp(&b.dst)).then(a.via.cmp(&b.via)));
        let mut pushed = self
            .last_pushed_routes
            .get(&net_uuid)
            .cloned()
            .unwrap_or_default();
        pushed.sort_by(|a, b| (a.dst.cmp(&b.dst)).then(a.via.cmp(&b.via)));
        let new_dirty = derived != pushed;
        let old_dirty = self.routes_dirty.get(&net_uuid).copied().unwrap_or(false);
        if new_dirty != old_dirty {
            self.emit(HubEvent::RoutesDirty {
                network: net_uuid,
                dirty: new_dirty,
            });
        }
        self.routes_dirty.insert(net_uuid, new_dirty);
    }

    /// Snapshot of which networks currently need a route push, for the
    /// WebUI's `routes_dirty` field on `GET /api/networks`.
    pub fn routes_dirty_set(&self) -> std::collections::HashSet<Uuid> {
        self.routes_dirty
            .iter()
            .filter_map(|(k, v)| if *v { Some(*k) } else { None })
            .collect()
    }

    /// Stand up one bridge per persisted network. Persists the config
    /// afterwards so any auto-derived `bridge_name` lands in the TOML.
    /// Must run before [`sync_local_routes`] so each network has its
    /// kernel bridge ready.
    async fn bootstrap_bridges(&mut self) {
        for net in self.cfg.networks.clone() {
            self.create_bridge_for(&net).await;
        }
    }

    /// Create (or look up) the kernel bridge for `net` and stash it in
    /// `self.bridges`. Idempotent — the platform's `create_bridge` is
    /// expected to reuse an existing kernel bridge of the same name.
    async fn create_bridge_for(&mut self, net: &NetRecord) {
        if self.bridges.contains_key(&net.uuid) {
            return;
        }
        let ip = if net.bridge_ip.is_empty() {
            None
        } else {
            net.bridge_ip.parse().ok()
        };
        match self
            .platform
            .create_bridge(&net.bridge_name, ip)
            .await
        {
            Ok(br) => {
                info!(
                    network = %net.uuid,
                    bridge = %net.bridge_name,
                    "bridge ready"
                );
                self.bridges.insert(net.uuid, br);
            }
            Err(e) => {
                error!(
                    network = %net.uuid,
                    bridge = %net.bridge_name,
                    error = %e,
                    "create_bridge failed; this network's L2 plane is offline",
                );
            }
        }
    }

    pub async fn run(mut self) {
        info!(
            networks = self.cfg.networks.len(),
            disconnect_timeout_s = self.disconnect_timeout.as_secs(),
            "hub start"
        );

        // Stand up the kernel bridge for every persisted network
        // before any conn can ask to join.
        self.bootstrap_bridges().await;

        // Replay any persisted routes into the kernel — covers the
        // "daemon restart" path. Without this, traffic from the
        // server toward LANs behind a client would 'no route to host'
        // until someone re-ran `route push`.
        self.sync_local_routes().await;

        // Seed `last_pushed_routes` from the persisted config so a
        // clean restart starts at `dirty=false` for every network.
        // Newly admitted clients between this startup and the next
        // `device_push` will flip dirty=true via `recheck_routes_dirty`
        // calls in the relevant handlers.
        for net in self.cfg.networks.clone() {
            let derived = derive_routes_for_network(&self.cfg, net.uuid);
            self.last_pushed_routes.insert(net.uuid, derived);
            self.routes_dirty.insert(net.uuid, false);
        }

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
            } => self.handle_hello(conn, client_uuid, version).await,
            HubCmd::Join { conn, net_uuid } => self.handle_join(conn, net_uuid).await,
            HubCmd::Disconnect { conn } => self.handle_disconnect(conn).await,
            HubCmd::MakeNet {
                name,
                bridge_ip,
                ack,
            } => self.handle_make_net(name, bridge_ip, ack).await,
            HubCmd::RenameNet {
                net_uuid,
                name,
                ack,
            } => self.handle_rename_net(net_uuid, name, ack).await,
            HubCmd::DeleteNet { net_uuid, ack } => self.handle_delete_net(net_uuid, ack).await,
            HubCmd::List { ack } => self.handle_list(ack),
            HubCmd::DeviceList { net_uuid, ack } => self.handle_device_list(net_uuid, ack),
            HubCmd::DeviceSet {
                client_uuid,
                net_uuid,
                update,
                ack,
            } => self.handle_device_set(client_uuid, net_uuid, update, ack).await,
            HubCmd::DevicePush { net_uuid, ack } => self.handle_device_push(net_uuid, ack).await,
            HubCmd::AssignClient {
                client_uuid,
                net_uuid,
                ack,
            } => self.handle_assign_client(client_uuid, net_uuid, ack).await,
            HubCmd::PatchPendingClient {
                client_uuid,
                name,
                lan_subnets,
                ack,
            } => {
                self.handle_patch_pending_client(client_uuid, name, lan_subnets, ack)
                    .await
            }
            HubCmd::SetNetBridgeIp {
                net_uuid,
                bridge_ip,
                ack,
            } => self.handle_set_net_bridge_ip(net_uuid, bridge_ip, ack).await,
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

    async fn handle_hello(&mut self, conn: ConnId, client_uuid: Uuid, version: u16) {
        if version != bifrost_proto::PROTOCOL_VERSION {
            warn!(?conn, version, "unsupported version");
            // Future: send a JoinDeny-like rejection. For v1 we're permissive.
        }
        if let Some(entry) = self.conns.get_mut(&conn) {
            entry.client_uuid = Some(client_uuid);
            debug!(?conn, %client_uuid, "hello accepted");
        }
        // Phase 3 — server is authoritative on assignment. If this is
        // a brand-new client_uuid (no row anywhere), create a
        // `pending_clients` entry so the WebUI's left pane can show
        // it. The client's `joined_network` in its toml is treated as
        // a hint; if the server has no record, we put it in pending
        // and let the admin assign.
        let known = self
            .cfg
            .approved_clients
            .iter()
            .any(|a| a.client_uuid == client_uuid)
            || self
                .cfg
                .pending_clients
                .iter()
                .any(|p| p.client_uuid == client_uuid);
        if !known {
            self.cfg.pending_clients.push(PendingClient {
                client_uuid,
                display_name: String::new(),
                lan_subnets: Vec::new(),
            });
            self.persist().await;
            self.emit(HubEvent::DevicePending {
                network: Uuid::nil(),
                device: DeviceEntry {
                    client_uuid,
                    net_uuid: None,
                    display_name: String::new(),
                    admitted: false,
                    tap_ip: None,
                    lan_subnets: Vec::new(),
                    online: true,
                    sid: None,
                    tap_name: None,
                },
            });
            info!(%client_uuid, "new pending client registered");
        } else {
            // Already known — emit DeviceChanged so the WebUI flips
            // the row's `online` to true. Find which row to broadcast.
            if let Some(p) = self
                .cfg
                .pending_clients
                .iter()
                .find(|p| p.client_uuid == client_uuid)
            {
                self.emit(HubEvent::DeviceChanged {
                    network: Uuid::nil(),
                    device: DeviceEntry {
                        client_uuid,
                        net_uuid: None,
                        display_name: p.display_name.clone(),
                        admitted: false,
                        tap_ip: None,
                        lan_subnets: p.lan_subnets.clone(),
                        online: true,
                        sid: None,
                        tap_name: None,
                    },
                });
            }
        }

        // Phase 3 — server is authoritative; the client's
        // `joined_network` in its toml is just a hint. Push the
        // current assignment to the conn so it knows where it
        // belongs, regardless of whatever (possibly stale) value
        // it had locally. `Some(net)` triggers an automatic
        // `Join { net }` on the client; `None` parks it idle.
        let assigned_net = self
            .cfg
            .approved_clients
            .iter()
            .find(|a| a.client_uuid == client_uuid)
            .map(|a| a.net_uuid);
        if let Some(c) = self.conns.get(&conn) {
            let _ = c
                .link
                .frame_tx
                .send(Frame::AssignNet {
                    net_uuid: assigned_net,
                })
                .await;
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

        // Phase 3 — if this conn was an unassigned client (Hello'd but
        // no Join), surface the offline event so the WebUI's left pane
        // can dim its row.
        if let Some(cuid) = entry.client_uuid {
            let in_pending_pool = self
                .cfg
                .pending_clients
                .iter()
                .any(|p| p.client_uuid == cuid);
            if in_pending_pool {
                self.emit(HubEvent::DeviceOffline {
                    network: Uuid::nil(),
                    client_uuid: cuid,
                });
            }
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

        // Phase 3 — server is authoritative. Reject Joins that don't
        // match the server's current assignment for this client.
        if self
            .cfg
            .pending_clients
            .iter()
            .any(|p| p.client_uuid == client_uuid)
        {
            let _ = frame_tx
                .send(Frame::JoinDeny {
                    reason: "unassigned".into(),
                })
                .await;
            return;
        }
        let assigned_net = self
            .cfg
            .approved_clients
            .iter()
            .find(|a| a.client_uuid == client_uuid)
            .map(|a| a.net_uuid);
        match assigned_net {
            Some(n) if n != net_uuid => {
                let _ = frame_tx
                    .send(Frame::JoinDeny {
                        reason: format!("wrong_network:assigned={n}"),
                    })
                    .await;
                return;
            }
            None => {
                // Defensive — handle_hello should always have created
                // a pending row already, but just in case.
                let _ = frame_tx
                    .send(Frame::JoinDeny {
                        reason: "unassigned".into(),
                    })
                    .await;
                return;
            }
            Some(_) => { /* matches; proceed */ }
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
        // Phase 3: handle_hello already created any necessary rows.
        // The Phase-3 server-authoritative validation above guarantees
        // we're here only if an approved_clients row exists for
        // (client_uuid, net_uuid). `fresh_row` is therefore always
        // false in Phase 3 (kept as a flag for the WebUI signal logic
        // below).
        let key = (client_uuid, net_uuid);
        let row_idx = self
            .cfg
            .approved_clients
            .iter()
            .position(|a| a.client_uuid == client_uuid && a.net_uuid == net_uuid)
            .expect("Phase 3 invariants checked above");
        let admitted = self.cfg.approved_clients[row_idx].admitted;
        let fresh_row = false;

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
                net_uuid: Some(net_uuid),
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

        let Some(bridge) = self.bridges.get(&net_uuid).cloned() else {
            error!(network = %net_uuid, "no bridge for network; refusing approval");
            let _ = tap.destroy().await;
            let _ = frame_tx
                .send(Frame::JoinDeny {
                    reason: "no_bridge".into(),
                })
                .await;
            return;
        };
        if let Err(e) = bridge.add_tap(&tap_name).await {
            error!(error = %e, "bridge.add_tap failed");
            let _ = tap.destroy().await;
            let _ = frame_tx
                .send(Frame::JoinDeny {
                    reason: format!("bridge_error:{e}"),
                })
                .await;
            return;
        }

        // Carries data-plane EthIn frames from the conn task into this
        // session. 1024 lets bulk traffic queue without immediately
        // blocking the conn task's `tx.send(EthIn).await`, which would
        // otherwise stall every inbound packet behind socket I/O.
        let (sess_cmd_tx, sess_cmd_rx) = mpsc::channel(1024);
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

        // Newly admitting a device may have introduced lan_subnets
        // not in the last-pushed set; flag the network as needing a
        // route push so the WebUI's button pulses amber.
        self.recheck_routes_dirty(net_uuid);
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

    async fn handle_make_net(
        &mut self,
        name: String,
        bridge_ip: Option<String>,
        ack: oneshot::Sender<MakeNetResult>,
    ) {
        // Validate `bridge_ip` BEFORE creating anything — same prefix
        // rules as `set_net_bridge_ip` (Phase 3 picker is /16 or /24
        // only). Failing fast keeps a half-created network out of the
        // persisted config.
        let validated_ip = match bridge_ip.as_deref() {
            None | Some("") => None,
            Some(s) => match s.parse::<IpNet>() {
                Ok(net) => match net.prefix_len() {
                    16 | 24 => Some(net),
                    other => {
                        let _ = ack.send(MakeNetResult::InvalidBridgeIp(format!(
                            "prefix /{other} not supported (use /16 or /24)"
                        )));
                        return;
                    }
                },
                Err(e) => {
                    let _ = ack.send(MakeNetResult::InvalidBridgeIp(e.to_string()));
                    return;
                }
            },
        };

        let uuid = Uuid::new_v4();
        let mut rec = NetRecord::new(name.clone(), uuid);
        if let Some(ip) = &validated_ip {
            rec.bridge_ip = ip.to_string();
        }
        // Create the kernel-side bridge first. If that fails the
        // record never lands in `cfg.networks` — better to surface the
        // error to the admin than to leave a half-created network in
        // a state where joins succeed but frames go nowhere.
        // `create_bridge_for` reads `rec.bridge_ip`, so passing it
        // here installs the IP on the kernel link in one step
        // instead of requiring a follow-up `set bridge-ip` call.
        self.create_bridge_for(&rec).await;
        self.cfg.networks.push(rec);
        self.persist().await;
        // Empty network → empty derived → empty last_pushed →
        // dirty=false. Initialising the entries explicitly keeps the
        // WebUI's `routes_dirty` snapshot complete from the moment
        // the network exists.
        self.last_pushed_routes.insert(uuid, Vec::new());
        self.routes_dirty.insert(uuid, false);
        info!(%name, %uuid, ?validated_ip, "network created");
        self.emit(HubEvent::NetworkCreated {
            network: uuid,
            name,
        });
        let _ = ack.send(MakeNetResult::Ok(uuid));
    }

    async fn handle_set_net_bridge_ip(
        &mut self,
        net_uuid: Uuid,
        new_ip: String,
        ack: oneshot::Sender<SetNetBridgeIpResult>,
    ) {
        // Validate.
        let new_prefix = if new_ip.is_empty() {
            None
        } else {
            match new_ip.parse::<IpNet>() {
                Ok(net) => match net.prefix_len() {
                    16 | 24 => Some(net.prefix_len()),
                    other => {
                        let _ = ack.send(SetNetBridgeIpResult::Invalid(format!(
                            "prefix /{other} not supported (use /16 or /24)"
                        )));
                        return;
                    }
                },
                Err(e) => {
                    let _ = ack.send(SetNetBridgeIpResult::Invalid(e.to_string()));
                    return;
                }
            }
        };

        let Some(rec) = self.cfg.networks.iter_mut().find(|n| n.uuid == net_uuid) else {
            let _ = ack.send(SetNetBridgeIpResult::NotFound);
            return;
        };

        let old_ip = rec.bridge_ip.clone();
        let old_prefix = old_ip.parse::<IpNet>().ok().map(|n| n.prefix_len());
        rec.bridge_ip = new_ip.clone();
        let updated_rec = rec.clone();

        // Phase 3 — when only the prefix changed (e.g. /24 → /16),
        // rewrite each client's tap_ip to the new prefix length. The
        // address octets are kept; this is the "auto-rewrite" promise
        // from the spec (B5).
        let prefix_changed = match (old_prefix, new_prefix) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        let mut sessions_to_notify: Vec<(Uuid, String)> = Vec::new();
        if prefix_changed {
            let new_p = new_prefix.expect("prefix_changed implies Some");
            for ac in self
                .cfg
                .approved_clients
                .iter_mut()
                .filter(|a| a.net_uuid == net_uuid && !a.tap_ip.is_empty())
            {
                if let Ok(parsed) = ac.tap_ip.parse::<IpNet>() {
                    let rewritten = format!("{}/{}", parsed.addr(), new_p);
                    ac.tap_ip = rewritten.clone();
                    sessions_to_notify.push((ac.client_uuid, rewritten));
                }
            }
        }

        self.persist().await;

        // Push the new IP through netlink onto the live bridge link.
        // Without this the WebUI/API edit only updates the on-disk
        // config — kernel state stays at whatever IP the bridge was
        // created with (or none), and clients can't reach the
        // gateway until the server restarts and re-creates the
        // bridge from persisted config.
        if let Some(bridge) = self.bridges.get(&net_uuid) {
            let parsed = if new_ip.is_empty() {
                None
            } else {
                new_ip.parse::<IpNet>().ok()
            };
            if let Err(e) = bridge.set_ip(parsed).await {
                warn!(%net_uuid, error = %e, "bridge.set_ip failed (config still updated)");
            }
        }

        // Push the new IP down to any live session whose tap_ip we
        // just rewrote.
        for (cuid, ip) in &sessions_to_notify {
            if let Some(s) = self.sessions.get_mut(&(*cuid, net_uuid)) {
                s.tap_ip = Some(ip.clone());
                if let Some(conn_id) = s.bound_conn {
                    if let Some(c) = self.conns.get(&conn_id) {
                        let _ = c
                            .link
                            .frame_tx
                            .send(Frame::SetIp {
                                ip: Some(ip.clone()),
                            })
                            .await;
                    }
                }
            }
        }

        // Routes are derived from tap_ip — re-sync now that prefixes
        // shifted.
        if prefix_changed {
            self.sync_local_routes().await;
        }

        info!(%net_uuid, old=%old_ip, new=%new_ip, "bridge_ip updated");
        // Reuse NetworkChanged for both rename and bridge-ip updates so
        // the WebUI just refreshes the network row.
        self.emit(HubEvent::NetworkChanged {
            network: net_uuid,
            name: updated_rec.name.clone(),
        });
        let _ = ack.send(SetNetBridgeIpResult::Ok(updated_rec));
    }

    async fn handle_rename_net(
        &mut self,
        net_uuid: Uuid,
        name: String,
        ack: oneshot::Sender<bool>,
    ) {
        let Some(rec) = self.cfg.networks.iter_mut().find(|n| n.uuid == net_uuid) else {
            let _ = ack.send(false);
            return;
        };
        rec.name = name.clone();
        self.persist().await;
        info!(%name, %net_uuid, "network renamed");
        self.emit(HubEvent::NetworkChanged {
            network: net_uuid,
            name,
        });
        let _ = ack.send(true);
    }

    async fn handle_delete_net(&mut self, net_uuid: Uuid, ack: oneshot::Sender<bool>) {
        if !self.cfg.networks.iter().any(|n| n.uuid == net_uuid) {
            let _ = ack.send(false);
            return;
        }

        // Phase 3 — deleting a network DETACHES its clients (they don't
        // disappear, they fall back to the pending pool). Each affected
        // live conn gets `AssignNet { None }` so the client tears down
        // its TAP and idles.
        let live_keys: Vec<(Uuid, Uuid)> = self
            .sessions
            .keys()
            .filter(|(_, n)| *n == net_uuid)
            .copied()
            .collect();
        for key in &live_keys {
            if let Some(session) = self.sessions.get(key) {
                let _ = session.cmd_tx.send(SessionCmd::Kill).await;
                // DON'T remove the conn — we want it alive to receive
                // AssignNet below.
            }
        }

        // Drop pending entries in this network (the conns stay).
        let pending_keys: Vec<(Uuid, Uuid)> = self
            .pending
            .keys()
            .filter(|(_, n)| *n == net_uuid)
            .copied()
            .collect();
        for key in &pending_keys {
            self.pending.remove(key);
        }

        // Move every approved_clients row in this net to pending_clients,
        // carrying display_name + lan_subnets across so re-assignment
        // preserves what the admin configured.
        let detaching: Vec<ApprovedClient> = self
            .cfg
            .approved_clients
            .iter()
            .filter(|a| a.net_uuid == net_uuid)
            .cloned()
            .collect();
        self.cfg
            .approved_clients
            .retain(|a| a.net_uuid != net_uuid);
        for ac in &detaching {
            if !self
                .cfg
                .pending_clients
                .iter()
                .any(|p| p.client_uuid == ac.client_uuid)
            {
                self.cfg.pending_clients.push(PendingClient {
                    client_uuid: ac.client_uuid,
                    display_name: ac.display_name.clone(),
                    lan_subnets: ac.lan_subnets.clone(),
                });
            }
            // Tell any live conn to drop its TAP and idle.
            if let Some(conn) = self.conn_for_client(ac.client_uuid) {
                if let Some(c) = self.conns.get(&conn) {
                    let _ = c
                        .link
                        .frame_tx
                        .send(Frame::AssignNet { net_uuid: None })
                        .await;
                    let _ = c.link.bind_tx.send(None).await;
                }
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.bound_session = None;
                }
            }
            // Re-emit as DevicePending so the WebUI sees the row appear
            // in the pending pane.
            self.emit(HubEvent::DevicePending {
                network: Uuid::nil(),
                device: DeviceEntry {
                    client_uuid: ac.client_uuid,
                    net_uuid: None,
                    display_name: ac.display_name.clone(),
                    admitted: false,
                    tap_ip: None,
                    lan_subnets: ac.lan_subnets.clone(),
                    online: self.conn_for_client(ac.client_uuid).is_some(),
                    sid: None,
                    tap_name: None,
                },
            });
        }

        // Drop the network record itself.
        self.cfg.networks.retain(|n| n.uuid != net_uuid);

        // Tear down the kernel bridge that was unique to this network.
        if let Some(bridge) = self.bridges.remove(&net_uuid) {
            if let Err(e) = bridge.destroy().await {
                warn!(%net_uuid, error = %e, "bridge destroy on delete_net failed");
            }
        }

        self.persist().await;
        self.sync_local_routes().await;
        // Network is gone — drop its routes-tracking entries.
        self.last_pushed_routes.remove(&net_uuid);
        self.routes_dirty.remove(&net_uuid);
        info!(%net_uuid, "network deleted; clients detached to pending");
        self.emit(HubEvent::NetworkDeleted { network: net_uuid });

        let _ = ack.send(true);
    }

    // ── Phase 3: assign / detach a client ────────────────────────────

    /// Find a live conn that has Hello'd as `client_uuid`, regardless
    /// of its session-binding state. Returns `None` if the client
    /// hasn't reconnected yet (or has disconnected).
    fn conn_for_client(&self, client_uuid: Uuid) -> Option<ConnId> {
        self.conns.iter().find_map(|(id, c)| {
            if c.client_uuid == Some(client_uuid) {
                Some(*id)
            } else {
                None
            }
        })
    }

    async fn handle_assign_client(
        &mut self,
        client_uuid: Uuid,
        new_net: Option<Uuid>,
        ack: oneshot::Sender<AssignClientResult>,
    ) {
        // Validate target network if specified.
        if let Some(n) = new_net {
            if !self.cfg.networks.iter().any(|net| net.uuid == n) {
                let _ = ack.send(AssignClientResult::UnknownNetwork);
                return;
            }
        }

        // Snapshot current state.
        let approved_idx = self
            .cfg
            .approved_clients
            .iter()
            .position(|a| a.client_uuid == client_uuid);
        let pending_idx = self
            .cfg
            .pending_clients
            .iter()
            .position(|p| p.client_uuid == client_uuid);
        let conn_id = self.conn_for_client(client_uuid);

        if approved_idx.is_none() && pending_idx.is_none() && conn_id.is_none() {
            let _ = ack.send(AssignClientResult::NotFound);
            return;
        }

        let (display_name, lan_subnets, current_net) = match (approved_idx, pending_idx) {
            (Some(i), _) => {
                let row = &self.cfg.approved_clients[i];
                (
                    row.display_name.clone(),
                    row.lan_subnets.clone(),
                    Some(row.net_uuid),
                )
            }
            (None, Some(i)) => {
                let row = &self.cfg.pending_clients[i];
                (row.display_name.clone(), row.lan_subnets.clone(), None)
            }
            (None, None) => (String::new(), Vec::new(), None),
        };

        // Same-net no-op (B3): preserve all current fields, just echo.
        if current_net == new_net {
            let entry = self.build_device_entry(client_uuid);
            let _ = ack.send(AssignClientResult::Ok(entry));
            return;
        }

        // Kill any live session bound to the OLD assignment.
        if let Some(old_net) = current_net {
            let key = (client_uuid, old_net);
            if let Some(session) = self.sessions.get(&key) {
                let _ = session.cmd_tx.send(SessionCmd::Kill).await;
            }
            self.pending.remove(&key);
        }

        // Rewrite the config: drop both possible row types, then add
        // exactly one back.
        self.cfg
            .approved_clients
            .retain(|a| a.client_uuid != client_uuid);
        self.cfg
            .pending_clients
            .retain(|p| p.client_uuid != client_uuid);

        if let Some(n) = new_net {
            self.cfg.approved_clients.push(ApprovedClient {
                client_uuid,
                net_uuid: n,
                tap_ip: String::new(), // cleared per spec B3
                display_name: display_name.clone(),
                lan_subnets: lan_subnets.clone(),
                admitted: false, // cleared per spec
            });
        } else {
            self.cfg.pending_clients.push(PendingClient {
                client_uuid,
                display_name: display_name.clone(),
                lan_subnets: lan_subnets.clone(),
            });
        }
        self.persist().await;

        // Tell the live conn (if any) about the new assignment.
        if let Some(conn) = conn_id {
            if let Some(c) = self.conns.get(&conn) {
                let _ = c
                    .link
                    .frame_tx
                    .send(Frame::AssignNet { net_uuid: new_net })
                    .await;
                let _ = c.link.bind_tx.send(None).await;
            }
            if let Some(c) = self.conns.get_mut(&conn) {
                c.bound_session = None;
            }
        }

        // Old admitted row (if any) is gone, so re-sync local routes.
        self.sync_local_routes().await;

        // Both networks need a recheck — the old one because a row
        // (with possibly non-empty lan_subnets) just left, the new
        // one because a row just arrived (admitted=false for now,
        // so its subnets aren't in `derive_routes` yet — but the
        // recheck covers the case where the previous derived was
        // also empty and stays empty, no event fires).
        if let Some(old_net) = current_net {
            self.recheck_routes_dirty(old_net);
        }
        if let Some(n) = new_net {
            self.recheck_routes_dirty(n);
        }

        let entry = DeviceEntry {
            client_uuid,
            net_uuid: new_net,
            display_name,
            admitted: false,
            tap_ip: None,
            lan_subnets,
            online: conn_id.is_some(),
            sid: None,
            tap_name: None,
        };
        self.emit(HubEvent::DeviceChanged {
            network: new_net.unwrap_or_else(Uuid::nil),
            device: entry.clone(),
        });
        let _ = ack.send(AssignClientResult::Ok(entry));
    }

    async fn handle_patch_pending_client(
        &mut self,
        client_uuid: Uuid,
        name: Option<String>,
        lan_subnets: Option<Vec<String>>,
        ack: oneshot::Sender<Option<DeviceEntry>>,
    ) {
        // Validate lan_subnets first (cheap, do it before mutating).
        if let Some(subs) = lan_subnets.as_deref() {
            for s in subs {
                if IpNet::from_str(s).is_err() {
                    let _ = ack.send(None);
                    return;
                }
            }
        }

        let Some(row) = self
            .cfg
            .pending_clients
            .iter_mut()
            .find(|p| p.client_uuid == client_uuid)
        else {
            let _ = ack.send(None);
            return;
        };
        if let Some(n) = name {
            row.display_name = n;
        }
        if let Some(subs) = lan_subnets {
            row.lan_subnets = subs;
        }
        let display_name = row.display_name.clone();
        let lan_clone = row.lan_subnets.clone();
        self.persist().await;

        let entry = DeviceEntry {
            client_uuid,
            net_uuid: None,
            display_name,
            admitted: false,
            tap_ip: None,
            lan_subnets: lan_clone,
            online: self.conn_for_client(client_uuid).is_some(),
            sid: None,
            tap_name: None,
        };
        self.emit(HubEvent::DeviceChanged {
            network: Uuid::nil(),
            device: entry.clone(),
        });
        let _ = ack.send(Some(entry));
    }

    /// Build a DeviceEntry view of the client's current state, regardless
    /// of whether it's in approved or pending. Used for assign no-op
    /// responses.
    fn build_device_entry(&self, client_uuid: Uuid) -> DeviceEntry {
        if let Some(ac) = self
            .cfg
            .approved_clients
            .iter()
            .find(|a| a.client_uuid == client_uuid)
        {
            let live = self.sessions.get(&(client_uuid, ac.net_uuid));
            return DeviceEntry {
                client_uuid,
                net_uuid: Some(ac.net_uuid),
                display_name: ac.display_name.clone(),
                admitted: ac.admitted,
                tap_ip: if ac.tap_ip.is_empty() {
                    None
                } else {
                    Some(ac.tap_ip.clone())
                },
                lan_subnets: ac.lan_subnets.clone(),
                online: live.is_some()
                    || self.pending.contains_key(&(client_uuid, ac.net_uuid)),
                sid: live.map(|s| s.sid.0),
                tap_name: live.map(|s| s.tap_name.clone()),
            };
        }
        if let Some(pc) = self
            .cfg
            .pending_clients
            .iter()
            .find(|p| p.client_uuid == client_uuid)
        {
            return DeviceEntry {
                client_uuid,
                net_uuid: None,
                display_name: pc.display_name.clone(),
                admitted: false,
                tap_ip: None,
                lan_subnets: pc.lan_subnets.clone(),
                online: self.conn_for_client(client_uuid).is_some(),
                sid: None,
                tap_name: None,
            };
        }
        // Live conn but no row anywhere — shouldn't happen after Phase 3.
        DeviceEntry {
            client_uuid,
            net_uuid: None,
            display_name: String::new(),
            admitted: false,
            tap_ip: None,
            lan_subnets: Vec::new(),
            online: self.conn_for_client(client_uuid).is_some(),
            sid: None,
            tap_name: None,
        }
    }

    fn handle_device_list(
        &self,
        net_uuid: Option<Uuid>,
        ack: oneshot::Sender<Vec<DeviceEntry>>,
    ) {
        let _ = ack.send(self.collect_devices(net_uuid));
    }

    /// Build the device list from `approved_clients` and (when no
    /// network filter is given) `pending_clients` rows. Each row is
    /// either admitted or pending; live runtime state (active session
    /// or pending conn) layers on top of the persistent record.
    ///
    /// `filter_net = Some(nid)` excludes the pending pool — pending
    /// clients have no network and thus never match a network filter.
    fn collect_devices(&self, filter_net: Option<Uuid>) -> Vec<DeviceEntry> {
        let mut out: Vec<DeviceEntry> = self
            .cfg
            .approved_clients
            .iter()
            .filter(|ac| filter_net.is_none_or(|w| ac.net_uuid == w))
            .map(|ac| {
                let live = self.sessions.get(&(ac.client_uuid, ac.net_uuid));
                let pending_conn = self.pending.contains_key(&(ac.client_uuid, ac.net_uuid));
                DeviceEntry {
                    client_uuid: ac.client_uuid,
                    net_uuid: Some(ac.net_uuid),
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
            .collect();
        // Phase 3 — when caller asks for "all networks" (or filter is
        // None), include pending (unassigned) clients with net_uuid=None.
        if filter_net.is_none() {
            for pc in &self.cfg.pending_clients {
                let live_conn = self.unassigned_conn(pc.client_uuid).is_some();
                out.push(DeviceEntry {
                    client_uuid: pc.client_uuid,
                    net_uuid: None,
                    display_name: pc.display_name.clone(),
                    admitted: false,
                    tap_ip: None,
                    lan_subnets: pc.lan_subnets.clone(),
                    online: live_conn,
                    sid: None,
                    tap_name: None,
                });
            }
        }
        out
    }

    /// Look up a live conn that has Hello'd as `client_uuid` but is
    /// not bound to any session and not in any `pending` row — i.e. an
    /// unassigned client awaiting a server-driven `AssignNet`.
    fn unassigned_conn(&self, client_uuid: Uuid) -> Option<ConnId> {
        for (id, c) in &self.conns {
            if c.client_uuid == Some(client_uuid)
                && c.bound_session.is_none()
                && !self
                    .pending
                    .iter()
                    .any(|((cu, _), conn)| *cu == client_uuid && *conn == *id)
            {
                return Some(*id);
            }
        }
        None
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
            net_uuid: Some(net_uuid),
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
        // Any of {lan_subnets, admitted, member-row added/removed}
        // can have shifted the derived route set vs what's currently
        // pushed to peers. The recheck is a no-op when nothing
        // actually moved.
        self.recheck_routes_dirty(net_uuid);
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
        // After a successful push the network's view of the world is
        // back in sync with what every bound peer holds. Update the
        // baseline and emit a `routes.dirty=false` (if it was true)
        // so the WebUI stops pulsing the button.
        self.last_pushed_routes
            .insert(net_uuid, routes.clone());
        self.recheck_routes_dirty(net_uuid);
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
            routes_dirty: self.routes_dirty_set(),
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
        if let Some(bridge) = self.bridges.get(&entry.net_uuid) {
            let _ = bridge.remove_tap(&entry.tap_name).await;
        }

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
        // Tear down every per-network bridge. Idempotent on the
        // platform side — `MockBridge` flips a flag, `LinuxBridge`
        // issues `link del`. We don't `clear()` `self.bridges` since
        // shutdown is the last thing this Hub instance does.
        for (nid, bridge) in self.bridges.iter() {
            if let Err(e) = bridge.destroy().await {
                warn!(network = %nid, error = %e, "bridge destroy failed");
            }
        }
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
