//! Admin Unix socket — accepts one request per connection from
//! `bifrost-server admin <cmd>` invocations and routes through
//! [`crate::dispatch::dispatch`].

use std::path::{Path, PathBuf};

use bifrost_core::HubHandle;
use bifrost_proto::admin::{read_admin, write_admin, ServerAdminReq, ServerAdminResp};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::dispatch::dispatch;

/// Run the admin socket listener. Returns when the listener errors.
///
/// Sends a single message into `shutdown_tx` if a client issues a
/// `Shutdown` request, so the main task can exit cleanly.
pub async fn serve(
    socket: PathBuf,
    hub: HubHandle,
    shutdown_tx: mpsc::Sender<()>,
) -> std::io::Result<()> {
    bind_unix_socket(&socket).await?;
    let listener = UnixListener::bind(&socket)?;
    // Tighten permissions to owner-only.
    set_socket_mode(&socket).await.ok();
    info!(path = %socket.display(), "admin socket listening");

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "admin accept failed");
                continue;
            }
        };
        let hub = hub.clone();
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_one(stream, hub, shutdown_tx).await {
                debug!(error = %e, "admin client handler ended");
            }
        });
    }
}

/// Process one accepted admin connection.
async fn handle_one(
    mut stream: UnixStream,
    hub: HubHandle,
    shutdown_tx: mpsc::Sender<()>,
) -> Result<(), bifrost_proto::admin::AdminError> {
    let req: ServerAdminReq = read_admin(&mut stream).await?;
    let is_shutdown = matches!(req, ServerAdminReq::Shutdown);
    let resp = dispatch(&hub, req).await;
    write_admin(&mut stream, &resp).await?;
    // Drain & close gracefully.
    use tokio::io::AsyncWriteExt;
    let _ = stream.shutdown().await;
    if is_shutdown {
        let _ = shutdown_tx.send(()).await;
    }
    Ok(())
}

/// Send one [`ServerAdminReq`] over a fresh connection and read the
/// response. Used by the `admin` CLI subcommand.
pub async fn round_trip(
    socket: &Path,
    req: ServerAdminReq,
) -> Result<ServerAdminResp, bifrost_proto::admin::AdminError> {
    let mut stream = UnixStream::connect(socket).await?;
    write_admin(&mut stream, &req).await?;
    let resp = read_admin(&mut stream).await?;
    Ok(resp)
}

/// Remove a stale socket file (e.g. from a previous run that crashed
/// without cleanup). Best-effort.
async fn bind_unix_socket(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        // Try to connect — if it succeeds, another instance is alive
        // and we refuse to clobber it.
        if UnixStream::connect(path).await.is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("socket {path:?} appears to be in use by another instance"),
            ));
        }
        // Stale — remove it.
        if let Err(e) = tokio::fs::remove_file(path).await {
            error!(path = %path.display(), error = %e, "failed to remove stale socket");
        }
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn set_socket_mode(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = tokio::fs::metadata(path).await?.permissions();
    perms.set_mode(0o600);
    tokio::fs::set_permissions(path, perms).await
}

#[cfg(not(unix))]
async fn set_socket_mode(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
