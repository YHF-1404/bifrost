//! Admin Unix socket — accepts one [`ClientAdminReq`] per connection
//! and dispatches via [`crate::dispatch::dispatch`].

use std::path::{Path, PathBuf};

use bifrost_proto::admin::{read_admin, write_admin, ClientAdminReq, ClientAdminResp};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::dispatch::dispatch;
use crate::repl::UserCmd;

pub async fn serve(
    socket: PathBuf,
    user_tx: mpsc::Sender<UserCmd>,
    shutdown_tx: mpsc::Sender<()>,
) -> std::io::Result<()> {
    bind_unix_socket(&socket).await?;
    let listener = UnixListener::bind(&socket)?;
    set_socket_mode(&socket).await.ok();
    info!(path = %socket.display(), "admin socket listening");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "admin accept failed");
                continue;
            }
        };
        let user_tx = user_tx.clone();
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_one(stream, user_tx, shutdown_tx).await {
                debug!(error = %e, "admin client handler ended");
            }
        });
    }
}

async fn handle_one(
    mut stream: UnixStream,
    user_tx: mpsc::Sender<UserCmd>,
    shutdown_tx: mpsc::Sender<()>,
) -> Result<(), bifrost_proto::admin::AdminError> {
    let req: ClientAdminReq = read_admin(&mut stream).await?;
    let is_shutdown = matches!(req, ClientAdminReq::Shutdown);
    let resp = dispatch(&user_tx, req).await;
    write_admin(&mut stream, &resp).await?;
    use tokio::io::AsyncWriteExt;
    let _ = stream.shutdown().await;
    if is_shutdown {
        let _ = shutdown_tx.send(()).await;
    }
    Ok(())
}

/// Send one [`ClientAdminReq`] over a fresh connection and read the
/// reply.
pub async fn round_trip(
    socket: &Path,
    req: ClientAdminReq,
) -> Result<ClientAdminResp, bifrost_proto::admin::AdminError> {
    let mut stream = UnixStream::connect(socket).await?;
    write_admin(&mut stream, &req).await?;
    let resp = read_admin(&mut stream).await?;
    Ok(resp)
}

async fn bind_unix_socket(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("socket {path:?} appears in use by another instance"),
            ));
        }
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
async fn set_socket_mode(_: &Path) -> std::io::Result<()> {
    Ok(())
}
