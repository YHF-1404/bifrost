//! `clap`-derived argument parsing for the `bifrost-client` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use uuid::Uuid;

/// Bifrost VPN client.
#[derive(Debug, Parser)]
#[command(name = "bifrost-client", version, about)]
pub struct Cli {
    /// Path to the client TOML config (created on first run).
    ///
    /// Default points at the standard systemd-deployed location so
    /// `bifrost-client admin <cmd>` works with no flags on the same
    /// host as the daemon. For dev runs, pass `--config ./client.toml`
    /// (or wherever).
    #[arg(long, default_value = "/etc/bifrost/client.toml", global = true)]
    pub config: PathBuf,

    /// Disable the SOCKS5 proxy even if `proxy.enabled = true` in the config.
    #[arg(long, global = true)]
    pub no_proxy: bool,

    /// Override `[admin] socket` from the config.
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,

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
    /// Request to join a virtual network by UUID.
    Join { net_uuid: Uuid },
    /// Leave the current network (destroys local TAP).
    Leave,
    /// Show client status snapshot.
    Status,
    /// Send a text broadcast to the server.
    Send { msg: String },
    /// Read a local file and ship it to the server as a Frame::File.
    Sendfile { path: PathBuf },
    /// Tell the daemon to exit cleanly.
    Shutdown,
}
