//! REST endpoints under `/api`.
//!
//! ```text
//!   GET    /networks
//!   POST   /networks
//!   PATCH  /networks/:nid                       (name and/or bridge_ip)
//!   DELETE /networks/:nid                       (Phase 3: detaches devices to pending)
//!   GET    /networks/:nid/devices
//!   PATCH  /networks/:nid/devices/:cid
//!   POST   /networks/:nid/routes/push
//!   GET    /networks/:nid/layout                (legacy, Phase 1/2 only)
//!   PUT    /networks/:nid/layout                (legacy, Phase 1/2 only)
//!   GET    /clients                             (Phase 3: pending + admitted)
//!   PATCH  /clients/:cid                        (Phase 3: pending edits)
//!   POST   /clients/:cid/assign                 (Phase 3: drag-to-assign)
//!   GET    /ui-layout                           (Phase 3: unified single-file layout)
//!   PUT    /ui-layout                           (Phase 3: unified single-file layout)
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
use bifrost_core::atomic_write::write_atomic;
use bifrost_core::{
    AssignClientResult, DeviceSetResult, DeviceUpdate, SetNetBridgeIpResult,
};
use bifrost_proto::admin::DeviceEntry;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/networks", get(list_networks).post(create_network))
        .route(
            "/networks/:nid",
            patch(patch_network).delete(delete_network),
        )
        .route("/networks/:nid/devices", get(list_devices))
        .route("/networks/:nid/devices/:cid", patch(patch_device))
        .route("/networks/:nid/routes/push", post(push_routes))
        .route(
            "/networks/:nid/layout",
            get(get_layout).put(put_layout),
        )
        .route("/clients", get(list_clients))
        .route("/clients/:cid", patch(patch_client))
        .route("/clients/:cid/assign", post(assign_client))
        .route("/ui-layout", get(get_ui_layout).put(put_ui_layout))
}

// ── GET handlers (unchanged from 1.1) ─────────────────────────────────────

/// JSON view of one virtual network. `bridge_*` are per-network as of
/// Phase 2.0; the WebUI uses `bridge_ip` to render the IP-segment
/// picker and to derive the prefix constraint for client TAP IPs.
/// `routes_dirty` is the initial value of the "push routes" button's
/// pulse state — `true` means the network's derived routes have
/// drifted from what was last pushed (e.g. a new device joined with
/// fresh `lan_subnets`); the WebUI keeps this in sync via the
/// `routes.dirty` WS event after the first paint.
#[derive(Debug, Serialize)]
struct Network {
    id: Uuid,
    name: String,
    bridge_name: String,
    bridge_ip: String,
    device_count: usize,
    online_count: usize,
    routes_dirty: bool,
}

