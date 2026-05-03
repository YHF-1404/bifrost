//! End-to-end smoke tests for the WebUI HTTP API.
//!
//! Each test boots a real `Hub` backed by `MockPlatform` / `MockBridge`,
//! starts the axum server on an OS-assigned localhost port, and hits
//! the API with `reqwest`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::{ApprovedClient, NetRecord, ServerConfig};
use bifrost_core::Hub;
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Bridge, Platform};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use uuid::Uuid;

struct Harness {
    addr: SocketAddr,
}

async fn spawn(cfg: ServerConfig) -> Harness {
    let platform = MockPlatform::new();
    let bridge = MockBridge::new(&cfg.bridge.name);
    let (hub, handle) = Hub::new(
        cfg,
        None,
        platform.clone() as Arc<dyn Platform>,
        bridge.clone() as Arc<dyn Bridge>,
    );
    tokio::spawn(hub.run());

    // Bind a random port, then hand the listener address to bifrost-web.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let (st, _rx) = mpsc::channel::<()>(1);
    let h = handle.clone();
    tokio::spawn(async move {
        let _ = bifrost_web::serve(addr, h, st).await;
    });
    // Wait for it to come up.
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Harness { addr }
}

#[tokio::test]
async fn networks_endpoint_returns_array() {
    let net_a = Uuid::new_v4();
    let net_b = Uuid::new_v4();
    let mut cfg = ServerConfig::default();
    cfg.networks.push(NetRecord {
        name: "alpha".into(),
        uuid: net_a,
    });
    cfg.networks.push(NetRecord {
        name: "beta".into(),
        uuid: net_b,
    });
    cfg.approved_clients.push(ApprovedClient {
        client_uuid: Uuid::new_v4(),
        net_uuid: net_a,
        tap_ip: "10.0.0.5/24".into(),
        display_name: "router".into(),
        lan_subnets: vec!["192.168.10.0/24".into()],
    });

    let h = spawn(cfg).await;
    let url = format!("http://{}/api/networks", h.addr);
    let body: Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    let alpha = arr
        .iter()
        .find(|n| n["name"] == "alpha")
        .expect("alpha present");
    assert_eq!(alpha["device_count"], 1);
    assert_eq!(alpha["online_count"], 0);
}

#[tokio::test]
async fn devices_endpoint_returns_approved() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = ServerConfig::default();
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients.push(ApprovedClient {
        client_uuid: client,
        net_uuid: net,
        tap_ip: "10.0.0.5/24".into(),
        display_name: "router".into(),
        lan_subnets: vec!["192.168.10.0/24".into()],
    });

    let h = spawn(cfg).await;
    let url = format!("http://{}/api/networks/{}/devices", h.addr, net);
    let body: Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["display_name"], "router");
    assert_eq!(arr[0]["tap_ip"], "10.0.0.5/24");
    assert_eq!(arr[0]["admitted"], true);
    assert_eq!(arr[0]["online"], false);
    assert_eq!(arr[0]["lan_subnets"][0], "192.168.10.0/24");
}

#[tokio::test]
async fn devices_endpoint_404s_unknown_net() {
    let h = spawn(ServerConfig::default()).await;
    let url = format!("http://{}/api/networks/{}/devices", h.addr, Uuid::new_v4());
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}
