//! `clap`-derived argument parsing.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Bifrost VPN server.
#[derive(Debug, Parser)]
#[command(name = "bifrost-server", version, about)]
pub struct Cli {
    /// Path to the server TOML config (created on first run).
    ///
    /// Default points at the standard systemd-deployed location so
    /// `bifrost-server admin <cmd>` works with no flags on the same
    /// host as the daemon. For dev runs, pass `--config ./server.toml`
    /// (or wherever).
    #[arg(long, default_value = "/etc/bifrost/server.toml", global = true)]
    pub config: PathBuf,

    /// Override `[admin] socket` from the config.
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,

    /// Override `[web] listen` from the config (e.g. `127.0.0.1:8080`).
    /// Daemon mode only; ignored for subcommands.
    #[arg(long)]
    pub web_listen: Option<String>,

    /// Disable the WebUI HTTP server even if `[web] enabled = true` in
    /// the config. Daemon mode only.
    #[arg(long)]
    pub no_web: bool,

    /// Also run an interactive REPL on stdin (default: daemon-only).
    /// Ignored when a subcommand is present.
    #[arg(long, global = true)]
    pub repl: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Connect to a running daemon's admin socket and run one command.
    Admin {
        #[command(subcommand)]
        cmd: AdminCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum AdminCmd {
    /// Create a virtual network and persist it.
    Mknet {
        name: String,
    },
    /// Rename an existing network.
    Rename {
        net_uuid: uuid::Uuid,
        name: String,
    },
    /// Delete a network and cascade-remove its devices.
    Rmnet {
        net_uuid: uuid::Uuid,
    },
    /// Inspect or mutate per-device state.
    Device {
        #[command(subcommand)]
        action: DeviceAction,
    },
    /// Show the current daemon snapshot.
    List,
    /// Broadcast a text message to every connected client.
    Send {
        msg: String,
    },
    /// Read a local file and broadcast it to every connected client.
    Sendfile {
        path: PathBuf,
    },
    /// Tell the daemon to exit cleanly.
    Shutdown,
}

#[derive(Debug, Subcommand)]
pub enum DeviceAction {
    /// List devices, optionally filtered by network.
    List {
        net_uuid: Option<uuid::Uuid>,
    },
    /// Mutate one approved-client row.
    Set {
        client_uuid: uuid::Uuid,
        /// Friendly display name. Empty string clears.
        #[arg(long)]
        name: Option<String>,
        /// `true` admits, `false` revokes admission.
        #[arg(long)]
        admit: Option<bool>,
        /// TAP IP/CIDR for the client, e.g. `10.0.0.5/24`. Empty clears.
        #[arg(long)]
        ip: Option<String>,
        /// LAN subnets reachable through this device, comma-separated.
        /// Pass `--lan ""` to clear.
        #[arg(long, value_delimiter = ',')]
        lan: Option<Vec<String>>,
    },
    /// Re-derive routes for a network and push to all members.
    Push {
        net_uuid: uuid::Uuid,
    },
}
