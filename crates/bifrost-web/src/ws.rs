//! WebSocket event stream for the WebUI.
//!
//! Each connection subscribes to the Hub's broadcast channel,
//! JSON-encodes every event, and forwards it as a Text frame.
//! Slow subscribers (browsers in background tabs) receive
//! `RecvError::Lagged(n)` and silently skip — the Hub is never
//! held back by a single subscriber.
//!
//! Plus a 25 s keepalive Ping to keep proxies happy.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use bifrost_core::HubEvent;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, trace};

use crate::state::AppState;

pub async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let events = state.hub.subscribe();
    ws.on_upgrade(move |socket| socket_loop(socket, events))
}

async fn socket_loop(mut socket: WebSocket, mut events: broadcast::Receiver<HubEvent>) {
    let mut keepalive = tokio::time::interval(Duration::from_secs(25));
    keepalive.tick().await; // discard the immediate first fire

    loop {
        tokio::select! {
            biased;

            _ = keepalive.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }

            evt = events.recv() => match evt {
                Ok(e) => {
                    let json = match serde_json::to_string(&e) {
                        Ok(j) => j,
                        Err(err) => {
                            debug!(error = %err, "ws: serialize HubEvent failed");
                            continue;
                        }
                    };
                    trace!(bytes = json.len(), "ws → client");
                    if socket.send(Message::Text(json)).await.is_err() {
                        break;
                    }
                }
                // The Hub broadcast buffer overflowed for this
                // subscriber; skip the missed events.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!(missed = n, "ws: subscriber lagged, dropping");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },

            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(p))) => {
                    if socket.send(Message::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {
                    // Ignore inbound Text/Binary/Pong (no client→server
                    // protocol surface yet).
                    trace!("ws: ignored inbound frame");
                }
                Some(Err(e)) => {
                    debug!(error = %e, "ws: recv err");
                    break;
                }
            }
        }
    }
}
