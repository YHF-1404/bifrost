//! REST endpoints under `/api`.
//!
//! ```text
//!   GET    /networks
//!   POST   /networks
//!   PATCH  /networks/:nid
//!   DELETE /networks/:nid
//!   GET    /networks/:nid/devices
//!   PATCH  /networks/:nid/devices/:cid
//!   POST   /networks/:nid/routes/push
//! ```
//!
//! Admit / kick are not separate endpoints — they're a field on PATCH:
//! `{ "admitted": true }` admits, `{ "admitted": false }` kicks the
//! device back to pending state without removing its row.
//!
//! Errors share a small JSON envelope: `{ "error": "<message>" }`.
//! 4xx is for caller-fixable mistakes (unknown network, invalid CIDR,
//! IP collision, no pending session); 5xx is reserved for hub failure.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use bifrost_core::{DeviceSetResult, DeviceUpdate};
use bifrost_proto::admin::DeviceEntry;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/networks", get(list_networks).post(create_network))
        .route(
            "/networks/:nid",
            patch(rename_network).delete(delete_network),
        )
        .route("/networks/:nid/devices", get(list_devices))
        .route("/networks/:nid/devices/:cid", patch(patch_device))
        .route("/networks/:nid/routes/push", post(push_routes))
}

// ── GET handlers (unchanged from 1.1) ─────────────────────────────────────

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

async fn list_networks(State(state): State<AppState>) -> Response {
    let snap = match state.hub.list().await {
        Some(s) => s,
        None => return service_unavailable("hub gone"),
    };
    let devices = state.hub.device_list(None).await;

    // 1.x will plumb bridge_name/bridge_ip through HubHandle.
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

/// `POST /api/networks` body — `{ "name": "..." }`.
#[derive(Debug, Deserialize)]
struct CreateNetworkBody {
    name: String,
}

#[derive(Debug, Serialize)]
struct CreateNetworkResp {
    id: Uuid,
    name: String,
}

/// `POST /api/networks` — create a new virtual network. Empty names
/// are rejected (the WebUI shouldn't send them, but mirror the
/// validation here).
async fn create_network(
    State(state): State<AppState>,
    Json(body): Json<CreateNetworkBody>,
) -> Response {
    let trimmed = body.name.trim().to_string();
    if trimmed.is_empty() {
        return bad_request("name is required");
    }
    let Some(uuid) = state.hub.make_net(trimmed.clone()).await else {
        return service_unavailable("hub gone");
    };
    Json(CreateNetworkResp {
        id: uuid,
        name: trimmed,
    })
    .into_response()
}

/// `PATCH /api/networks/:nid` body — `{ "name": "..." }`. Only the
/// name is renameable for now; other config fields are config-file
/// territory.
#[derive(Debug, Deserialize)]
struct RenameNetworkBody {
    name: String,
}

async fn rename_network(
    State(state): State<AppState>,
    Path(nid): Path<Uuid>,
    Json(body): Json<RenameNetworkBody>,
) -> Response {
    let trimmed = body.name.trim().to_string();
    if trimmed.is_empty() {
        return bad_request("name is required");
    }
    if !state.hub.rename_net(nid, trimmed.clone()).await {
        return not_found("unknown network");
    }
    Json(serde_json::json!({ "id": nid, "name": trimmed })).into_response()
}

/// `DELETE /api/networks/:nid` — cascade-delete the network and every
/// admitted/pending device row in it. Returns 204 on success.
async fn delete_network(State(state): State<AppState>, Path(nid): Path<Uuid>) -> Response {
    if !state.hub.delete_net(nid).await {
        return not_found("unknown network");
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn list_devices(State(state): State<AppState>, Path(nid): Path<Uuid>) -> Response {
    let snap = match state.hub.list().await {
        Some(s) => s,
        None => return service_unavailable("hub gone"),
    };
    if !snap.networks.iter().any(|n| n.uuid == nid) {
        return not_found("unknown network");
    }
    let devices = state.hub.device_list(Some(nid)).await;
    Json(devices).into_response()
}

// ── Write handlers ────────────────────────────────────────────────────────

/// Body of `PATCH /networks/:nid/devices/:cid`. Every field is
/// optional (= "leave alone"); empty string in `tap_ip` clears it,
/// empty array in `lan_subnets` clears the list. `admitted: false`
/// removes the device from the network.
#[derive(Debug, Deserialize)]
struct DeviceUpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    admitted: Option<bool>,
    #[serde(default)]
    tap_ip: Option<String>,
    #[serde(default)]
    lan_subnets: Option<Vec<String>>,
}

/// `PATCH /api/networks/:nid/devices/:cid` — mutate one device row.
/// `admitted=true` promotes a pending row (and admits any waiting
/// conn); `admitted=false` kicks an active session, dropping its
/// conn and leaving the row in the list with admitted=false. Other
/// fields edit metadata regardless of admit state.
async fn patch_device(
    State(state): State<AppState>,
    Path((nid, cid)): Path<(Uuid, Uuid)>,
    Json(body): Json<DeviceUpdateBody>,
) -> Response {
    if !network_exists(&state, nid).await {
        return not_found("unknown network");
    }
    let update = DeviceUpdate {
        name: body.name,
        admitted: body.admitted,
        tap_ip: body.tap_ip,
        lan_subnets: body.lan_subnets,
    };
    match state.hub.device_set(cid, nid, update).await {
        DeviceSetResult::Ok(d) => Json(d).into_response(),
        DeviceSetResult::NotFound => not_found("no such device in this network"),
        DeviceSetResult::InvalidIp => bad_request("invalid IP/CIDR"),
        DeviceSetResult::Conflict { msg } => conflict(msg),
    }
}

/// Body of `POST /api/networks/:nid/routes/push`. Empty for now; left
/// as an explicit struct so a future `dry_run` flag is additive.
#[derive(Debug, Serialize)]
struct PushRoutesResp {
    count: u64,
    routes: Vec<RouteRow>,
}

#[derive(Debug, Serialize)]
struct RouteRow {
    dst: String,
    via: String,
}

/// `POST /api/networks/:nid/routes/push` — re-derive routes from
/// `lan_subnets` and push to all joined peers in this network.
async fn push_routes(State(state): State<AppState>, Path(nid): Path<Uuid>) -> Response {
    if !network_exists(&state, nid).await {
        return not_found("unknown network");
    }
    let r = state.hub.device_push(nid).await;
    Json(PushRoutesResp {
        count: r.count,
        routes: r
            .routes
            .into_iter()
            .map(|w| RouteRow { dst: w.dst, via: w.via })
            .collect(),
    })
    .into_response()
}

// ── Small helpers ─────────────────────────────────────────────────────────

async fn network_exists(state: &AppState, nid: Uuid) -> bool {
    state
        .hub
        .list()
        .await
        .is_some_and(|s| s.networks.iter().any(|n| n.uuid == nid))
}

fn service_unavailable(msg: &str) -> Response {
    err(StatusCode::SERVICE_UNAVAILABLE, msg)
}

fn not_found(msg: &str) -> Response {
    err(StatusCode::NOT_FOUND, msg)
}

fn bad_request(msg: &str) -> Response {
    err(StatusCode::BAD_REQUEST, msg)
}

fn conflict(msg: impl Into<String>) -> Response {
    let m = msg.into();
    err(StatusCode::CONFLICT, &m)
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}
