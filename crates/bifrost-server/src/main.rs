//! `bifrost-server` binary entry point.
//!
//! Two modes:
//!
//! * **Daemon** (default) — load config, start hub + accept loop +
//!   admin Unix socket; optionally also run an interactive REPL when
//!   `--repl` is on.
//! * **Admin client** — `bifrost-server admin <cmd>` connects to a
//!   running daemon's socket, sends one request, prints the response.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bifrost_core::config::ServerConfig;
use bifrost_core::{Hub, HubHandle};
use bifrost_net::Platform;
use bifrost_proto::admin::ServerAdminReq;
use bifrost_server::cli::{AdminCmd, Cli, Command, DeviceAction};
use bifrost_server::dispatch::{dispatch, format_resp};
use bifrost_server::repl::ReplCmd;
use bifrost_server::{accept, admin, repl};
use clap::Parser;
use tokio::sync::{mpsc, oneshot};
use tracing_subscriber::{fmt, EnvFilter};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Cli::parse();
    let Cli {
        config,
        socket,
        web_listen,
        no_web,
        repl,
        command,
    } = args;

    match command {
        Some(Command::Admin { cmd }) => {
            let socket = resolve_admin_socket(socket, &config).await?;
            admin_client(socket, cmd).await
        }
        None => {
            let cfg = ServerConfig::load(&config)
                .await
                .with_context(|| format!("load {config:?}"))?;
            run_daemon(cfg, config, socket, web_listen, no_web, repl).await
        }
    }
}

async fn resolve_admin_socket(socket: Option<PathBuf>, config: &PathBuf) -> Result<PathBuf> {
    if let Some(s) = socket {
        return Ok(s);
    }
    let cfg = ServerConfig::load(config)
        .await
        .with_context(|| format!("load {config:?} (use --socket to skip)"))?;
    Ok(PathBuf::from(&cfg.admin.socket))
}

// ── Admin subcommand ──────────────────────────────────────────────────────

async fn admin_client(socket: PathBuf, cmd: AdminCmd) -> Result<()> {
    let req = match cmd {
        AdminCmd::Mknet { name, ip } => ServerAdminReq::MakeNet {
            name,
            bridge_ip: ip,
        },
        AdminCmd::Rename { net_uuid, name } => ServerAdminReq::RenameNet { net_uuid, name },
        AdminCmd::Rmnet { net_uuid } => ServerAdminReq::DeleteNet { net_uuid },
        AdminCmd::Device { action } => match action {
            DeviceAction::List { net_uuid } => ServerAdminReq::DeviceList { net_uuid },
            DeviceAction::Push { net_uuid } => ServerAdminReq::DevicePush { net_uuid },
            DeviceAction::Set {
                client_uuid,
                name,
                admit,
                ip,
                lan,
            } => ServerAdminReq::DeviceSet {
                client_uuid,
                name,
                admitted: admit,
                tap_ip: ip,
                lan_subnets: lan,
            },
        },
        AdminCmd::Assign { client_uuid, net } => {
            let net_uuid = if net == "none" || net == "-" {
                None
            } else {
                Some(
                    net.parse::<Uuid>()
                        .with_context(|| format!("bad net uuid: {net:?} (use a UUID or `none`)"))?,
                )
            };
            ServerAdminReq::AssignClient {
                client_uuid,
                net_uuid,
            }
        }
        AdminCmd::List => ServerAdminReq::List,
        AdminCmd::Send { msg } => ServerAdminReq::Send { msg },
        AdminCmd::Sendfile { path } => {
            let data =
                tokio::fs::read(&path)
                    .await
                    .with_context(|| format!("read {path:?}"))?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            ServerAdminReq::SendFile { name, data }
        }
        AdminCmd::Shutdown => ServerAdminReq::Shutdown,
    };

    let resp = admin::round_trip(&socket, req)
        .await
        .with_context(|| format!("admin RPC against {:?}", socket))?;
    print!("{}", format_resp(&resp));
    if !format_resp(&resp).ends_with('\n') {
        println!();
    }
    Ok(())
}

// ── Daemon mode ───────────────────────────────────────────────────────────

