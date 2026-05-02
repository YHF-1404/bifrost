//! `bifrost-client` binary entry point.
//!
//! Default = daemon (no REPL, opens admin Unix socket); `--repl` adds
//! an interactive prompt; `bifrost-client admin <cmd>` is a one-shot
//! RPC into a running daemon's admin socket.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use bifrost_client::app::{App, AppPorts};
use bifrost_client::cli::{AdminCmd, Cli, Command};
use bifrost_client::conn::ConnTask;
use bifrost_client::dispatch::format_resp;
use bifrost_client::{admin, repl};
use bifrost_core::config::ClientConfig;
use bifrost_proto::admin::ClientAdminReq;
use clap::Parser;
use tokio::sync::mpsc;
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
        no_proxy,
        socket,
        repl,
        command,
    } = args;

    match command {
        // Admin path is config-light: never generates a UUID, never
        // writes to disk. If `--socket` is given, that's all we need;
        // otherwise we resolve via config.
        Some(Command::Admin { cmd }) => {
            let socket = resolve_admin_socket(socket, &config).await?;
            admin_client(socket, cmd).await
        }
        None => {
            let mut cfg = ClientConfig::load(&config).await?;
            if no_proxy {
                cfg.proxy.enabled = false;
            }
            if cfg.client.uuid.is_empty() {
                cfg.client.uuid = Uuid::new_v4().to_string();
                cfg.save(&config).await?;
                println!("[*] generated client uuid: {}", cfg.client.uuid);
            }
            run_daemon(cfg, config, socket, repl).await
        }
    }
}

async fn resolve_admin_socket(socket: Option<PathBuf>, config: &PathBuf) -> Result<PathBuf> {
    if let Some(s) = socket {
        return Ok(s);
    }
    let cfg = ClientConfig::load(config)
        .await
        .with_context(|| format!("load {config:?} (use --socket to skip)"))?;
    Ok(PathBuf::from(&cfg.admin.socket))
}

async fn admin_client(socket: PathBuf, cmd: AdminCmd) -> Result<()> {
    let req = match cmd {
        AdminCmd::Join { net_uuid } => ClientAdminReq::Join { net_uuid },
        AdminCmd::Leave => ClientAdminReq::Leave,
        AdminCmd::Status => ClientAdminReq::Status,
        AdminCmd::Send { msg } => ClientAdminReq::Send { msg },
        AdminCmd::Sendfile { path } => {
            let data = tokio::fs::read(&path)
                .await
                .with_context(|| format!("read {path:?}"))?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            ClientAdminReq::SendFile { name, data }
        }
        AdminCmd::Shutdown => ClientAdminReq::Shutdown,
    };

    let resp = admin::round_trip(&socket, req)
        .await
        .with_context(|| format!("admin RPC against {socket:?}"))?;
    let rendered = format_resp(&resp);
    print!("{rendered}");
    if !rendered.ends_with('\n') {
        println!();
    }
    Ok(())
}

async fn run_daemon(
    mut cfg: ClientConfig,
    cfg_path: PathBuf,
    socket_override: Option<PathBuf>,
    enable_repl: bool,
) -> Result<()> {
    if let Some(s) = &socket_override {
        cfg.admin.socket = s.to_string_lossy().into_owned();
    }
    let admin_socket = PathBuf::from(&cfg.admin.socket);

    println!("[*] client uuid: {}", cfg.client.uuid);
    print!("[*] target: {}:{}", cfg.client.host, cfg.client.port);
    if cfg.proxy.enabled {
        println!(" via SOCKS5 {}:{}", cfg.proxy.host, cfg.proxy.port);
    } else {
        println!(" (direct)");
    }
    println!("[*] admin: {}", admin_socket.display());

    let platform: Arc<dyn bifrost_net::Platform> = build_platform()?;

    let (out_tx, out_rx) = mpsc::channel(128);
    let (events_tx, events_rx) = mpsc::channel(128);
    let (user_tx, user_rx) = mpsc::channel::<bifrost_client::repl::UserCmd>(64);

    tokio::spawn(ConnTask::new(cfg.clone(), out_rx, events_tx).run());

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(2);
    {
        let user_tx = user_tx.clone();
        let shutdown_tx = shutdown_tx.clone();
        let socket = admin_socket.clone();
        tokio::spawn(async move {
            if let Err(e) = admin::serve(socket, user_tx, shutdown_tx).await {
                tracing::error!(error = %e, "admin socket loop ended");
            }
        });
    }

    if enable_repl {
        let user_tx = user_tx.clone();
        std::thread::spawn(move || repl::run(user_tx));
    }

    let app = App::new(AppPorts {
        cfg,
        cfg_path,
        platform,
        out_tx,
        events_rx,
        user_rx,
    });
    let app_join = tokio::spawn(app.run());

    tokio::select! {
        _ = shutdown_rx.recv() => println!("[*] shutdown requested"),
        _ = tokio::signal::ctrl_c() => println!("[*] ctrl-c"),
        _ = wait_app_done(&user_tx) => {} // app exited (REPL Quit)
    }

    // Tell App to quit if not already.
    let _ = user_tx.send(bifrost_client::repl::UserCmd::Quit).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), app_join).await;
    let _ = tokio::fs::remove_file(&admin_socket).await;
    println!("[*] bye");
    Ok(())
}

/// Resolves when the user_tx receiver (App) is dropped, signalling the
/// app exited. We watch via `closed()`.
async fn wait_app_done(user_tx: &mpsc::Sender<bifrost_client::repl::UserCmd>) {
    user_tx.closed().await;
}

#[cfg(target_os = "linux")]
fn build_platform() -> Result<Arc<dyn bifrost_net::Platform>> {
    let platform = bifrost_net::LinuxPlatform::new()
        .map_err(|e| anyhow::anyhow!("rtnetlink connection: {e}"))?;
    Ok(Arc::new(platform))
}

#[cfg(not(target_os = "linux"))]
fn build_platform() -> Result<Arc<dyn bifrost_net::Platform>> {
    println!("[!] non-Linux build — using NullPlatform; join will fail at runtime.");
    Ok(Arc::new(bifrost_net::NullPlatform))
}
