//! HTTP write-side tests for `bifrost-web`: PATCH device (incl. admit
//! toggle) and POST routes/push.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::{ApprovedClient, NetRecord, ServerConfig};
use bifrost_core::{ConnLink, Hub, HubHandle, SessionCmd};
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Bridge, Platform};
use bifrost_proto::PROTOCOL_VERSION;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use uuid::Uuid;

struct Harness {
    addr: SocketAddr,
    hub: HubHandle,
    net: Uuid,
}

fn approved(client: Uuid, net: Uuid, ip: &str, lan: &[&str]) -> ApprovedClient {
    ApprovedClient {
        client_uuid: client,
        net_uuid: net,
        tap_ip: ip.into(),
        display_name: String::new(),
        lan_subnets: lan.iter().map(|s| s.to_string()).collect(),
        admitted: true,
    }
}

async fn spawn_with(approveds: Vec<ApprovedClient>) -> Harness {
    let net = Uuid::new_v4();
    let mut cfg = ServerConfig::default();
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    for mut a in approveds {
        a.net_uuid = net;
        cfg.approved_clients.push(a);
    }

    let platform = MockPlatform::new();
    let bridge = MockBridge::new(&cfg.bridge.name);
    let (hub, handle) = Hub::new(
        cfg,
        None,
        platform.clone() as Arc<dyn Platform>,
        bridge.clone() as Arc<dyn Bridge>,
    );
    tokio::spawn(hub.run());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let (st, _rx) = mpsc::channel::<()>(1);
    let h = handle.clone();
    tokio::spawn(async move {
        let _ = bifrost_web::serve(addr, h, st).await;
    });
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Harness {
        addr,
        hub: handle,
        net,
    }
}

fn url(h: &Harness, path: &str) -> String {
    format!("http://{}{}", h.addr, path)
}

#[tokio::test]
async fn patch_device_updates_fields_and_returns_record() {
    let cid = Uuid::new_v4();
    let h = spawn_with(vec![approved(cid, Uuid::nil(), "10.0.0.5/24", &[])]).await;

    let resp = reqwest::Client::new()
        .patch(url(&h, &format!("/api/networks/{}/devices/{}", h.net, cid)))
        .json(&json!({
            "name": "router",
            "lan_subnets": ["192.168.10.0/24"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["display_name"], "router");
    assert_eq!(body["lan_subnets"][0], "192.168.10.0/24");
    assert_eq!(body["tap_ip"], "10.0.0.5/24"); // untouched
    assert_eq!(body["admitted"], true);
}

#[tokio::test]
async fn patch_device_admit_false_keeps_row_in_pending_state() {
    let cid = Uuid::new_v4();
    let h = spawn_with(vec![approved(cid, Uuid::nil(), "10.0.0.5/24", &[])]).await;

    let resp = reqwest::Client::new()
        .patch(url(&h, &format!("/api/networks/{}/devices/{}", h.net, cid)))
        .json(&json!({ "admitted": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["admitted"], false);

    // Listing now shows the row with admitted=false (NOT removed).
    let resp = reqwest::Client::new()
        .get(url(&h, &format!("/api/networks/{}/devices", h.net)))
        .send()
        .await
        .unwrap();
    let arr: Value = resp.json().await.unwrap();
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["client_uuid"], cid.to_string());
    assert_eq!(arr[0]["admitted"], false);
}

#[tokio::test]
async fn patch_device_admit_toggle_round_trip() {
    // Admin starts a row as admitted=false, then flips it on.
    let cid = Uuid::new_v4();
    let mut row = approved(cid, Uuid::nil(), "10.0.0.5/24", &[]);
    row.admitted = false;
    let h = spawn_with(vec![row]).await;

    let resp = reqwest::Client::new()
        .patch(url(&h, &format!("/api/networks/{}/devices/{}", h.net, cid)))
        .json(&json!({ "admitted": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["admitted"], true);
}

#[tokio::test]
async fn patch_device_invalid_ip_is_400() {
    let cid = Uuid::new_v4();
    let h = spawn_with(vec![approved(cid, Uuid::nil(), "", &[])]).await;

    let resp = reqwest::Client::new()
        .patch(url(&h, &format!("/api/networks/{}/devices/{}", h.net, cid)))
        .json(&json!({ "tap_ip": "not-an-ip" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_device_conflict_is_409() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let h = spawn_with(vec![
        approved(a, Uuid::nil(), "10.0.0.2/24", &[]),
        approved(b, Uuid::nil(), "10.0.0.3/24", &[]),
    ])
    .await;

    let resp = reqwest::Client::new()
        .patch(url(&h, &format!("/api/networks/{}/devices/{}", h.net, b)))
        .json(&json!({ "tap_ip": "10.0.0.2/24" })) // collide with a
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn patch_device_unknown_is_404() {
    let h = spawn_with(vec![]).await;
    let resp = reqwest::Client::new()
        .patch(url(
            &h,
            &format!("/api/networks/{}/devices/{}", h.net, Uuid::new_v4()),
        ))
        .json(&json!({ "name": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fresh_join_creates_pending_row_and_admit_promotes_it() {
    let h = spawn_with(vec![]).await;

    // Stand up a fake conn that joins. Hub creates a row with
    // admitted=false on first contact.
    let cid = Uuid::new_v4();
    let (frame_tx, _frame_rx) = mpsc::channel(16);
    let (bind_tx, _bind_rx) = mpsc::channel::<Option<mpsc::Sender<SessionCmd>>>(8);
    let conn = h
        .hub
        .register_conn("x".into(), ConnLink { frame_tx, bind_tx })
        .await
        .unwrap();
    h.hub.hello(conn, cid, PROTOCOL_VERSION).await;
    h.hub.join(conn, h.net).await;
    // brief settle
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Listing shows the device in pending state.
    let arr: Value = reqwest::get(url(&h, &format!("/api/networks/{}/devices", h.net)))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["admitted"], false);
    assert_eq!(arr[0]["online"], true);

    // Flip admit on via PATCH — the row turns admitted=true and the
    // pending conn promotes to a real session.
    let resp = reqwest::Client::new()
        .patch(url(&h, &format!("/api/networks/{}/devices/{}", h.net, cid)))
        .json(&json!({ "admitted": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["admitted"], true);
}

#[tokio::test]
async fn push_routes_returns_count_and_table() {
    let cid = Uuid::new_v4();
    let h = spawn_with(vec![approved(
        cid,
        Uuid::nil(),
        "10.0.0.5/24",
        &["192.168.10.0/24"],
    )])
    .await;
    let resp = reqwest::Client::new()
        .post(url(&h, &format!("/api/networks/{}/routes/push", h.net)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["count"], 0);
    let routes = body["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["dst"], "192.168.10.0/24");
    assert_eq!(routes[0]["via"], "10.0.0.5");
}

#[tokio::test]
async fn push_routes_unknown_net_is_404() {
    let h = spawn_with(vec![]).await;
    let resp = reqwest::Client::new()
        .post(url(
            &h,
            &format!("/api/networks/{}/routes/push", Uuid::new_v4()),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}
