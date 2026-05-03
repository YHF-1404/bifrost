//! End-to-end tests for [`Hub`] driven against the mock platform.
//!
//! Each test plays the role of a connection task: it owns the
//! receiving ends of `frame_tx`/`bind_tx`, sends the equivalent of
//! `Hello`/`Join`/`Disconnect` via [`HubHandle`], and then asserts on
//! what the hub pushed back through those channels and what state
//! ended up on the mock platform / mock bridge.

use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::{ApprovedClient, NetRecord, ServerConfig};
use bifrost_core::{ConnId, ConnLink, DeviceUpdate, Hub, HubHandle, SessionCmd, SessionId};
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Bridge, Platform, Tap};
use bifrost_proto::{Frame, PROTOCOL_VERSION};
use tokio::sync::mpsc;
use uuid::Uuid;

const SHORT: Duration = Duration::from_millis(200);

// ─── Harness ──────────────────────────────────────────────────────────────

struct Harness {
    hub: HubHandle,
    platform: Arc<MockPlatform>,
    bridge: Arc<MockBridge>,
    #[allow(dead_code)]
    join: tokio::task::JoinHandle<()>,
}

struct FakeConn {
    id: ConnId,
    frame_rx: mpsc::Receiver<Frame>,
    bind_rx: mpsc::Receiver<Option<mpsc::Sender<SessionCmd>>>,
}

fn build_cfg(disconnect_secs: u64) -> ServerConfig {
    let mut cfg = ServerConfig::default();
    cfg.bridge.disconnect_timeout = disconnect_secs;
    cfg
}

fn approved(client_uuid: Uuid, net_uuid: Uuid, tap_ip: &str) -> ApprovedClient {
    ApprovedClient {
        client_uuid,
        net_uuid,
        tap_ip: tap_ip.to_string(),
        display_name: String::new(),
        lan_subnets: Vec::new(),
    }
}

fn approved_with_lan(
    client_uuid: Uuid,
    net_uuid: Uuid,
    tap_ip: &str,
    lan: &[&str],
) -> ApprovedClient {
    ApprovedClient {
        client_uuid,
        net_uuid,
        tap_ip: tap_ip.to_string(),
        display_name: String::new(),
        lan_subnets: lan.iter().map(|s| s.to_string()).collect(),
    }
}

async fn spawn(cfg: ServerConfig) -> Harness {
    spawn_with_path(cfg, None).await
}

async fn spawn_with_path(cfg: ServerConfig, cfg_path: Option<std::path::PathBuf>) -> Harness {
    let platform = MockPlatform::new();
    let bridge = MockBridge::new(&cfg.bridge.name);
    let (hub, handle) = Hub::new(
        cfg,
        cfg_path,
        platform.clone() as Arc<dyn Platform>,
        bridge.clone() as Arc<dyn Bridge>,
    );
    let join = tokio::spawn(hub.run());
    Harness {
        hub: handle,
        platform,
        bridge,
        join,
    }
}

async fn fake_conn(h: &Harness, addr: &str) -> FakeConn {
    let (frame_tx, frame_rx) = mpsc::channel(16);
    let (bind_tx, bind_rx) = mpsc::channel(8);
    let id = h
        .hub
        .register_conn(addr.to_owned(), ConnLink { frame_tx, bind_tx })
        .await
        .expect("register_conn");
    FakeConn {
        id,
        frame_rx,
        bind_rx,
    }
}

async fn recv(rx: &mut mpsc::Receiver<Frame>) -> Frame {
    tokio::time::timeout(SHORT, rx.recv())
        .await
        .expect("frame timeout")
        .expect("frame_rx closed")
}

async fn try_recv_silent(rx: &mut mpsc::Receiver<Frame>, dur: Duration) -> Option<Frame> {
    tokio::time::timeout(dur, rx.recv()).await.ok().flatten()
}

