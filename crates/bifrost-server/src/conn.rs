//! Per-TCP-connection task.
//!
//! Each accepted socket gets one [`run`] future spawned for it. Its job:
//!
//! 1. Register with the [`bifrost_core::Hub`] and keep its assigned
//!    [`ConnId`].
//! 2. Reply to a [`Frame::Hello`] with [`Frame::HelloAck`] **before**
//!    notifying the hub — the client expects the ack as a synchronous
//!    handshake step.
//! 3. Forward control-plane frames (`Hello` / `Join`) to the hub.
//! 4. Forward Ethernet frames **directly** to the bound session via the
//!    `session_cmd_tx` the hub hands over through `bind_rx` — bypassing
//!    the hub keeps the data plane off the slow control-plane queue.
//! 5. Save received `File` frames into the configured directory.
//! 6. On disconnect, tell the hub via `disconnect`.
//!
//! Generic over the underlying byte stream so integration tests can plug
//! in a `tokio::io::duplex` pipe instead of a real socket.

use std::path::{Path, PathBuf};

use bifrost_core::{ConnLink, HubHandle, SessionCmd};
use bifrost_proto::{Frame, FrameCodec, PROTOCOL_VERSION};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Drive one connection from accept to disconnect.
///
/// Returns when the socket closes, the hub goes away, or a fatal
/// protocol error happens.
pub async fn run<S>(stream: S, addr: String, hub: HubHandle, server_id: Uuid, save_dir: PathBuf)
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (frame_tx, mut frame_rx) = mpsc::channel::<Frame>(128);
    let (bind_tx, mut bind_rx) = mpsc::channel::<Option<mpsc::Sender<SessionCmd>>>(8);
    let conn_id = match hub
        .register_conn(addr.clone(), ConnLink { frame_tx, bind_tx })
        .await
    {
        Some(id) => id,
        None => return,
    };
    info!(?conn_id, %addr, "conn accepted");

    let mut framed = Framed::new(stream, FrameCodec::new());
    let mut session_cmd_tx: Option<mpsc::Sender<SessionCmd>> = None;

    'main: loop {
        tokio::select! {
            biased;

            // Outbound from hub / session → wire.
            outbound = frame_rx.recv() => match outbound {
                Some(frame) => {
                    if let Err(e) = framed.send(frame).await {
                        warn!(?conn_id, error = %e, "socket write failed");
                        break 'main;
                    }
                }
                None => break 'main,  // hub dropped us
            },

            // Hub-driven session binding updates.
            bind = bind_rx.recv() => match bind {
                Some(b) => session_cmd_tx = b,
                None => break 'main,
            },

            // Inbound from wire → control to hub or data to session.
            incoming = framed.next() => match incoming {
                Some(Ok(frame)) => {
                    if !route_inbound(
                        frame,
                        &hub,
                        conn_id,
                        server_id,
                        &session_cmd_tx,
                        &mut framed,
                        &save_dir,
                        &addr,
                    ).await {
                        break 'main;
                    }
                }
                Some(Err(e)) => {
                    warn!(?conn_id, error = %e, "frame decode failed");
                    break 'main;
                }
                None => {
                    debug!(?conn_id, "client closed connection");
                    break 'main;
                }
            }
        }
    }

    info!(?conn_id, %addr, "conn closed");
    hub.disconnect(conn_id).await;
}

/// Dispatch one inbound frame.
///
/// Returns `false` if the connection should be torn down (e.g. version
/// mismatch on Hello).
#[allow(clippy::too_many_arguments)]
async fn route_inbound<S>(
    frame: Frame,
    hub: &HubHandle,
    conn_id: bifrost_core::ConnId,
    server_id: Uuid,
    session_cmd_tx: &Option<mpsc::Sender<SessionCmd>>,
    framed: &mut Framed<S, FrameCodec>,
    save_dir: &Path,
    addr: &str,
) -> bool
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    match frame {
        Frame::Hello {
            version,
            client_uuid,
            ..
        } => {
            if version != PROTOCOL_VERSION {
                warn!(
                    ?conn_id,
                    client_version = version,
                    server_version = PROTOCOL_VERSION,
                    "version mismatch — closing"
                );
                let _ = framed
                    .send(Frame::JoinDeny {
                        reason: format!("version_mismatch:server={PROTOCOL_VERSION}"),
                    })
                    .await;
                return false;
            }
            // Reply HelloAck synchronously so the client doesn't race.
            if let Err(e) = framed
                .send(Frame::HelloAck {
                    version: PROTOCOL_VERSION,
                    server_id,
                    caps: 0,
                })
                .await
            {
                warn!(?conn_id, error = %e, "HelloAck write failed");
                return false;
            }
            hub.hello(conn_id, client_uuid, version).await;
        }

        Frame::Join { net_uuid } => hub.join(conn_id, net_uuid).await,

        Frame::Eth(bytes) => {
            // Direct path: bypass hub.
            if let Some(tx) = session_cmd_tx {
                let _ = tx.send(SessionCmd::EthIn(bytes)).await;
            }
            // else: silently drop — client sent ETH before being bound.
        }

        Frame::Text(s) => println!("[{addr}] > {s}"),

        Frame::File { name, data } => {
            match save_received_file(save_dir, &name, &data).await {
                Ok(p) => println!(
                    "[{addr}] file {name:?} ({} B) → {}",
                    data.len(),
                    p.display()
                ),
                Err(e) => warn!(?conn_id, error = %e, "save file failed"),
            }
        }

        Frame::Ping(nonce) => {
            if let Err(e) = framed.send(Frame::Pong(nonce)).await {
                warn!(?conn_id, error = %e, "pong write failed");
                return false;
            }
        }

        Frame::Pong(_) => {} // unused server-side for now

        // Server-originated frames echoed back by a misbehaving client.
        f @ (Frame::HelloAck { .. }
        | Frame::JoinOk { .. }
        | Frame::JoinDeny { .. }
        | Frame::AssignNet { .. }
        | Frame::SetIp { .. }
        | Frame::SetRoutes(_)) => {
            warn!(?conn_id, ?f, "unexpected server→client frame from client");
        }
    }
    true
}

/// Save `data` under `dir/name`, with `_1`, `_2`, … suffixes to avoid
/// clobbering existing files.
async fn save_received_file(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let mut path = dir.join(name);
    if !path.exists() {
        tokio::fs::write(&path, data).await?;
        return Ok(path);
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut i: u32 = 1;
    loop {
        let candidate = if ext.is_empty() {
            format!("{stem}_{i}")
        } else {
            format!("{stem}_{i}.{ext}")
        };
        path = dir.join(candidate);
        if !path.exists() {
            tokio::fs::write(&path, data).await?;
            return Ok(path);
        }
        i += 1;
    }
}
