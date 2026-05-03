//! REST endpoints under `/api`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use bifrost_proto::admin::DeviceEntry;
use serde::Serialize;
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/networks", get(list_networks))
        .route("/networks/:nid/devices", get(list_devices))
}

/// JSON view of one virtual network. `bridge_*` are global in v0.1
/// (Phase 2 will give each network its own bridge).
#[derive(Debug, Serialize)]
struct Network {
    id: Uuid,
    name: String,
    bridge_name: String,
    bridge_ip: String,
    device_count: usize,
    online_count: usize,
}

/// `GET /api/networks` — list virtual networks plus per-network counts.
async fn list_networks(State(state): State<AppState>) -> impl IntoResponse {
    let snap = match state.hub.list().await {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "hub gone" })),
            )
                .into_response()
        }
    };
    let devices = state.hub.device_list(None).await;

    // The bridge config used to be on `cfg.bridge`; the hub doesn't
    // expose it directly. For 1.0 we fill in placeholders — the
    // WebUI doesn't render these yet. Phase 1.x will plumb a
    // `bridge_info()` accessor through HubHandle.
    let bridge_name = String::new();
    let bridge_ip = String::new();

    let nets: Vec<Network> = snap
        .networks
        .iter()
        .map(|n| {
            let in_net: Vec<&DeviceEntry> =
                devices.iter().filter(|d| d.net_uuid == n.uuid).collect();
            Network {
                id: n.uuid,
                name: n.name.clone(),
                bridge_name: bridge_name.clone(),
                bridge_ip: bridge_ip.clone(),
                device_count: in_net.len(),
                online_count: in_net.iter().filter(|d| d.online).count(),
            }
        })
        .collect();
    Json(nets).into_response()
}

/// `GET /api/networks/:nid/devices` — device list for one network.
async fn list_devices(
    State(state): State<AppState>,
    Path(nid): Path<Uuid>,
) -> impl IntoResponse {
    let snap = match state.hub.list().await {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "hub gone" })),
            )
                .into_response()
        }
    };
    if !snap.networks.iter().any(|n| n.uuid == nid) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown network" })),
        )
            .into_response();
    }
    let devices = state.hub.device_list(Some(nid)).await;
    Json(devices).into_response()
}
