//! HTTP + WebSocket frontend for the bifrost server.
//!
//! Wraps a [`HubHandle`] in an [`axum`] router and serves it on a
//! TCP listener. By default the server binds to `127.0.0.1:8080`
//! (localhost-only — access from another host requires an SSH tunnel
//! or similar). Auth is intentionally absent in v0.1; the localhost
//! bind *is* the security model.
//!
//! # Endpoints (Phase 1.0)
//!
//! ```text
//! GET  /api/networks
//! GET  /api/networks/:nid/devices
//! ```
//!
//! Future phases add:
//!
//! ```text
//! PATCH /api/networks/:nid/devices/:cid
//! POST  /api/networks/:nid/routes/push
//! GET   /ws
//! ```
//!
//! Static assets and SPA fallback for the React frontend will be
//! mounted at `/` once the frontend ships (1.5).

#![forbid(unsafe_code)]

use std::net::SocketAddr;

use axum::Router;
use bifrost_core::HubHandle;
use tokio::sync::mpsc;
use tracing::{info, warn};

mod api;
mod state;

pub use state::AppState;

/// Run the HTTP server until the listener returns an error or the
/// shutdown channel fires.
///
/// `shutdown` is the same `mpsc::Sender<()>` that
/// `bifrost_server::admin::serve` accepts: it lets HTTP- triggered
/// shutdowns (none in 1.0, but coming) bubble up to `main`.
pub async fn serve(
    addr: SocketAddr,
    hub: HubHandle,
    _shutdown: mpsc::Sender<()>,
) -> anyhow::Result<()> {
    let state = AppState { hub };

    let app = Router::new()
        .nest("/api", api::router())
        .with_state(state)
        .layer(tower_http::trace::TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "web server listening");

    if let Err(e) = axum::serve(listener, app).await {
        warn!(error = %e, "web server stopped");
    }
    Ok(())
}
