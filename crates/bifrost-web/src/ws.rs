//! WebSocket event stream for the WebUI.
//!
//! Phase 1.1 is intentionally a no-op: the endpoint upgrades and
//! holds the connection open, but emits no events. It exists so the
//! frontend can wire up its connection / reconnect machinery against
//! a real listener. Phase 1.2 lands `metrics.tick`, Phase 1.3 lands
//! `device.online` / `device.offline` / `device.changed`.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use std::time::Duration;
use tracing::debug;

use crate::state::AppState;

pub async fn handler(ws: WebSocketUpgrade, State(_state): State<AppState>) -> Response {
    ws.on_upgrade(socket_loop)
}

/// Minimal protocol for v0.1:
///
/// * Server pings every 25 s; client should `pong` (browsers do this
///   automatically).
/// * Any inbound text frame is logged and ignored.
/// * On `Close` the loop returns; axum drops the socket.
async fn socket_loop(mut socket: WebSocket) {
    let mut tick = tokio::time::interval(Duration::from_secs(25));
    tick.tick().await; // discard the immediate first fire

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(p))) => {
                    if socket.send(Message::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {
                    // Ignore Text/Binary/Pong in 1.1.
                    debug!("ws: ignored inbound frame");
                }
                Some(Err(e)) => {
                    debug!(error = %e, "ws: recv err");
                    break;
                }
            }
        }
    }
}