async fn list_networks(State(state): State<AppState>) -> Response {
    let snap = match state.hub.list().await {
        Some(s) => s,
        None => return service_unavailable("hub gone"),
    };
    let devices = state.hub.device_list(None).await;

    let nets: Vec<Network> = snap
        .networks
        .iter()
        .map(|n| {
            let in_net: Vec<&DeviceEntry> = devices
                .iter()
                .filter(|d| d.net_uuid == Some(n.uuid))
                .collect();
            Network {
                id: n.uuid,
                name: n.name.clone(),
                bridge_name: n.bridge_name.clone(),
                bridge_ip: n.bridge_ip.clone(),
                device_count: in_net.len(),
                online_count: in_net.iter().filter(|d| d.online).count(),
                routes_dirty: snap.routes_dirty.contains(&n.uuid),
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
    // The WebUI's create-network flow sets `bridge_ip` as a follow-up
    // PATCH (segment-locked picker on the network card), so we pass
    // `None` here. Programmatic CLI / `bifrost-server admin mknet
    // --ip` callers can pre-populate `bridge_ip` instead.
    match state.hub.make_net(trimmed.clone(), None).await {
        Some(bifrost_core::MakeNetResult::Ok(uuid)) => Json(CreateNetworkResp {
            id: uuid,
            name: trimmed,
        })
        .into_response(),
        Some(bifrost_core::MakeNetResult::InvalidBridgeIp(msg)) => {
            // Cannot happen from this code path (we passed None) but
            // mirror the typed result so adding a body field later is
            // a one-line change.
            bad_request(&format!("invalid bridge_ip: {msg}"))
        }
        None => service_unavailable("hub gone"),
    }
}

/// `PATCH /api/networks/:nid` body. Either or both of `name` and
/// `bridge_ip` may be provided. Empty `bridge_ip` clears it; non-empty
/// must be a `/16` or `/24` CIDR (Phase 3 constraint, B4).
#[derive(Debug, Deserialize)]
struct PatchNetworkBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    bridge_ip: Option<String>,
}

async fn patch_network(
    State(state): State<AppState>,
    Path(nid): Path<Uuid>,
    Json(body): Json<PatchNetworkBody>,
) -> Response {
    if !network_exists(&state, nid).await {
        return not_found("unknown network");
    }
    if let Some(name) = body.name.as_ref() {
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            return bad_request("name is required");
        }
        if !state.hub.rename_net(nid, trimmed).await {
            return not_found("unknown network");
        }
    }
    if let Some(ip) = body.bridge_ip {
        match state.hub.set_net_bridge_ip(nid, ip).await {
            SetNetBridgeIpResult::Ok(_) => {}
            SetNetBridgeIpResult::NotFound => return not_found("unknown network"),
            SetNetBridgeIpResult::Invalid(msg) => return bad_request(&msg),
        }
    }
    // Return the freshly-listed network so the caller sees the merged
    // post-patch view.
    let Some(snap) = state.hub.list().await else {
        return service_unavailable("hub gone");
    };
    let Some(rec) = snap.networks.iter().find(|n| n.uuid == nid) else {
        return not_found("unknown network");
    };
    Json(serde_json::json!({
        "id": rec.uuid,
        "name": rec.name,
        "bridge_name": rec.bridge_name,
        "bridge_ip": rec.bridge_ip,
    }))
    .into_response()
}

/// `DELETE /api/networks/:nid` — Phase 3: detaches every client in
/// this network back to the pending pool (preserving display_name and
/// lan_subnets), tears down the kernel bridge, and removes the
/// network record. Returns 204 on success.
async fn delete_network(State(state): State<AppState>, Path(nid): Path<Uuid>) -> Response {
    if !state.hub.delete_net(nid).await {
        return not_found("unknown network");
    }
    // Best-effort cleanup of the legacy per-network layout file (in
    // case the daemon predates Phase 3) so a future network that
    // happens to reuse this UUID doesn't inherit stale positions.
    let _ = tokio::fs::remove_file(legacy_layout_path(&state, nid)).await;
    // Drop the network's frame box from the unified layout.
    if let Ok(bytes) = tokio::fs::read(ui_layout_path(&state)).await {
        if let Ok(mut layout) = serde_json::from_slice::<UiLayout>(&bytes) {
            if layout.graph.frames.remove(&nid.to_string()).is_some() {
                if let Ok(b) = serde_json::to_vec(&layout) {
                    let _ = write_atomic(&ui_layout_path(&state), &b).await;
                }
            }
        }
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

// ── Phase 3: cross-network client endpoints ──────────────────────────────

/// `GET /api/clients` — list every known client in one shot, both the
/// network-assigned ones (admitted or pending-admit) and the unassigned
/// ones in the pending pool. Used by the unified WebUI to populate
/// both panes from a single fetch.
async fn list_clients(State(state): State<AppState>) -> Response {
    let devices = state.hub.device_list(None).await;
    Json(devices).into_response()
}

/// `PATCH /api/clients/:cid` body. Used by the WebUI to edit metadata
/// of a pending (unassigned) client — name and lan_subnets only;
/// admitted/tap_ip are meaningless without a network. For admitted
/// clients use `PATCH /api/networks/:nid/devices/:cid`.
#[derive(Debug, Deserialize)]
struct PatchClientBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    lan_subnets: Option<Vec<String>>,
}

async fn patch_client(
    State(state): State<AppState>,
    Path(cid): Path<Uuid>,
    Json(body): Json<PatchClientBody>,
) -> Response {
    // Find the client. If admitted, route to device_set; if pending,
    // route to assign_client(same net=None) which preserves the row but
    // updates its fields. Simplest path: for an admitted client we
    // delegate to the existing device_set handler.
    let devices = state.hub.device_list(None).await;
    let Some(d) = devices.iter().find(|d| d.client_uuid == cid) else {
        return not_found("unknown client");
    };

    match d.net_uuid {
        Some(nid) => {
            let update = DeviceUpdate {
                name: body.name,
                admitted: None,
                tap_ip: None,
                lan_subnets: body.lan_subnets,
            };
            match state.hub.device_set(cid, nid, update).await {
                DeviceSetResult::Ok(d) => Json(d).into_response(),
                DeviceSetResult::NotFound => not_found("client gone"),
                DeviceSetResult::InvalidIp => bad_request("invalid IP/CIDR"),
                DeviceSetResult::Conflict { msg } => conflict(msg),
            }
        }
        None => {
            // Pending client — patch the pending_clients row directly.
            // Hub doesn't expose a dedicated command for this (we don't
            // need a whole new HubCmd for two scalar fields), so we
            // round-trip through assign_client(None) after we mutate.
            // Actually the cleanest path: emit a new dedicated command.
            match state
                .hub
                .patch_pending_client(cid, body.name, body.lan_subnets)
                .await
            {
                Some(d) => Json(d).into_response(),
                None => not_found("client gone"),
            }
        }
    }
}

/// `POST /api/clients/:cid/assign` body. `net_uuid: null` detaches the
/// client to the pending pool; `Some(nid)` assigns it to that network
/// (admitted=false, tap_ip cleared, per spec B3).
#[derive(Debug, Deserialize)]
struct AssignClientBody {
    #[serde(default)]
    net_uuid: Option<Uuid>,
}

async fn assign_client(
    State(state): State<AppState>,
    Path(cid): Path<Uuid>,
    Json(body): Json<AssignClientBody>,
) -> Response {
    match state.hub.assign_client(cid, body.net_uuid).await {
        AssignClientResult::Ok(d) => Json(d).into_response(),
        AssignClientResult::NotFound => not_found("unknown client"),
        AssignClientResult::UnknownNetwork => bad_request("unknown network"),
    }
}

// ── UI layout (Phase 3 unified single-file schema) ───────────────────────

/// One node's saved x/y in flow-space. Matches the React Flow
/// position object the frontend ships.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct NodeXY {
    x: f64,
    y: f64,
}

/// One network's bounding-frame box in graph view: top-left + width/height.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
struct FrameBox {
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
    #[serde(default)]
    width: f64,
    #[serde(default)]
    height: f64,
}

/// Graph-view-specific persisted state.
#[derive(Debug, Default, Serialize, Deserialize)]
struct GraphLayout {
    /// React-Flow positions keyed by node id (`hub:<nid>`,
    /// `client:<cuid>`).
    #[serde(default)]
    positions: std::collections::HashMap<String, NodeXY>,
    /// Per-network frame boxes, keyed by net_uuid as string.
    #[serde(default)]
    frames: std::collections::HashMap<String, FrameBox>,
}

/// Table-view-specific persisted state (Phase 3 split panes).
#[derive(Debug, Default, Serialize, Deserialize)]
struct TableLayout {
    /// Width ratio of the LEFT pane (0.0–1.0). Persisted across
    /// reloads so the user's preferred split survives.
    #[serde(default)]
    left_ratio: Option<f64>,
    /// Whether the left pane is collapsed.
    #[serde(default)]
    left_collapsed: bool,
}

/// **Phase 3.** Single-file UI layout, replacing the per-network
/// `<nid>.json` files of Phase 1/2. The WebUI does one GET on load
/// and one debounced PUT after each interaction.
#[derive(Debug, Default, Serialize, Deserialize)]
struct UiLayout {
    #[serde(default)]
    table: TableLayout,
    #[serde(default)]
    graph: GraphLayout,
}

fn ui_layout_path(state: &AppState) -> std::path::PathBuf {
    state.layout_dir.join("ui-layout.json")
}

fn legacy_layout_path(state: &AppState, nid: Uuid) -> std::path::PathBuf {
    state.layout_dir.join(format!("{}.json", nid))
}

/// **Phase 2 → 3 migration.** If `ui-layout.json` doesn't exist yet
/// but legacy per-network files do, fold their `positions` maps into
/// the new file. Best-effort — failures are logged and swallowed,
/// since a stale legacy layout isn't worth blocking startup over.
async fn maybe_migrate_legacy_layouts(state: &AppState) {
    let new_path = ui_layout_path(state);
    if tokio::fs::try_exists(&new_path).await.unwrap_or(false) {
        return;
    }
    let mut migrated = UiLayout::default();
    let mut found_any = false;
    let Ok(mut dir) = tokio::fs::read_dir(&state.layout_dir).await else {
        return;
    };
    while let Ok(Some(ent)) = dir.next_entry().await {
        let path = ent.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if Uuid::parse_str(stem).is_err() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        // Old shape: { positions: { id -> {x,y} } }
        #[derive(Deserialize)]
        struct OldLayout {
            #[serde(default)]
            positions: std::collections::HashMap<String, NodeXY>,
        }
        let Ok(old) = serde_json::from_slice::<OldLayout>(&bytes) else {
            continue;
        };
        for (k, v) in old.positions {
            // Old keys were `server:<nid>` / `device:<cuid>`; preserve
            // them in the merged map (Phase 3 frontend treats both old
            // and new shape as the same node-id namespace).
            migrated.graph.positions.insert(k, v);
        }
        found_any = true;
    }
    if !found_any {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(&migrated) {
        if let Err(e) = write_atomic(&new_path, &bytes).await {
            tracing::warn!(error = %e, "ui-layout migration write failed");
        } else {
            tracing::info!("migrated legacy per-network layout files into ui-layout.json");
        }
    }
}

async fn get_ui_layout(State(state): State<AppState>) -> Response {
    maybe_migrate_legacy_layouts(&state).await;
    let path = ui_layout_path(&state);
    match tokio::fs::read(&path).await {
        Ok(bytes) => match serde_json::from_slice::<UiLayout>(&bytes) {
            Ok(layout) => Json(layout).into_response(),
            Err(_) => Json(UiLayout::default()).into_response(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Json(UiLayout::default()).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, ?path, "failed to read ui-layout");
            err(StatusCode::INTERNAL_SERVER_ERROR, "read failed")
        }
    }
}

async fn put_ui_layout(
    State(state): State<AppState>,
    Json(body): Json<UiLayout>,
) -> Response {
    let bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("malformed layout"),
    };
    let path = ui_layout_path(&state);
    if let Err(e) = write_atomic(&path, &bytes).await {
        tracing::warn!(error = %e, ?path, "failed to write ui-layout");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "write failed");
    }
    StatusCode::NO_CONTENT.into_response()
}

