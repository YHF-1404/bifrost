//! TOML-backed configuration schemas for both binaries.
//!
//! Two concrete schemas live here so the two REPLs share the same
//! definitions and same atomic-save behavior. They are intentionally
//! **not** wire-compatible with the old Python `*.toml` files — the
//! decision was made up front to break clean.

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::atomic_write::write_atomic;
use crate::error::CoreError;

// ─── Server ─────────────────────────────────────────────────────────────────

/// Top-level server config (`server.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub server: ServerListen,
    #[serde(default)]
    pub bridge: BridgeConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default = "default_server_admin")]
    pub admin: AdminConfig,
    #[serde(default)]
    pub web: WebConfig,

    // `skip_serializing_if` keeps an empty list out of the saved
    // TOML — otherwise serde produces a stray `routes = []` that
    // ends up before `[server]` because of TOML's "scalars first,
    // tables after" rule.
    #[serde(default, rename = "networks", skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<NetRecord>,

    #[serde(
        default,
        rename = "approved_clients",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub approved_clients: Vec<ApprovedClient>,

    /// Phase 3 — clients that have connected (or been pre-registered)
    /// but are not currently assigned to any network. Survives restarts
    /// so an admin can pre-fill `display_name` and `lan_subnets` while
    /// the client is offline. A given `client_uuid` is in at most one
    /// of `pending_clients` and `approved_clients` at a time.
    #[serde(
        default,
        rename = "pending_clients",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub pending_clients: Vec<PendingClient>,
}

fn default_server_admin() -> AdminConfig {
    AdminConfig {
        socket: "/tmp/bifrost-server.sock".into(),
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server: ServerListen::default(),
            bridge: BridgeConfig::default(),
            metrics: MetricsConfig::default(),
            admin: default_server_admin(),
            web: WebConfig::default(),
            networks: Vec::new(),
            approved_clients: Vec::new(),
            pending_clients: Vec::new(),
        }
    }
}

/// Where the daemon's admin Unix socket lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminConfig {
    pub socket: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerListen {
    pub host: String,
    pub port: u16,
    pub save_dir: String,
}

impl Default for ServerListen {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_owned(),
            port: 8888,
            save_dir: "./received".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub name: String,
    /// Optional bridge IP/CIDR, e.g. `"10.0.0.1/24"`. Empty string = no IP.
    #[serde(default)]
    pub ip: String,
    pub disconnect_timeout: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            name: "br0".to_owned(),
            ip: String::new(),
            disconnect_timeout: 60,
        }
    }
}

/// HTTP / WebSocket WebUI listener. Default: bind localhost only.
/// Disable by setting `enabled = false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebConfig {
    pub enabled: bool,
    pub listen: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: "127.0.0.1:8080".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub listen: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:9090".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetRecord {
    pub name: String,
    pub uuid: Uuid,
    /// Kernel-side bridge name for this virtual network. Phase 2 each
    /// network gets its own L2 broadcast domain — the bridge is per-net,
    /// not global. Auto-derived from the UUID at network-creation time
    /// (see [`default_bridge_name_for`]); user can override at creation
    /// time. Empty in configs from Phase 1 — see the migration in
    /// [`ServerConfig::load`].
    #[serde(default)]
    pub bridge_name: String,
    /// Optional CIDR for the bridge interface, e.g. `"10.0.0.1/24"`.
    /// Empty = pure-L2, no host-side address (clients can still reach
    /// each other; the host can't ping them directly through this
    /// bridge). Mirrors the old `[bridge].ip` behavior, just per-net.
    #[serde(default)]
    pub bridge_ip: String,
}

/// Auto-derive a bridge name from a network UUID. Stable across
/// restarts (same UUID → same name) and short enough to fit Linux's
/// 15-char IFNAMSIZ limit (`bf-` prefix + 8 hex chars = 11).
pub fn default_bridge_name_for(net_uuid: Uuid) -> String {
    let s = net_uuid.simple().to_string();
    format!("bf-{}", &s[..8])
}

impl NetRecord {
    /// Build a fresh network record with `bridge_name` auto-derived
    /// from the UUID and no bridge IP. Most callers want this; the
    /// struct-literal form is reserved for tests and migration code
    /// that needs to set every field explicitly.
    pub fn new(name: impl Into<String>, uuid: Uuid) -> Self {
        Self {
            name: name.into(),
            uuid,
            bridge_name: default_bridge_name_for(uuid),
            bridge_ip: String::new(),
        }
    }
}

