//! End-to-end test of the `/ws` channel: spin up a Hub, attach a
//! tungstenite client, wait for a `metrics.tick` event to arrive
//! through the broadcast → JSON serialization → WebSocket pipeline.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::{ApprovedClient, NetRecord, ServerConfig};
use bifrost_core::Hub;
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Bridge, Platform};
use bifrost_proto::PROTOCOL_VERSION;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

async fn spawn_server() -> (SocketAddr, bifrost_core::HubHandle) {
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
        display_name: String::new(),
        lan_subnets: Vec::new(),
    });

    let platform = MockPlatform::new();
    let bridge = MockBridge::new(&cfg.bridge.name);
    let (hub, handle) = Hub::new(
        cfg,
        None,
        platform.clone() as Arc<dyn Platform>,
        bridge.clone() as Arc<dyn Bridge>,
    );
    tokio::spawn(hub.run());

    // Bring a fake conn online so a real session exists; the metrics
    // sampler emits non-empty arrays.
    let (frame_tx, _frame_rx) = mpsc::channel(16);
    let (bind_tx, _bind_rx) = mpsc::channel(8);
    let conn = handle
        .register_conn("test".into(), bifrost_core::ConnLink { frame_tx, bind_tx })
        .await
        .expect("register conn");
    handle.hello(conn, client, PROTOCOL_VERSION).await;
    handle.join(conn, net).await;

    // Bind a free port and hand it to bifrost-web::serve.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let (st, _stx) = mpsc::channel::<()>(1);
    let h = handle.clone();
    tokio::spawn(async move {
        let _ = bifrost_web::serve(addr, h, st).await;
    });

    // Wait for the listener.
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (addr, handle)
}

#[tokio::test]
async fn ws_delivers_metrics_tick_as_json() {
    let (addr, _hub) = spawn_server().await;

    let url = format!("ws://{}/ws", addr);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("ws connect");

    // Wait up to 3 s for the first text frame. Skip Pings.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let json: Value = loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("ws first event timed out")
            .expect("ws stream ended");
        match msg.expect("ws msg err") {
            WsMessage::Text(t) => break serde_json::from_str(&t).expect("invalid json"),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            other => panic!("unexpected ws frame: {other:?}"),
        }
    };
    assert_eq!(json["type"], "metrics.tick");
    let samples = json["samples"].as_array().expect("samples is array");
    assert_eq!(samples.len(), 1);
    assert!(samples[0]["client_uuid"].is_string());
    assert_eq!(samples[0]["bps_in"], 0);
    assert_eq!(samples[0]["bps_out"], 0);

    let _ = ws.send(WsMessage::Close(None)).await;
}
