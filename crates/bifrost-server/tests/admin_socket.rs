//! End-to-end tests for the admin Unix-socket protocol.
//!
//! Each test starts a real Hub backed by `MockPlatform`/`MockBridge`,
//! spawns [`bifrost_server::admin::serve`] on a tempdir socket, and then
//! drives `round_trip` exactly the way the `bifrost-server admin <cmd>`
//! subcommand does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::{ApprovedClient, NetRecord, ServerConfig};
use bifrost_core::Hub;
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Bridge, Platform};
use bifrost_proto::admin::{ServerAdminReq, ServerAdminResp};
use bifrost_server::admin;
use tempfile::TempDir;
use tokio::sync::mpsc;
use uuid::Uuid;

struct Harness {
    socket: PathBuf,
    #[allow(dead_code)]
    shutdown_rx: mpsc::Receiver<()>,
    #[allow(dead_code)]
    hub_join: tokio::task::JoinHandle<()>,
    #[allow(dead_code)]
    _tmp: TempDir,
}

async fn spawn(approved: Vec<(Uuid, Uuid, &str)>, networks: Vec<Uuid>) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("admin.sock");

    let mut cfg = ServerConfig::default();
    cfg.bridge.disconnect_timeout = 60;
    for net in networks {
        cfg.networks.push(NetRecord {
            name: format!("net-{}", &net.simple().to_string()[..8]),
            uuid: net,
        });
    }
    for (client, net, ip) in approved {
        cfg.approved_clients.push(ApprovedClient {
            client_uuid: client,
            net_uuid: net,
            tap_ip: ip.to_string(),
        });
    }
    let platform = MockPlatform::new();
    let bridge = MockBridge::new(&cfg.bridge.name);
    let (hub, handle) = Hub::new(
        cfg,
        None,
        platform.clone() as Arc<dyn Platform>,
        bridge.clone() as Arc<dyn Bridge>,
    );
    let hub_join = tokio::spawn(hub.run());

    let (shutdown_tx, shutdown_rx) = mpsc::channel(2);
    let socket_clone = socket.clone();
    let hub_admin = handle.clone();
    tokio::spawn(async move {
        let _ = admin::serve(socket_clone, hub_admin, shutdown_tx).await;
    });
    // Wait for the listener to come up.
    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Harness {
        socket,
        shutdown_rx,
        hub_join,
        _tmp: tmp,
    }
}

async fn rpc(socket: &std::path::Path, req: ServerAdminReq) -> ServerAdminResp {
    admin::round_trip(socket, req).await.expect("admin RPC")
}

#[tokio::test]
async fn mknet_round_trip_returns_uuid() {
    let h = spawn(vec![], vec![]).await;
    let resp = rpc(
        &h.socket,
        ServerAdminReq::MakeNet {
            name: "hml".into(),
        },
    )
    .await;
    match resp {
        ServerAdminResp::NetCreated { uuid } => {
            assert_ne!(uuid, Uuid::nil());
        }
        other => panic!("expected NetCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn list_returns_snapshot() {
    let net = Uuid::new_v4();
    let h = spawn(vec![], vec![net]).await;
    let resp = rpc(&h.socket, ServerAdminReq::List).await;
    match resp {
        ServerAdminResp::Snapshot(snap) => {
            assert_eq!(snap.networks.len(), 1);
            assert_eq!(snap.networks[0].uuid, net);
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn route_add_persists_and_lists() {
    let h = spawn(vec![], vec![]).await;

    let resp = rpc(
        &h.socket,
        ServerAdminReq::RouteAdd {
            dst: "192.168.10.0/24".into(),
            via: "10.0.0.1".into(),
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::Ok));

    let resp = rpc(&h.socket, ServerAdminReq::List).await;
    match resp {
        ServerAdminResp::Snapshot(snap) => {
            assert_eq!(snap.routes.len(), 1);
            assert_eq!(snap.routes[0].dst, "192.168.10.0/24");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    // route del
    let resp = rpc(
        &h.socket,
        ServerAdminReq::RouteDel {
            dst: "192.168.10.0/24".into(),
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::Ok));

    // del again — NotFound
    let resp = rpc(
        &h.socket,
        ServerAdminReq::RouteDel {
            dst: "192.168.10.0/24".into(),
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::NotFound));
}

#[tokio::test]
async fn route_add_validates_input() {
    let h = spawn(vec![], vec![]).await;
    let resp = rpc(
        &h.socket,
        ServerAdminReq::RouteAdd {
            dst: "garbage".into(),
            via: "10.0.0.1".into(),
        },
    )
    .await;
    match resp {
        ServerAdminResp::Error(msg) => assert!(msg.contains("CIDR"), "got: {msg}"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn approve_unknown_returns_not_found() {
    let h = spawn(vec![], vec![]).await;
    let resp = rpc(&h.socket, ServerAdminReq::Approve { sid: 999 }).await;
    assert!(matches!(resp, ServerAdminResp::NotFound));
}

#[tokio::test]
async fn setip_unknown_prefix_returns_not_found() {
    let h = spawn(vec![], vec![]).await;
    let resp = rpc(
        &h.socket,
        ServerAdminReq::SetIp {
            prefix: "ff".into(),
            ip: "10.0.0.1".into(),
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::NotFound));
}

#[tokio::test]
async fn setip_invalid_ip() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let h = spawn(
        vec![(client, net, "")],
        vec![net],
    )
    .await;
    let prefix = client.simple().to_string()[..8].into();
    let resp = rpc(
        &h.socket,
        ServerAdminReq::SetIp {
            prefix,
            ip: "not-an-ip".into(),
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::SetIpInvalid));
}

#[tokio::test]
async fn shutdown_request_signals_main() {
    let mut h = spawn(vec![], vec![]).await;
    let resp = rpc(&h.socket, ServerAdminReq::Shutdown).await;
    assert!(matches!(resp, ServerAdminResp::Ok));

    // The serve task should have signalled shutdown before responding.
    let signal = tokio::time::timeout(Duration::from_millis(500), h.shutdown_rx.recv()).await;
    assert!(signal.is_ok(), "shutdown_rx must receive a signal");
}

#[tokio::test]
async fn broadcast_text_returns_count() {
    let h = spawn(vec![], vec![]).await;
    // No conns connected → broadcast reaches 0.
    let resp = rpc(
        &h.socket,
        ServerAdminReq::Send {
            msg: "hello".into(),
        },
    )
    .await;
    match resp {
        ServerAdminResp::Count(n) => assert_eq!(n, 0),
        other => panic!("expected Count, got {other:?}"),
    }
}