/// One known `(client, net)` pair. Despite the historical name, a row
/// here can be either currently *admitted* or *pending* (admitted=false)
/// — see [`Self::admitted`]. The table is the union of "every device
/// the admin has ever seen for this network."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedClient {
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
    /// Persisted TAP IP/CIDR for this `(client, net)` pair. Empty = unset.
    #[serde(default)]
    pub tap_ip: String,
    /// Friendly name for UI / CLI display. Empty = no name set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    /// LAN subnets reachable through this client's TAP. The server-wide
    /// route table is *derived* from these: each subnet becomes a route
    /// `{ dst: subnet, via: tap_ip.addr() }`. See `crate::routes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lan_subnets: Vec<String>,
    /// `true` = admitted to the network (gets a TAP, frames flow).
    /// `false` = pending: the row is known but the device is held in
    /// the join queue. Flipping this controls admission. Defaults to
    /// `true` so configs from before the introduction of pending rows
    /// load with their existing admit semantics.
    #[serde(default = "default_admitted")]
    pub admitted: bool,
}

fn default_admitted() -> bool {
    true
}

/// Phase 3 — a client known to the server but not currently in any
/// network. Gets promoted to an [`ApprovedClient`] row when the admin
/// drags it onto a network in the WebUI; demoted back to here when an
/// admin removes it from a network or deletes the network it was in.
///
/// `display_name` and `lan_subnets` are pre-configurable while the
/// client is offline, and survive a "drag into / drag out of" round
/// trip (the server copies them across when moving rows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingClient {
    pub client_uuid: Uuid,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lan_subnets: Vec<String>,
}

/// Route as it appears on disk (client config) and on the wire — strings
/// only, validated at the platform layer (see `bifrost_net::RouteEntry::parse`).
///
/// The server no longer persists routes directly; it derives them at push
/// time from the per-client `lan_subnets`. This type lives on solely for
/// the client-side cache populated from `Frame::SetRoutes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRoute {
    pub dst: String,
    pub via: String,
}

// ─── Client ─────────────────────────────────────────────────────────────────

/// Top-level client config (`client.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub client: ClientCore,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub tap: TapConfig,
    #[serde(default = "default_client_admin")]
    pub admin: AdminConfig,

    #[serde(default, rename = "routes", skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<WireRoute>,
}

