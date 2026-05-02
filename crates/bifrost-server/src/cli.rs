//! `clap`-derived argument parsing.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Bifrost VPN server.
#[derive(Debug, Parser)]
#[command(name = "bifrost-server", version, about)]
pub struct Cli {
    /// Path to the server TOML config (created on first run).
    #[arg(long, default_value = "server.toml", global = true)]
    pub config: PathBuf,

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
    /// Create a virtual network and persist it.
    Mknet {
        name: String,
    },
    /// Approve a pending join request by session id.
    Approve {
        sid: u64,
    },
    /// Deny a pending join request by session id.
    Deny {
        sid: u64,
    },
    /// Set TAP IP for a client matched by UUID prefix.
    Setip {
        prefix: String,
        /// Empty string = clear the address.
        ip: String,
    },
    /// Manage the route table.
    Route {
        #[command(subcommand)]
        action: RouteAction,
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
pub enum RouteAction {
    Add { dst: String, via: String },
    Del { dst: String },
    List,
    Push,
}
