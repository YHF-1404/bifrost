//! `ConnTask` — owns the TCP socket and survives reconnects.
//!
//! The task runs forever in two nested loops:
//!
//! ```text
//! outer:                                inner (one connection):
//!   ┌─ try connect ─────────┐             ┌──── select! ────┐
//!   │  fail → wait, retry   │             │  socket → events│
//!   │  ok  → events::Connected            │  out_rx → socket│
//!   │       enter inner    ─┼──────────►  └─────────────────┘
//!   │  inner ended → events::Disconnected
//!   │             → wait, restart
//!   └────────────────────────┘
//! ```
//!
//! Outbound frames are read from `out_rx` and written to the socket
//! while connected; when the socket dies the task re-enters the outer
//! loop and any frames sent during the gap accumulate in the channel
//! until the next reconnect drains them.
//!
//! Shutdown is implicit: when the controller drops `events_rx` (closes
//! the channel), the next `events_tx.send` returns `Err` and the task
//! returns. When the controller drops `out_tx`, the next `out_rx.recv`
//! returns `None` and likewise the task returns.

use std::time::Duration;

use bifrost_core::config::ClientConfig;
use bifrost_proto::{Frame, FrameCodec};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_socks::tcp::Socks5Stream;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

/// Events the connection task pushes to the controller.
#[derive(Debug)]
pub enum ConnEvent {
    /// Socket established.
    Connected,
    /// Socket lost; the task is now waiting before retrying.
    Disconnected,
    /// One frame decoded from the wire.
    FrameIn(Frame),
}

/// Owns the connection lifecycle. Spawn via `tokio::spawn(task.run())`.
pub struct ConnTask {
    cfg: ClientConfig,
    out_rx: mpsc::Receiver<Frame>,
    events_tx: mpsc::Sender<ConnEvent>,
}

impl ConnTask {
    pub fn new(
        cfg: ClientConfig,
        out_rx: mpsc::Receiver<Frame>,
        events_tx: mpsc::Sender<ConnEvent>,
    ) -> Self {
        Self {
            cfg,
            out_rx,
            events_tx,
        }
    }

    pub async fn run(mut self) {
        let retry = Duration::from_secs(self.cfg.client.retry_interval as u64);
        loop {
            // ── outer: connect / reconnect ──────────────────────────
            let stream = match Self::connect(&self.cfg).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "connect failed");
                    tokio::time::sleep(retry).await;
                    continue;
                }
            };
            info!(
                host = self.cfg.client.host,
                port = self.cfg.client.port,
                "connected"
            );
            if self.events_tx.send(ConnEvent::Connected).await.is_err() {
                return; // controller gone
            }

            let mut framed = Framed::new(stream, FrameCodec::new());

            // ── inner: read / write while connected ──────────────────
            loop {
                tokio::select! {
                    incoming = framed.next() => match incoming {
                        Some(Ok(frame)) => {
                            if self.events_tx.send(ConnEvent::FrameIn(frame)).await.is_err() {
                                return;
                            }
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "frame decode failed");
                            break;
                        }
                        None => {
                            debug!("server closed connection");
                            break;
                        }
                    },
                    outgoing = self.out_rx.recv() => match outgoing {
                        Some(frame) => {
                            // Per-frame send (= feed + flush). With
                            // TCP_NODELAY each frame becomes its own
                            // small TCP segment immediately. Earlier we
                            // batched (drain channel into one flush) to
                            // save syscalls — but that creates large
                            // bursts that overwhelm a slow downstream
                            // proxy (e.g. xray-core), causing its ACK
                            // window to stall and our outer TCP to drop
                            // cwnd to 10. Per-frame writes give the
                            // proxy a steady, paced stream.
                            if let Err(e) = framed.send(frame).await {
                                warn!(error = %e, "socket write failed");
                                break;
                            }
                        }
                        None => return,  // controller dropped out_tx
                    }
                }
            }

            if self.events_tx.send(ConnEvent::Disconnected).await.is_err() {
                return;
            }
            tokio::time::sleep(retry).await;
        }
    }

    /// Open a TCP stream to the configured server, optionally via SOCKS5.
    ///
    /// `Socks5Stream::into_inner` returns the underlying `TcpStream`
    /// after the handshake completes; for plain CONNECT mode this is
    /// transparent forwarding, so we can use the raw stream from then
    /// on and avoid threading a generic type all the way through.
    async fn connect(cfg: &ClientConfig) -> anyhow::Result<TcpStream> {
        let target = (cfg.client.host.as_str(), cfg.client.port);
        let stream = if cfg.proxy.enabled {
            let s = Socks5Stream::connect(
                (cfg.proxy.host.as_str(), cfg.proxy.port),
                target,
            )
            .await?;
            s.into_inner()
        } else {
            TcpStream::connect(target).await?
        };
        // Disable Nagle so per-frame writes don't get held waiting for
        // the previous segment's ACK. See accept.rs for the matching
        // server-side rationale.
        if let Err(e) = stream.set_nodelay(true) {
            warn!(error = %e, "set_nodelay failed (continuing)");
        }
        // Bound kernel SNDBUF — see accept.rs. Through nested TCP
        // tunnels (xray etc.) the auto-tuned multi-MB buffer hides
        // congestion from the inner TCP, which then keeps growing
        // its cwnd until bulk throughput collapses to near zero.
        #[cfg(target_os = "linux")]
        if let Err(e) = bifrost_net::set_send_buffer_size(&stream, 256 * 1024) {
            warn!(error = %e, "set_send_buffer_size failed (continuing)");
        }
        Ok(stream)
    }
}
