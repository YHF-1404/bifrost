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

    #[serde(default, rename = "routes", skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<WireRoute>,
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
            networks: Vec::new(),
            approved_clients: Vec::new(),
            routes: Vec::new(),
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedClient {
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
    /// Persisted TAP IP/CIDR for this `(client, net)` pair. Empty = unset.
    #[serde(default)]
    pub tap_ip: String,
}

/// Route as it appears on disk and on the wire — strings only, validated
/// at the platform layer (see `bifrost_net::RouteEntry::parse`).
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
        load_toml::<Self>(path).await
    }

    pub async fn save(&self, path: &Path) -> Result<(), CoreError> {
        save_toml(self, path).await
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
        cfg.networks.push(NetRecord {
            name: "hml-net".to_owned(),
            uuid: Uuid::new_v4(),
        });
        cfg.approved_clients.push(ApprovedClient {
            client_uuid: Uuid::new_v4(),
            net_uuid: cfg.networks[0].uuid,
            tap_ip: "10.0.0.2/24".to_owned(),
        });
        cfg.routes.push(WireRoute {
            dst: "192.168.10.0/24".to_owned(),
            via: "10.0.0.2".to_owned(),
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