async fn recv_bind(
    rx: &mut mpsc::Receiver<Option<mpsc::Sender<SessionCmd>>>,
) -> Option<mpsc::Sender<SessionCmd>> {
    tokio::time::timeout(SHORT, rx.recv())
        .await
        .expect("bind timeout")
        .expect("bind_rx closed")
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn whitelisted_join_creates_tap_and_binds_conn() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();

    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients
        .push(approved(client, net, "10.0.0.5/24"));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "1.2.3.4:1").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;

    // Expect JoinOk first.
    match recv(&mut c.frame_rx).await {
        Frame::JoinOk { tap_suffix, ip } => {
            assert!(!tap_suffix.is_empty());
            assert_eq!(ip.as_deref(), Some("10.0.0.5/24"));
        }
        other => panic!("expected JoinOk, got {other:?}"),
    }

    // Mock platform must have created exactly one TAP, added to bridge.
    assert_eq!(h.platform.taps_count().await, 1);
    let tap = h.platform.last_tap().await.unwrap();
    assert_eq!(h.bridge.snapshot().await.ports, vec![tap.name().to_owned()]);

    // Conn must have received Some(session_cmd_tx) on bind_rx.
    let bound = recv_bind(&mut c.bind_rx).await;
    assert!(bound.is_some(), "conn must be bound");

    h.hub.shutdown().await;
}

#[tokio::test]
async fn unknown_network_is_denied() {
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: Uuid::new_v4(),
    });
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, Uuid::new_v4(), PROTOCOL_VERSION).await;
    h.hub.join(c.id, Uuid::new_v4()).await; // wrong net

    match recv(&mut c.frame_rx).await {
        Frame::JoinDeny { reason } => assert_eq!(reason, "unknown_network"),
        other => panic!("expected JoinDeny, got {other:?}"),
    }
    assert_eq!(h.platform.taps_count().await, 0);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn join_without_hello_is_denied() {
    let net = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    // Skip Hello.
    h.hub.join(c.id, net).await;

    match recv(&mut c.frame_rx).await {
        Frame::JoinDeny { reason } => assert_eq!(reason, "no_hello"),
        other => panic!("expected JoinDeny, got {other:?}"),
    }
    h.hub.shutdown().await;
}

#[tokio::test]
async fn pending_then_approve_yields_join_ok() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;

    // No frame should arrive — hub is waiting for admin to approve.
    assert!(try_recv_silent(&mut c.frame_rx, Duration::from_millis(50))
        .await
        .is_none());

    // Find the pending sid via list().
    let snap = h.hub.list().await.unwrap();
    assert_eq!(snap.pending.len(), 1);
    let pending_sid = snap.pending[0].sid;

    let approved = h.hub.approve(pending_sid).await;
    assert!(approved);

    match recv(&mut c.frame_rx).await {
        Frame::JoinOk { .. } => {}
        other => panic!("expected JoinOk, got {other:?}"),
    }
    assert_eq!(h.platform.taps_count().await, 1);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn pending_then_deny_yields_join_deny() {
    let net = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, Uuid::new_v4(), PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;

    let snap = h.hub.list().await.unwrap();
    let sid = snap.pending[0].sid;
    assert!(h.hub.deny(sid).await);

    match recv(&mut c.frame_rx).await {
        Frame::JoinDeny { reason } => assert_eq!(reason, "denied_by_admin"),
        other => panic!("expected JoinDeny, got {other:?}"),
    }
    assert_eq!(h.platform.taps_count().await, 0);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn approve_unknown_sid_returns_false() {
    let h = spawn(build_cfg(60)).await;
    assert!(!h.hub.approve(SessionId(999)).await);
    assert!(!h.hub.deny(SessionId(999)).await);
    h.hub.shutdown().await;
}

#[tokio::test]
async fn make_net_persists_into_list() {
    let h = spawn(build_cfg(60)).await;
    let uuid = h.hub.make_net("hml-net".into()).await.unwrap();
    let snap = h.hub.list().await.unwrap();
    assert!(snap
        .networks
        .iter()
        .any(|n| n.uuid == uuid && n.name == "hml-net"));
    h.hub.shutdown().await;
}

#[tokio::test]
async fn disconnect_unbinds_session_but_keeps_tap() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await; // JoinOk
    let _ = recv_bind(&mut c.bind_rx).await; // bound

    h.hub.disconnect(c.id).await;

    // Session remains, but bound_conn cleared.
    let snap = h.hub.list().await.unwrap();
    assert_eq!(snap.sessions.len(), 1);
    assert_eq!(snap.sessions[0].bound_conn, None);
    // TAP still present in bridge.
    assert_eq!(h.bridge.snapshot().await.ports.len(), 1);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn reconnect_reuses_session_no_new_tap() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients
        .push(approved(client, net, "10.0.0.7/24"));
    let h = spawn(cfg).await;

    // First join.
    let mut c1 = fake_conn(&h, "x1").await;
    h.hub.hello(c1.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c1.id, net).await;
    let _ = recv(&mut c1.frame_rx).await;
    let _ = recv_bind(&mut c1.bind_rx).await;
    assert_eq!(h.platform.taps_count().await, 1);

    // First conn drops.
    h.hub.disconnect(c1.id).await;

    // Second conn reconnects.
    let mut c2 = fake_conn(&h, "x2").await;
    h.hub.hello(c2.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c2.id, net).await;

    // Still only 1 TAP.
    assert_eq!(h.platform.taps_count().await, 1);

    // Second conn gets JoinOk and a binding.
    match recv(&mut c2.frame_rx).await {
        Frame::JoinOk { ip, .. } => assert_eq!(ip.as_deref(), Some("10.0.0.7/24")),
        other => panic!("expected JoinOk, got {other:?}"),
    }
    let bound = recv_bind(&mut c2.bind_rx).await;
    assert!(bound.is_some());

    h.hub.shutdown().await;
}

