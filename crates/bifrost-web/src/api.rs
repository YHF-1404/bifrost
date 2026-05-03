//! REST endpoints under `/api`.
//!
//! ```text
//!   GET    /networks
//!   GET    /networks/:nid/devices
//!   PATCH  /networks/:nid/devices/:cid
//!   POST   /networks/:nid/devices/:cid/approve
//!   POST   /networks/:nid/devices/:cid/deny
//!   POST   /networks/:nid/routes/push
//! ```
//!
//! Errors share a small JSON envelope: `{ "error": "<message>" }`.
//! 4xx is for caller-fixable mistakes (unknown network, invalid CIDR,
//! IP collision, no pending session); 5xx is reserved for hub failure.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use bifrost_core::{DeviceSetResult, DeviceUpdate, SessionId};
use bifrost_proto::admin::DeviceEntry;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/networks", get(list_networks))
        .route("/networks/:nid/devices", get(list_devices))
        .route("/networks/:nid/devices/:cid", patch(patch_device))
        .route(
            "/networks/:nid/devices/:cid/approve",
            post(approve_device),
        )
        .route("/networks/:nid/devices/:cid/deny", post(deny_device))
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

/// `PATCH /api/networks/:nid/devices/:cid` — mutate one already-admitted
/// device. Pending devices use `/approve` or `/deny` instead.
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

/// `POST /api/networks/:nid/devices/:cid/approve` — admit a currently-
/// pending device. The body is empty; subsequent edits go through
/// `PATCH`. The device must be in the network's pending list.
async fn approve_device(
    State(state): State<AppState>,
    Path((nid, cid)): Path<(Uuid, Uuid)>,
) -> Response {
    if !network_exists(&state, nid).await {
        return not_found("unknown network");
    }
    let sid = match find_pending_sid(&state, nid, cid).await {
        Some(sid) => sid,
        None => return not_found("no pending session for this device"),
    };
    if !state.hub.approve(SessionId(sid)).await {
        return conflict("approve raced — pending session is gone");
    }
    // After approval, return the freshly-admitted device record (may
    // have just landed in the approved_clients list).
    let devs = state.hub.device_list(Some(nid)).await;
    match devs.into_iter().find(|d| d.client_uuid == cid) {
        Some(d) => Json(d).into_response(),
        None => no_content(),
    }
}

/// `POST /api/networks/:nid/devices/:cid/deny` — reject a pending
/// device. No row is created in `approved_clients`.
async fn deny_device(
    State(state): State<AppState>,
    Path((nid, cid)): Path<(Uuid, Uuid)>,
) -> Response {
    if !network_exists(&state, nid).await {
        return not_found("unknown network");
    }
    let sid = match find_pending_sid(&state, nid, cid).await {
        Some(sid) => sid,
        None => return not_found("no pending session for this device"),
    };
    if !state.hub.deny(SessionId(sid)).await {
        return conflict("deny raced — pending session is gone");
    }
    no_content()
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

async fn find_pending_sid(state: &AppState, nid: Uuid, cid: Uuid) -> Option<u64> {
    let snap = state.hub.list().await?;
    snap.pending
        .into_iter()
        .find(|p| p.net_uuid == nid && p.client_uuid == cid)
        .map(|p| p.sid.0)
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

fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}