// ── Legacy per-network layout endpoints ──────────────────────────────────
//
// Kept temporarily so a half-deployed cluster doesn't 404 mid-upgrade.
// The frontend in 3.0e drops the per-network calls; the endpoints will
// be removed in a follow-up.

#[derive(Debug, Default, Serialize, Deserialize)]
struct LegacyGraphLayout {
    #[serde(default)]
    positions: std::collections::HashMap<String, NodeXY>,
}

async fn get_layout(State(state): State<AppState>, Path(nid): Path<Uuid>) -> Response {
    if !network_exists(&state, nid).await {
        return not_found("unknown network");
    }
    let path = legacy_layout_path(&state, nid);
    match tokio::fs::read(&path).await {
        Ok(bytes) => match serde_json::from_slice::<LegacyGraphLayout>(&bytes) {
            Ok(layout) => Json(layout).into_response(),
            Err(_) => Json(LegacyGraphLayout::default()).into_response(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Json(LegacyGraphLayout::default()).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, ?path, "failed to read legacy layout");
            err(StatusCode::INTERNAL_SERVER_ERROR, "read failed")
        }
    }
}

async fn put_layout(
    State(state): State<AppState>,
    Path(nid): Path<Uuid>,
    Json(body): Json<LegacyGraphLayout>,
) -> Response {
    if !network_exists(&state, nid).await {
        return not_found("unknown network");
    }
    let bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("malformed layout"),
    };
    let path = legacy_layout_path(&state, nid);
    if let Err(e) = write_atomic(&path, &bytes).await {
        tracing::warn!(error = %e, ?path, "failed to write legacy layout");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "write failed");
    }
    StatusCode::NO_CONTENT.into_response()
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