#[tokio::test]
async fn reconnect_while_first_still_bound_unbinds_first() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c1 = fake_conn(&h, "x1").await;
    h.hub.hello(c1.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c1.id, net).await;
    let _ = recv(&mut c1.frame_rx).await;
    let _ = recv_bind(&mut c1.bind_rx).await; // initial bind

    // Second conn from same client without disconnecting the first.
    let mut c2 = fake_conn(&h, "x2").await;
    h.hub.hello(c2.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c2.id, net).await;

    // First conn must observe an unbind (`None` on bind_rx).
    let unbind = recv_bind(&mut c1.bind_rx).await;
    assert!(unbind.is_none(), "old conn must be unbound");

    let _ = recv(&mut c2.frame_rx).await; // JoinOk on new conn
    h.hub.shutdown().await;
}

#[tokio::test]
async fn disconnect_then_timeout_removes_session_and_tap() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(0); // 0 = expire instantly
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await;
    let _ = recv_bind(&mut c.bind_rx).await;

    h.hub.disconnect(c.id).await;
    // Allow the session task to time out and the hub to process death.
    tokio::time::sleep(Duration::from_millis(120)).await;

    let snap = h.hub.list().await.unwrap();
    assert!(snap.sessions.is_empty(), "session should be cleaned up");
    let bridge = h.bridge.snapshot().await;
    assert!(bridge.ports.is_empty(), "tap should be removed from bridge");

    h.hub.shutdown().await;
}

#[tokio::test]
async fn routes_pushed_after_join_and_filter_self_via() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    let other_client = Uuid::new_v4();
    cfg.approved_clients.push(approved_with_lan(
        client,
        net,
        "10.0.0.5/24",
        // This route's via == joining client's own IP → must be filtered.
        &["192.168.20.0/24"],
    ));
    cfg.approved_clients.push(approved_with_lan(
        other_client,
        net,
        "10.0.0.7/24",
        &["192.168.10.0/24"],
    ));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;

    let _ = recv(&mut c.frame_rx).await; // JoinOk
    match recv(&mut c.frame_rx).await {
        Frame::SetRoutes(rs) => {
            assert_eq!(rs.len(), 1, "self-via route must be filtered: {rs:?}");
            assert_eq!(rs[0].dst, "192.168.10.0/24");
            assert_eq!(rs[0].via, "10.0.0.7");
        }
        other => panic!("expected SetRoutes, got {other:?}"),
    }

    h.hub.shutdown().await;
}

#[tokio::test]
async fn shutdown_destroys_bridge_and_kills_sessions() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await;
    let _ = recv_bind(&mut c.bind_rx).await;

    let tap = h.platform.last_tap().await.unwrap();

    h.hub.shutdown().await;
    // Allow the hub task to finish its shutdown sequence.
    let _ = tokio::time::timeout(Duration::from_secs(2), h.join).await;

    assert!(h.bridge.snapshot().await.destroyed, "bridge must be destroyed");
    assert!(tap.snapshot().await.destroyed, "tap must be destroyed");
}