async fn run_daemon(
    mut cfg: ServerConfig,
    cfg_path: PathBuf,
    socket_override: Option<PathBuf>,
    web_listen_override: Option<String>,
    no_web: bool,
    enable_repl: bool,
) -> Result<()> {
    if let Some(s) = &socket_override {
        cfg.admin.socket = s.to_string_lossy().into_owned();
    }
    if let Some(addr) = &web_listen_override {
        cfg.web.listen = addr.clone();
    }
    if no_web {
        cfg.web.enabled = false;
    }
    cfg.save(&cfg_path).await.ok(); // canonicalise / create on first run

    let server_id = Uuid::new_v4();
    let listen = format!("{}:{}", cfg.server.host, cfg.server.port);
    let save_dir = PathBuf::from(&cfg.server.save_dir);
    let admin_socket = PathBuf::from(&cfg.admin.socket);
    let web_cfg = cfg.web.clone();

    println!("[*] server id: {server_id}");
    println!("[*] listen:    {listen}");
    println!("[*] networks:  {}", cfg.networks.len());
    for net in &cfg.networks {
        println!(
            "    └─ {} ({}) bridge={}{}",
            net.name,
            net.uuid,
            net.bridge_name,
            if net.bridge_ip.is_empty() {
                String::new()
            } else {
                format!(" ip={}", net.bridge_ip)
            }
        );
    }
    println!("[*] save dir:  {}", save_dir.display());
    println!("[*] admin:     {}", admin_socket.display());
    if web_cfg.enabled {
        println!("[*] web:       http://{}", web_cfg.listen);
    }

    // Phase 2: bridges are per-network. Hub creates them itself during
    // startup (and on each `mknet` thereafter); main no longer prepares
    // a single global bridge before instantiating the Hub.
    let platform: Arc<dyn Platform> = build_platform()?;

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("bind {listen}"))?;

    let (hub, hub_handle) = Hub::new(cfg.clone(), Some(cfg_path.clone()), platform);
    let hub_join = tokio::spawn(hub.run());

    tokio::spawn(accept::run(
        listener,
        hub_handle.clone(),
        server_id,
        save_dir.clone(),
    ));

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(2);
    tokio::spawn({
        let hub = hub_handle.clone();
        let st = shutdown_tx.clone();
        let socket = admin_socket.clone();
        async move {
            if let Err(e) = admin::serve(socket, hub, st).await {
                tracing::error!(error = %e, "admin socket loop ended");
            }
        }
    });

    // Optional WebUI HTTP server. Failure to bind is logged but does
    // not abort the daemon — the rest of the server stays useful.
    if web_cfg.enabled {
        match web_cfg.listen.parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                let hub = hub_handle.clone();
                let st = shutdown_tx.clone();
                let state_dir = save_dir.clone();
                tokio::spawn(async move {
                    if let Err(e) = bifrost_web::serve(addr, hub, state_dir, st).await {
                        tracing::error!(error = %e, "web server stopped");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, listen = %web_cfg.listen, "bad [web] listen; web disabled");
            }
        }
    }

    if enable_repl {
        let (req_tx, req_rx) = mpsc::channel::<(ServerAdminReq, oneshot::Sender<String>)>(8);
        std::thread::spawn(move || repl::run_blocking(req_tx));
        tokio::spawn(repl_pump(hub_handle.clone(), req_rx, shutdown_tx.clone()));
    }

    // Wait for either an admin shutdown or a SIGINT/SIGTERM.
    tokio::select! {
        _ = shutdown_rx.recv() => println!("[*] shutdown requested"),
        _ = tokio::signal::ctrl_c() => println!("[*] ctrl-c"),
    }

    hub_handle.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(3), hub_join).await;
    let _ = tokio::fs::remove_file(&admin_socket).await;
    println!("[*] bye");
    Ok(())
}

/// Pump REPL-emitted commands through the same dispatcher the admin
/// socket uses, then send the rendered response back to the REPL via
/// the oneshot channel.
async fn repl_pump(
    hub: HubHandle,
    mut req_rx: mpsc::Receiver<(ServerAdminReq, oneshot::Sender<String>)>,
    shutdown_tx: mpsc::Sender<()>,
) {
    while let Some((req, ack)) = req_rx.recv().await {
        let is_shutdown = matches!(req, ServerAdminReq::Shutdown);
        let resp = dispatch(&hub, req).await;
        let rendered = format_resp(&resp);
        let _ = ack.send(rendered);
        if is_shutdown {
            let _ = shutdown_tx.send(()).await;
            break;
        }
    }
}

// Wire the cli enums up.
#[allow(dead_code)]
fn _link(_: &ReplCmd) {}

#[cfg(target_os = "linux")]
fn build_platform() -> Result<Arc<dyn Platform>> {
    let platform = bifrost_net::LinuxPlatform::new().context("rtnetlink connection")?;
    Ok(Arc::new(platform))
}

#[cfg(not(target_os = "linux"))]
fn build_platform() -> Result<Arc<dyn Platform>> {
    println!("[!] non-Linux build — using NullPlatform; JOIN will fail at runtime.");
    Ok(Arc::new(bifrost_net::NullPlatform))
}