fn default_client_admin() -> AdminConfig {
    AdminConfig {
        socket: "/tmp/bifrost-client.sock".into(),
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            client: ClientCore::default(),
            proxy: ProxyConfig::default(),
            tap: TapConfig::default(),
            admin: default_client_admin(),
            routes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientCore {
    /// Generated on first run if empty.
    #[serde(default)]
    pub uuid: String,
    pub host: String,
    pub port: u16,
    pub save_dir: String,
    pub retry_interval: u32,
    /// Auto-rejoin this network on reconnect. Empty = none.
    #[serde(default)]
    pub joined_network: String,
}

impl Default for ClientCore {
    fn default() -> Self {
        Self {
            uuid: String::new(),
            host: "127.0.0.1".to_owned(),
            port: 8888,
            save_dir: "./received".to_owned(),
            retry_interval: 5,
            joined_network: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_owned(),
            port: 1080,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapConfig {
    /// Pushed by the server via `SET_IP`. Empty = unset.
    #[serde(default)]
    pub ip: String,
}

// ─── Load / save ────────────────────────────────────────────────────────────

impl ServerConfig {
    pub async fn load(path: &Path) -> Result<Self, CoreError> {
        let mut cfg: Self = load_toml(path).await?;
        cfg.migrate_phase2_bridges();
        cfg.migrate_phase3_one_net_per_client();
        Ok(cfg)
    }

    pub async fn save(&self, path: &Path) -> Result<(), CoreError> {
        save_toml(self, path).await
    }

    /// Phase-1 → Phase-2 migration for the per-network bridge split.
    ///
    /// Phase 1 had a single global bridge configured under `[bridge]`.
    /// Phase 2 makes the bridge a per-network resource. To preserve
    /// existing deployments verbatim, the FIRST network whose
    /// `bridge_name` is empty inherits the legacy `[bridge].name` and
    /// `[bridge].ip`; any further networks without their own
    /// `bridge_name` auto-derive one from their UUID.
    ///
    /// This is idempotent: once a config has been written back with
    /// every network carrying its own `bridge_name`, this function is
    /// a no-op.
    /// Phase-2 → Phase-3 migration: one client lives in at most one
    /// network at a time.
    ///
    /// Phase 1/2 schemas allowed an `approved_clients` row per
    /// `(client, net)` pair, so the same client could end up in
    /// multiple networks simultaneously. Phase 3's drag-to-assign UX
    /// fundamentally treats each client as belonging to one network
    /// or none. To make existing configs load cleanly we collapse
    /// duplicates here:
    ///
    /// * If a `client_uuid` has any `admitted=true` row, keep the
    ///   first such row and drop the rest.
    /// * If all rows for a client are `admitted=false`, keep the
    ///   first and drop the rest.
    ///
    /// Idempotent on Phase-3-clean configs.
    fn migrate_phase3_one_net_per_client(&mut self) {
        use std::collections::HashSet;
        let mut seen: HashSet<Uuid> = HashSet::new();
        // Two passes: prefer admitted rows.
        let mut keep: Vec<ApprovedClient> = Vec::new();
        for ac in self.approved_clients.iter().filter(|a| a.admitted) {
            if seen.insert(ac.client_uuid) {
                keep.push(ac.clone());
            }
        }
        for ac in self.approved_clients.iter().filter(|a| !a.admitted) {
            if seen.insert(ac.client_uuid) {
                keep.push(ac.clone());
            }
        }
        self.approved_clients = keep;

        // Drop any pending_clients row whose client_uuid also appears
        // in approved_clients — the network row wins.
        let approved_ids: HashSet<Uuid> = self
            .approved_clients
            .iter()
            .map(|a| a.client_uuid)
            .collect();
        self.pending_clients
            .retain(|p| !approved_ids.contains(&p.client_uuid));
    }

    fn migrate_phase2_bridges(&mut self) {
        let mut legacy_consumed = false;
        for net in &mut self.networks {
            if !net.bridge_name.is_empty() {
                continue;
            }
            if !legacy_consumed && !self.bridge.name.is_empty() {
                // First network without per-net config inherits the
                // legacy global bridge so the existing kernel-side
                // bridge (and its host IP) keep working without any
                // operator intervention.
                net.bridge_name = self.bridge.name.clone();
                net.bridge_ip = self.bridge.ip.clone();
                legacy_consumed = true;
            } else {
                net.bridge_name = default_bridge_name_for(net.uuid);
            }
        }
    }
}

impl ClientConfig {
    pub async fn load(path: &Path) -> Result<Self, CoreError> {
        load_toml::<Self>(path).await
    }

    pub async fn save(&self, path: &Path) -> Result<(), CoreError> {
        save_toml(self, path).await
    }
}

async fn load_toml<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T, CoreError> {
    if !path.exists() {
        return Ok(T::default());
    }
    let text = tokio::fs::read_to_string(path).await?;
    let cfg = toml::from_str(&text)?;
    Ok(cfg)
}

async fn save_toml<T: Serialize>(value: &T, path: &Path) -> Result<(), CoreError> {
    let text = toml::to_string_pretty(value)?;
    write_atomic(path, text.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_config_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");

        let mut cfg = ServerConfig::default();
        cfg.bridge.ip = "10.0.0.1/24".to_owned();
        cfg.networks.push(NetRecord::new("hml-net".to_owned(), Uuid::new_v4()));
        cfg.approved_clients.push(ApprovedClient {
            client_uuid: Uuid::new_v4(),
            net_uuid: cfg.networks[0].uuid,
            tap_ip: "10.0.0.2/24".to_owned(),
            display_name: "router".to_owned(),
            lan_subnets: vec!["192.168.10.0/24".to_owned()],
            admitted: true,
        });

        cfg.save(&path).await.unwrap();
        let loaded = ServerConfig::load(&path).await.unwrap();
        assert_eq!(loaded, cfg);
    }

    #[tokio::test]
    async fn client_config_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.toml");

        let mut cfg = ClientConfig::default();
        cfg.client.uuid = Uuid::new_v4().to_string();
        cfg.client.host = "10.10.10.10".to_owned();
        cfg.client.port = 9999;
        cfg.proxy.enabled = true;
        cfg.tap.ip = "10.0.0.5/24".to_owned();
        cfg.routes.push(WireRoute {
            dst: "10.20.0.0/16".to_owned(),
            via: "10.0.0.1".to_owned(),
        });

        cfg.save(&path).await.unwrap();
        let loaded = ClientConfig::load(&path).await.unwrap();
        assert_eq!(loaded, cfg);
    }

    #[tokio::test]
    async fn missing_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let cfg = ServerConfig::load(&path).await.unwrap();
        assert_eq!(cfg, ServerConfig::default());
    }

    #[tokio::test]
    async fn partial_file_keeps_defaults_for_missing_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.toml");
        // Only specify [server], everything else should default.
        tokio::fs::write(
            &path,
            "[server]\nhost = \"127.0.0.1\"\nport = 7777\nsave_dir = \"./x\"\n",
        )
        .await
        .unwrap();
        let cfg = ServerConfig::load(&path).await.unwrap();
        assert_eq!(cfg.server.port, 7777);
        assert_eq!(cfg.bridge, BridgeConfig::default());
        assert!(cfg.networks.is_empty());
    }
}