// ── Newly-added REPL commands: device_set / device_push / broadcast ──────

#[tokio::test]
async fn device_set_ip_pushes_to_live_session() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients.push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await; // JoinOk
    let _ = recv_bind(&mut c.bind_rx).await;

    let result = h
        .hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                tap_ip: Some("10.0.0.42/24".to_string()),
                ..Default::default()
            },
        )
        .await;
    match result {
        bifrost_core::DeviceSetResult::Ok(d) => {
            assert_eq!(d.tap_ip.as_deref(), Some("10.0.0.42/24"));
            assert!(d.online);
        }
        other => panic!("expected Ok, got {other:?}"),
    }

    match recv(&mut c.frame_rx).await {
        Frame::SetIp { ip } => assert_eq!(ip.as_deref(), Some("10.0.0.42/24")),
        other => panic!("expected SetIp, got {other:?}"),
    }
    h.hub.shutdown().await;
}

#[tokio::test]
async fn device_set_offline_only_persists_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("server.toml");
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients.push(approved(client, net, ""));
    cfg.save(&cfg_path).await.unwrap();
    let h = spawn_with_path(cfg, Some(cfg_path.clone())).await;

    // No live conn — pure offline update of name + lan.
    let result = h
        .hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                name: Some("router".into()),
                lan_subnets: Some(vec!["192.168.10.0/24".into(), "192.168.20.0/24".into()]),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, bifrost_core::DeviceSetResult::Ok(_)));

    let on_disk = ServerConfig::load(&cfg_path).await.unwrap();
    assert_eq!(on_disk.approved_clients.len(), 1);
    assert_eq!(on_disk.approved_clients[0].display_name, "router");
    assert_eq!(on_disk.approved_clients[0].lan_subnets.len(), 2);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn device_set_rejects_invalid_or_unknown() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients.push(approved(client, net, ""));
    let h = spawn(cfg).await;

    // Unknown (client, net) and not admitting → NotFound.
    let result = h
        .hub
        .device_set(
            Uuid::new_v4(),
            net,
            DeviceUpdate {
                tap_ip: Some("10.0.0.1/24".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, bifrost_core::DeviceSetResult::NotFound));

    // Invalid IP.
    let result = h
        .hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                tap_ip: Some("not-an-ip".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, bifrost_core::DeviceSetResult::InvalidIp));

    // Invalid CIDR in lan_subnets.
    let result = h
        .hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                lan_subnets: Some(vec!["not-a-cidr".into()]),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, bifrost_core::DeviceSetResult::InvalidIp));

    h.hub.shutdown().await;
}

#[tokio::test]
async fn device_set_detects_tap_ip_conflict() {
    let net = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients.push(approved(a, net, "10.0.0.2/24"));
    cfg.approved_clients.push(approved(b, net, "10.0.0.3/24"));
    let h = spawn(cfg).await;

    // Try to set b's IP to a's IP.
    let result = h
        .hub
        .device_set(
            b,
            net,
            DeviceUpdate {
                tap_ip: Some("10.0.0.2/24".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(result, bifrost_core::DeviceSetResult::Conflict { .. }),
        "got {result:?}"
    );
    h.hub.shutdown().await;
}

#[tokio::test]
async fn device_set_persists_lan_subnets_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("server.toml");
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    cfg.approved_clients
        .push(approved(client, net, "10.0.0.5/24"));
    cfg.save(&cfg_path).await.unwrap();
    let h = spawn_with_path(cfg, Some(cfg_path.clone())).await;

    h.hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                lan_subnets: Some(vec!["192.168.10.0/24".into(), "192.168.20.0/24".into()]),
                ..Default::default()
            },
        )
        .await;
    let on_disk = ServerConfig::load(&cfg_path).await.unwrap();
    assert_eq!(on_disk.approved_clients[0].lan_subnets.len(), 2);

    // Clear them.
    h.hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                lan_subnets: Some(Vec::new()),
                ..Default::default()
            },
        )
        .await;
    let on_disk = ServerConfig::load(&cfg_path).await.unwrap();
    assert!(on_disk.approved_clients[0].lan_subnets.is_empty());

    h.hub.shutdown().await;
}

