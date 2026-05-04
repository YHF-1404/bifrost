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
use bifrost_net::mock::MockPlatform;
use bifrost_net::Platform;
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
        cfg.networks.push(NetRecord::new(
            format!("net-{}", &net.simple().to_string()[..8]),
            net,
        ));
    }
    for (client, net, ip) in approved {
        cfg.approved_clients.push(ApprovedClient {
            client_uuid: client,
            net_uuid: net,
            tap_ip: ip.to_string(),
            display_name: String::new(),
            lan_subnets: Vec::new(),
            admitted: true,
        });
    }
    let platform = MockPlatform::new();
    let (hub, handle) = Hub::new(cfg,
        None,
        platform.clone() as Arc<dyn Platform>);
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
async fn device_set_lan_subnets_round_trips() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let h = spawn(vec![(client, net, "10.0.0.5/24")], vec![net]).await;

    // Set lan_subnets via DeviceSet.
    let resp = rpc(
        &h.socket,
        ServerAdminReq::DeviceSet {
            client_uuid: client,
            name: Some("router".into()),
            admitted: None,
            tap_ip: None,
            lan_subnets: Some(vec!["192.168.10.0/24".into()]),
        },
    )
    .await;
    match resp {
        ServerAdminResp::Device(d) => {
            assert_eq!(d.display_name, "router");
            assert_eq!(d.lan_subnets, vec!["192.168.10.0/24".to_string()]);
        }
        other => panic!("expected Device, got {other:?}"),
    }

    // Push and verify routes.
    let resp = rpc(&h.socket, ServerAdminReq::DevicePush { net_uuid: net }).await;
    match resp {
        ServerAdminResp::Pushed { routes, .. } => {
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].dst, "192.168.10.0/24");
            assert_eq!(routes[0].via, "10.0.0.5");
        }
        other => panic!("expected Pushed, got {other:?}"),
    }
}

#[tokio::test]
async fn device_set_validates_invalid_subnet() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let h = spawn(vec![(client, net, "10.0.0.5/24")], vec![net]).await;

    let resp = rpc(
        &h.socket,
        ServerAdminReq::DeviceSet {
            client_uuid: client,
            name: None,
            admitted: None,
            tap_ip: None,
            lan_subnets: Some(vec!["garbage".into()]),
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::InvalidIp), "got {resp:?}");
}

#[tokio::test]
async fn device_set_unknown_client_returns_not_found() {
    let h = spawn(vec![], vec![]).await;
    let resp = rpc(
        &h.socket,
        ServerAdminReq::DeviceSet {
            client_uuid: Uuid::new_v4(),
            name: None,
            admitted: None,
            tap_ip: Some("10.0.0.1/24".into()),
            lan_subnets: None,
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::NotFound));
}

#[tokio::test]
async fn device_set_invalid_ip() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let h = spawn(vec![(client, net, "")], vec![net]).await;
    let resp = rpc(
        &h.socket,
        ServerAdminReq::DeviceSet {
            client_uuid: client,
            name: None,
            admitted: None,
            tap_ip: Some("not-an-ip".into()),
            lan_subnets: None,
        },
    )
    .await;
    assert!(matches!(resp, ServerAdminResp::InvalidIp));
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