#[tokio::test]
async fn device_push_pushes_to_each_bound_session() {
    let net = Uuid::new_v4();
    let client_a = Uuid::new_v4();
    let client_b = Uuid::new_v4();
    let other = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord {
        name: "n".into(),
        uuid: net,
    });
    // Two live clients and one offline client whose lan_subnet drives
    // the route table.
    cfg.approved_clients
        .push(approved(client_a, net, "10.0.0.2/24"));
    cfg.approved_clients
        .push(approved(client_b, net, "10.0.0.3/24"));
    cfg.approved_clients.push(approved_with_lan(
        other,
        net,
        "10.0.0.7/24",
        &["192.168.10.0/24"],
    ));
    let h = spawn(cfg).await;

    let mut a = fake_conn(&h, "x:1").await;
    h.hub.hello(a.id, client_a, PROTOCOL_VERSION).await;
    h.hub.join(a.id, net).await;
    let _ = recv(&mut a.frame_rx).await; // JoinOk
    let _ = recv(&mut a.frame_rx).await; // SetRoutes (initial push)
    let _ = recv_bind(&mut a.bind_rx).await;

    let mut b = fake_conn(&h, "x:2").await;
    h.hub.hello(b.id, client_b, PROTOCOL_VERSION).await;
    h.hub.join(b.id, net).await;
    let _ = recv(&mut b.frame_rx).await;
    let _ = recv(&mut b.frame_rx).await;
    let _ = recv_bind(&mut b.bind_rx).await;

    let result = h.hub.device_push(net).await;
    assert_eq!(result.count, 2);
    assert_eq!(result.routes.len(), 1);
    match recv(&mut a.frame_rx).await {
        Frame::SetRoutes(rs) => assert_eq!(rs.len(), 1),
        other => panic!("expected SetRoutes on a, got {other:?}"),
    }
    match recv(&mut b.frame_rx).await {
        Frame::SetRoutes(rs) => assert_eq!(rs.len(), 1),
        other => panic!("expected SetRoutes on b, got {other:?}"),
    }
    h.hub.shutdown().await;
}

#[tokio::test]
async fn broadcast_text_reaches_all_conns() {
    let h = spawn(build_cfg(60)).await;
    let mut a = fake_conn(&h, "x:a").await;
    let mut b = fake_conn(&h, "x:b").await;

    let n = h.hub.broadcast_text("hello".into()).await;
    assert_eq!(n, 2);

    match recv(&mut a.frame_rx).await {
        Frame::Text(s) => assert_eq!(s, "hello"),
        other => panic!("expected Text on a, got {other:?}"),
    }
    match recv(&mut b.frame_rx).await {
        Frame::Text(s) => assert_eq!(s, "hello"),
        other => panic!("expected Text on b, got {other:?}"),
    }
    h.hub.shutdown().await;
}

#[tokio::test]
async fn broadcast_file_reaches_all_conns() {
    let h = spawn(build_cfg(60)).await;
    let mut a = fake_conn(&h, "x:a").await;

    let n = h
        .hub
        .broadcast_file("foo.bin".into(), vec![1, 2, 3, 4])
        .await;
    assert_eq!(n, 1);

    match recv(&mut a.frame_rx).await {
        Frame::File { name, data } => {
            assert_eq!(name, "foo.bin");
            assert_eq!(data, vec![1, 2, 3, 4]);
        }
        other => panic!("expected File, got {other:?}"),
    }
    h.hub.shutdown().await;
}

#[tokio::test]
async fn make_net_persists_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("server.toml");
    let cfg = build_cfg(60);
    cfg.save(&cfg_path).await.unwrap();
    let h = spawn_with_path(cfg, Some(cfg_path.clone())).await;

    let uuid = h.hub.make_net("hml".into()).await.unwrap();
    let on_disk = ServerConfig::load(&cfg_path).await.unwrap();
    assert!(on_disk
        .networks
        .iter()
        .any(|n| n.uuid == uuid && n.name == "hml"));
    h.hub.shutdown().await;
}
