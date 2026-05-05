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
use bifrost_core::{
    ConnId, ConnLink, DeviceUpdate, Hub, HubEvent, HubHandle, MakeNetResult, SessionCmd,
};
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Platform, Tap};
use bifrost_proto::{Frame, PROTOCOL_VERSION};
use tokio::sync::mpsc;
use uuid::Uuid;

const SHORT: Duration = Duration::from_millis(200);

// ─── Harness ──────────────────────────────────────────────────────────────

struct Harness {
    hub: HubHandle,
    platform: Arc<MockPlatform>,
    #[allow(dead_code)]
    join: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// Fetch the bridge the Hub created on startup. Phase-2 the Hub
    /// owns its bridges via `MockPlatform::create_bridge`; the test
    /// harness no longer creates a `MockBridge` of its own. With one
    /// network per test (the common case), `last_bridge()` is the
    /// right one; tests with two networks should look it up by name
    /// via `bridge_named`.
    async fn bridge(&self) -> Arc<MockBridge> {
        self.platform
            .last_bridge()
            .await
            .expect("hub did not create a bridge")
    }
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
        admitted: true,
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
        admitted: true,
    }
}

async fn spawn(cfg: ServerConfig) -> Harness {
    spawn_with_path(cfg, None).await
}

async fn spawn_with_path(cfg: ServerConfig, cfg_path: Option<std::path::PathBuf>) -> Harness {
    let platform = MockPlatform::new();
    let (hub, handle) = Hub::new(
        cfg,
        cfg_path,
        platform.clone() as Arc<dyn Platform>,
    );
    let join = tokio::spawn(hub.run());
    // Give Hub::run a tick to bootstrap its per-network bridges; tests
    // that immediately probe the platform need them in place.
    tokio::time::sleep(Duration::from_millis(10)).await;
    Harness {
        hub: handle,
        platform,
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

/// Phase 3 — `handle_hello` always pushes one `AssignNet` so the
/// client knows its server-authoritative assignment. Tests that only
/// care about post-Hello behavior call this to consume it.
async fn drain_hello_assign(rx: &mut mpsc::Receiver<Frame>) {
    let f = recv(rx).await;
    assert!(
        matches!(f, Frame::AssignNet { .. }),
        "expected AssignNet from Hello, got {f:?}"
    );
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn whitelisted_join_creates_tap_and_binds_conn() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();

    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, "10.0.0.5/24"));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "1.2.3.4:1").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
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
    assert_eq!(h.bridge().await.snapshot().await.ports, vec![tap.name().to_owned()]);

    // Conn must have received Some(session_cmd_tx) on bind_rx.
    let bound = recv_bind(&mut c.bind_rx).await;
    assert!(bound.is_some(), "conn must be bound");

    h.hub.shutdown().await;
}

#[tokio::test]
async fn unknown_network_is_denied() {
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", Uuid::new_v4()));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, Uuid::new_v4(), PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
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
    cfg.networks.push(NetRecord::new("n", net));
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
async fn fresh_hello_creates_pending_unassigned_row() {
    // Phase 3 — server is authoritative on assignment. A first Hello
    // from an unknown client creates a pending_clients row (left pane
    // in the WebUI) and any Join attempt is denied with "unassigned"
    // until an admin assigns it.
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    // Give the hub a beat to write the pending_clients row.
    tokio::time::sleep(Duration::from_millis(20)).await;

    h.hub.join(c.id, net).await;
    match recv(&mut c.frame_rx).await {
        Frame::JoinDeny { reason } => assert_eq!(reason, "unassigned"),
        other => panic!("expected JoinDeny(unassigned), got {other:?}"),
    }

    // Device list (no filter) shows the unassigned row with net_uuid=None.
    let devices = h.hub.device_list(None).await;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].client_uuid, client);
    assert_eq!(devices[0].net_uuid, None);
    assert!(!devices[0].admitted);
    assert!(devices[0].online);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn assign_then_join_creates_pending_admit_row_and_holds_conn() {
    // After Hello, an admin's `assign_client` creates an approved_clients
    // row with admitted=false. The client (re-)joins and the server
    // holds the conn silently in `pending` — waiting for admit toggle.
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Admin assigns. This sends `Frame::AssignNet { Some(net) }` to the
    // conn; in the test harness we observe it on `frame_rx`.
    let result = h.hub.assign_client(client, Some(net)).await;
    assert!(matches!(result, bifrost_core::AssignClientResult::Ok(_)));

    match recv(&mut c.frame_rx).await {
        Frame::AssignNet { net_uuid } => assert_eq!(net_uuid, Some(net)),
        other => panic!("expected AssignNet, got {other:?}"),
    }

    // Now the client (simulating its on_assign_net handler) issues Join.
    h.hub.join(c.id, net).await;
    // Server holds silently — no JoinOk yet.
    assert!(try_recv_silent(&mut c.frame_rx, Duration::from_millis(80))
        .await
        .is_none());

    let snap = h.hub.list().await.unwrap();
    assert_eq!(snap.pending.len(), 1);
    assert_eq!(snap.pending[0].client_uuid, client);

    let devices = h.hub.device_list(Some(net)).await;
    assert_eq!(devices.len(), 1);
    assert!(!devices[0].admitted);
    assert!(devices[0].online);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn admit_toggle_on_promotes_pending_to_session() {
    // Phase 3 flow: Hello → assign → Join (silent) → admit → JoinOk.
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = h.hub.assign_client(client, Some(net)).await;
    // Drain the AssignNet frame.
    let _ = recv(&mut c.frame_rx).await;
    h.hub.join(c.id, net).await;

    // Flip admitted on.
    let result = h
        .hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                admitted: Some(true),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, bifrost_core::DeviceSetResult::Ok(_)));

    match recv(&mut c.frame_rx).await {
        Frame::JoinOk { .. } => {}
        other => panic!("expected JoinOk, got {other:?}"),
    }
    assert_eq!(h.platform.taps_count().await, 1);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn admit_toggle_off_kicks_session_keeps_row_in_pending() {
    // Admitted device with a live session. Flipping admitted=false
    // should kick the session, drop the conn, and leave the row in
    // approved_clients with admitted=false.
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, "10.0.0.5/24"));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await; // JoinOk
    let _ = recv_bind(&mut c.bind_rx).await;

    // Live session.
    let snap = h.hub.list().await.unwrap();
    assert_eq!(snap.sessions.len(), 1);

    // Kick.
    let result = h
        .hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                admitted: Some(false),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, bifrost_core::DeviceSetResult::Ok(_)));

    // Wait briefly for the session-died → handle_session_died cleanup
    // path to run.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let snap = h.hub.list().await.unwrap();
    assert_eq!(snap.sessions.len(), 0, "session must be killed");

    // The row remains in the device list with admitted=false.
    let devices = h.hub.device_list(Some(net)).await;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].client_uuid, client);
    assert!(!devices[0].admitted);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn make_net_persists_into_list() {
    let h = spawn(build_cfg(60)).await;
    let MakeNetResult::Ok(uuid) = h.hub.make_net("hml-net".into(), None).await.unwrap() else {
        panic!("expected MakeNetResult::Ok");
    };
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
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await; // JoinOk
    let _ = recv_bind(&mut c.bind_rx).await; // bound

    h.hub.disconnect(c.id).await;

    // Session remains, but bound_conn cleared.
    let snap = h.hub.list().await.unwrap();
    assert_eq!(snap.sessions.len(), 1);
    assert_eq!(snap.sessions[0].bound_conn, None);
    // TAP still present in bridge.
    assert_eq!(h.bridge().await.snapshot().await.ports.len(), 1);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn reconnect_reuses_session_no_new_tap() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, "10.0.0.7/24"));
    let h = spawn(cfg).await;

    // First join.
    let mut c1 = fake_conn(&h, "x1").await;
    h.hub.hello(c1.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c1.frame_rx).await;
    h.hub.join(c1.id, net).await;
    let _ = recv(&mut c1.frame_rx).await;
    let _ = recv_bind(&mut c1.bind_rx).await;
    assert_eq!(h.platform.taps_count().await, 1);

    // First conn drops.
    h.hub.disconnect(c1.id).await;

    // Second conn reconnects.
    let mut c2 = fake_conn(&h, "x2").await;
    h.hub.hello(c2.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c2.frame_rx).await;
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
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c1 = fake_conn(&h, "x1").await;
    h.hub.hello(c1.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c1.frame_rx).await;
    h.hub.join(c1.id, net).await;
    let _ = recv(&mut c1.frame_rx).await;
    let _ = recv_bind(&mut c1.bind_rx).await; // initial bind

    // Second conn from same client without disconnecting the first.
    let mut c2 = fake_conn(&h, "x2").await;
    h.hub.hello(c2.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c2.frame_rx).await;
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
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await;
    let _ = recv_bind(&mut c.bind_rx).await;

    h.hub.disconnect(c.id).await;
    // Allow the session task to time out and the hub to process death.
    tokio::time::sleep(Duration::from_millis(120)).await;

    let snap = h.hub.list().await.unwrap();
    assert!(snap.sessions.is_empty(), "session should be cleaned up");
    let bridge = h.bridge().await.snapshot().await;
    assert!(bridge.ports.is_empty(), "tap should be removed from bridge");

    h.hub.shutdown().await;
}

#[tokio::test]
async fn routes_pushed_after_join_and_filter_self_via() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
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
    drain_hello_assign(&mut c.frame_rx).await;
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
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await;
    let _ = recv_bind(&mut c.bind_rx).await;

    let tap = h.platform.last_tap().await.unwrap();
    // Capture the bridge handle BEFORE shutdown moves `h.join` and
    // makes `h.bridge()` calls trip the partial-move check.
    let bridge = h.bridge().await;

    h.hub.shutdown().await;
    // Allow the hub task to finish its shutdown sequence.
    let _ = tokio::time::timeout(Duration::from_secs(2), h.join).await;

    assert!(bridge.snapshot().await.destroyed, "bridge must be destroyed");
    assert!(tap.snapshot().await.destroyed, "tap must be destroyed");
}

// ── Newly-added REPL commands: device_set / device_push / broadcast ──────

#[tokio::test]
async fn device_set_ip_pushes_to_live_session() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients.push(approved(client, net, ""));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
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
    cfg.networks.push(NetRecord::new("n", net));
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
    cfg.networks.push(NetRecord::new("n", net));
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
    cfg.networks.push(NetRecord::new("n", net));
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
    cfg.networks.push(NetRecord::new("n", net));
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
    cfg.networks.push(NetRecord::new("n", net));
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
    drain_hello_assign(&mut a.frame_rx).await;
    h.hub.join(a.id, net).await;
    let _ = recv(&mut a.frame_rx).await; // JoinOk
    let _ = recv(&mut a.frame_rx).await; // SetRoutes (initial push)
    let _ = recv_bind(&mut a.bind_rx).await;

    let mut b = fake_conn(&h, "x:2").await;
    h.hub.hello(b.id, client_b, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut b.frame_rx).await;
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

    let MakeNetResult::Ok(uuid) = h.hub.make_net("hml".into(), None).await.unwrap() else {
        panic!("expected MakeNetResult::Ok");
    };
    let on_disk = ServerConfig::load(&cfg_path).await.unwrap();
    assert!(on_disk
        .networks
        .iter()
        .any(|n| n.uuid == uuid && n.name == "hml"));
    h.hub.shutdown().await;
}

#[tokio::test]
async fn metrics_tick_broadcasts_one_sample_per_session() {
    // Spawn Hub with one approved client; bring a conn in; observe at
    // least one MetricsTick on the broadcast subscription. Real-time
    // wait — the sampler ticks at 1 Hz, so up to ~2 s.
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients
        .push(approved(client, net, "10.0.0.5/24"));
    let h = spawn(cfg).await;

    let mut events = h.hub.subscribe();

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    h.hub.join(c.id, net).await;
    let _ = recv(&mut c.frame_rx).await; // JoinOk
    let _ = recv_bind(&mut c.bind_rx).await;

    // Drain device.* events that fire on join, wait up to 2.5 s for
    // the first MetricsTick (sampler ticks at 1 Hz).
    let deadline = tokio::time::Instant::now() + Duration::from_millis(2500);
    let samples = loop {
        let e = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("metrics tick timed out")
            .expect("broadcast closed");
        if let HubEvent::MetricsTick { samples } = e {
            break samples;
        }
    };
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].network, net);
    assert_eq!(samples[0].client_uuid, client);
    // No traffic yet → zero deltas + zero totals.
    assert_eq!(samples[0].bps_in, 0);
    assert_eq!(samples[0].bps_out, 0);
    assert_eq!(samples[0].total_in, 0);
    assert_eq!(samples[0].total_out, 0);
    h.hub.shutdown().await;
}

#[tokio::test]
async fn metrics_tick_skipped_when_no_sessions() {
    // No sessions → no MetricsTick. (Other event variants don't fire
    // either — no joins, no admin actions.) Verify by not seeing one
    // within 2 s.
    let h = spawn(build_cfg(60)).await;
    let mut events = h.hub.subscribe();

    let res = tokio::time::timeout(Duration::from_millis(2000), events.recv()).await;
    assert!(res.is_err(), "expected no events, got {:?}", res);
    h.hub.shutdown().await;
}

/// Wait for the first event matching `pred`, skipping anything else.
/// Returns `None` on timeout.
async fn next_matching<F>(
    events: &mut tokio::sync::broadcast::Receiver<HubEvent>,
    timeout: Duration,
    mut pred: F,
) -> Option<HubEvent>
where
    F: FnMut(&HubEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let e = tokio::time::timeout_at(deadline, events.recv()).await.ok()?.ok()?;
        if pred(&e) {
            return Some(e);
        }
    }
}

#[tokio::test]
async fn admit_toggle_emits_device_online() {
    // Phase 3 — Hello → DevicePending(net=nil) (unassigned),
    // assign  → DeviceChanged (now in net), admit → DeviceOnline.
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let h = spawn(cfg).await;
    let mut events = h.hub.subscribe();

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;

    // First a DevicePending(network=nil) arrives — unassigned client.
    let pending = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::DevicePending { .. })
    })
    .await
    .expect("expected DevicePending");
    assert!(
        matches!(pending, HubEvent::DevicePending { network, .. } if network == Uuid::nil())
    );

    // Assign the client to `net`.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = h.hub.assign_client(client, Some(net)).await;
    let _ = recv(&mut c.frame_rx).await; // AssignNet
    h.hub.join(c.id, net).await;

    // Flip admitted on via device_set.
    h.hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                admitted: Some(true),
                ..Default::default()
            },
        )
        .await;

    // DeviceOnline should follow.
    let online = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::DeviceOnline { .. })
    })
    .await
    .expect("expected DeviceOnline");
    if let HubEvent::DeviceOnline { network, client_uuid, .. } = online {
        assert_eq!(network, net);
        assert_eq!(client_uuid, client);
    }

    // Skip past JoinOk frames the conn received.
    let _ = recv(&mut c.frame_rx).await;
    h.hub.shutdown().await;
}

#[tokio::test]
async fn device_set_emits_changed_on_each_edit() {
    // After the admit-toggle refactor, kick (admitted=false) emits
    // `device.changed { admitted: false }`, NOT `device.removed`. Rows
    // never disappear from the list — they just toggle admitted.
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients.push(approved(client, net, "10.0.0.5/24"));
    let h = spawn(cfg).await;
    let mut events = h.hub.subscribe();

    // Edit name.
    h.hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                name: Some("router".into()),
                ..Default::default()
            },
        )
        .await;
    let changed = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::DeviceChanged { .. })
    })
    .await
    .expect("expected DeviceChanged after rename");
    if let HubEvent::DeviceChanged { network, device } = changed {
        assert_eq!(network, net);
        assert_eq!(device.display_name, "router");
        assert!(device.admitted);
    }

    // Kick.
    h.hub
        .device_set(
            client,
            net,
            DeviceUpdate {
                admitted: Some(false),
                ..Default::default()
            },
        )
        .await;
    let kicked = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::DeviceChanged { device, .. } if !device.admitted)
    })
    .await
    .expect("expected DeviceChanged with admitted=false after kick");
    if let HubEvent::DeviceChanged { network, device } = kicked {
        assert_eq!(network, net);
        assert_eq!(device.client_uuid, client);
        assert!(!device.admitted);
    }

    h.hub.shutdown().await;
}

#[tokio::test]
async fn device_push_emits_routes_changed() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    cfg.approved_clients.push(approved_with_lan(
        client,
        net,
        "10.0.0.5/24",
        &["192.168.10.0/24"],
    ));
    let h = spawn(cfg).await;
    let mut events = h.hub.subscribe();

    h.hub.device_push(net).await;
    let evt = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::RoutesChanged { .. })
    })
    .await
    .expect("expected RoutesChanged");
    if let HubEvent::RoutesChanged { network, routes, .. } = evt {
        assert_eq!(network, net);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].dst, "192.168.10.0/24");
    }
    h.hub.shutdown().await;
}

// ── Phase 2: per-network bridges ─────────────────────────────────────────

#[tokio::test]
async fn each_network_gets_its_own_bridge() {
    // Two networks → MockPlatform sees two distinct bridges, each
    // named after the corresponding NetRecord.bridge_name (which
    // NetRecord::new auto-derives from the UUID).
    let net_a = Uuid::new_v4();
    let net_b = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    let rec_a = NetRecord::new("alpha", net_a);
    let rec_b = NetRecord::new("beta", net_b);
    let br_a_name = rec_a.bridge_name.clone();
    let br_b_name = rec_b.bridge_name.clone();
    cfg.networks.push(rec_a);
    cfg.networks.push(rec_b);
    let h = spawn(cfg).await;

    let bridges = h.platform.bridges().await;
    assert_eq!(bridges.len(), 2, "expected one bridge per network");
    assert!(h.platform.bridge_named(&br_a_name).await.is_some());
    assert!(h.platform.bridge_named(&br_b_name).await.is_some());
    assert_ne!(br_a_name, br_b_name);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn admit_attaches_tap_to_owning_network_bridge_only() {
    // Approving a device on net A must add its TAP to A's bridge —
    // and B's bridge must stay empty.
    let net_a = Uuid::new_v4();
    let net_b = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    let rec_a = NetRecord::new("alpha", net_a);
    let rec_b = NetRecord::new("beta", net_b);
    let br_a_name = rec_a.bridge_name.clone();
    let br_b_name = rec_b.bridge_name.clone();
    cfg.networks.push(rec_a);
    cfg.networks.push(rec_b);
    cfg.approved_clients
        .push(approved(client, net_a, "10.0.0.5/24"));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    h.hub.join(c.id, net_a).await;
    let _ = recv(&mut c.frame_rx).await; // JoinOk

    let br_a = h.platform.bridge_named(&br_a_name).await.unwrap();
    let br_b = h.platform.bridge_named(&br_b_name).await.unwrap();
    assert_eq!(
        br_a.snapshot().await.ports.len(),
        1,
        "net A's bridge should have the TAP",
    );
    assert!(
        br_b.snapshot().await.ports.is_empty(),
        "net B's bridge must NOT see net A's TAP",
    );

    h.hub.shutdown().await;
}

#[tokio::test]
async fn delete_net_destroys_only_that_networks_bridge() {
    // Two live networks; deleting A tears down A's kernel bridge but
    // leaves B's running.
    let net_a = Uuid::new_v4();
    let net_b = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    let rec_a = NetRecord::new("alpha", net_a);
    let rec_b = NetRecord::new("beta", net_b);
    let br_a_name = rec_a.bridge_name.clone();
    let br_b_name = rec_b.bridge_name.clone();
    cfg.networks.push(rec_a);
    cfg.networks.push(rec_b);
    let h = spawn(cfg).await;

    let br_a = h.platform.bridge_named(&br_a_name).await.unwrap();
    let br_b = h.platform.bridge_named(&br_b_name).await.unwrap();
    assert!(!br_a.snapshot().await.destroyed);
    assert!(!br_b.snapshot().await.destroyed);

    let ok = h.hub.delete_net(net_a).await;
    assert!(ok, "delete_net should succeed for an existing net");
    // Hub processes delete asynchronously; let the bridge teardown finish.
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert!(br_a.snapshot().await.destroyed, "net A's bridge must be torn down");
    assert!(!br_b.snapshot().await.destroyed, "net B's bridge must survive");

    h.hub.shutdown().await;
}

#[tokio::test]
async fn legacy_bridge_config_migrates_to_first_networks_fields() {
    // A Phase-1 config: single `[bridge]` block, networks have no
    // bridge_name. After load + save round-trip, the first network
    // inherits the legacy name/ip and the second auto-derives a
    // unique bridge name.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.toml");
    let net_a = Uuid::new_v4();
    let net_b = Uuid::new_v4();
    let toml = format!(
        r#"
[server]
host = "0.0.0.0"
port = 8888
save_dir = "/var/lib/bifrost"

[bridge]
name = "br-bifrost"
ip = "10.0.0.1/24"
disconnect_timeout = 60

[admin]
socket = "/run/bifrost/server.sock"

[[networks]]
name = "alpha"
uuid = "{net_a}"

[[networks]]
name = "beta"
uuid = "{net_b}"
"#
    );
    tokio::fs::write(&path, toml).await.unwrap();
    let cfg = ServerConfig::load(&path).await.unwrap();

    assert_eq!(cfg.networks.len(), 2);
    assert_eq!(
        cfg.networks[0].bridge_name, "br-bifrost",
        "first network should inherit the legacy bridge name"
    );
    assert_eq!(cfg.networks[0].bridge_ip, "10.0.0.1/24");
    assert_ne!(
        cfg.networks[1].bridge_name, "br-bifrost",
        "second network should not collide on bridge name"
    );
    assert_eq!(cfg.networks[1].bridge_ip, "");
    assert!(cfg.networks[1].bridge_name.starts_with("bf-"));
}

// ─── Phase 3: assign_client + bridge_ip ────────────────────────────────────

#[tokio::test]
async fn assign_pending_to_network_emits_assignnet_and_creates_row() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let h = spawn(cfg).await;

    let mut c = fake_conn(&h, "x").await;
    h.hub.hello(c.id, client, PROTOCOL_VERSION).await;
    drain_hello_assign(&mut c.frame_rx).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Sanity: client is in the pending pool now (no approved row).
    let devices = h.hub.device_list(None).await;
    assert!(devices.iter().any(|d| d.client_uuid == client && d.net_uuid.is_none()));

    // Assign.
    let r = h.hub.assign_client(client, Some(net)).await;
    assert!(matches!(r, bifrost_core::AssignClientResult::Ok(_)));

    match recv(&mut c.frame_rx).await {
        Frame::AssignNet { net_uuid } => assert_eq!(net_uuid, Some(net)),
        other => panic!("expected AssignNet, got {other:?}"),
    }

    // Now the row is in the network as admitted=false.
    let devices = h.hub.device_list(Some(net)).await;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].client_uuid, client);
    assert!(!devices[0].admitted);
    assert!(devices[0].tap_ip.is_none());

    h.hub.shutdown().await;
}

#[tokio::test]
async fn assign_detach_moves_admitted_client_to_pending_pool() {
    let net = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let mut row = approved_with_lan(client, net, "10.0.0.5/24", &["192.168.10.0/24"]);
    row.display_name = "router".into();
    cfg.approved_clients.push(row);
    let h = spawn(cfg).await;

    // No live conn — purely a config-level move.
    let r = h.hub.assign_client(client, None).await;
    assert!(matches!(r, bifrost_core::AssignClientResult::Ok(_)));

    let devices = h.hub.device_list(None).await;
    let only = devices.iter().find(|d| d.client_uuid == client).unwrap();
    assert!(only.net_uuid.is_none());
    // display_name + lan_subnets carried across to the pending row.
    assert_eq!(only.display_name, "router");
    assert_eq!(only.lan_subnets, vec!["192.168.10.0/24"]);

    h.hub.shutdown().await;
}

#[tokio::test]
async fn assign_unknown_network_returns_unknown_network() {
    let client = Uuid::new_v4();
    let h = spawn(build_cfg(60)).await;

    let r = h.hub.assign_client(client, Some(Uuid::new_v4())).await;
    assert!(matches!(r, bifrost_core::AssignClientResult::UnknownNetwork));
    h.hub.shutdown().await;
}

#[tokio::test]
async fn delete_net_detaches_clients_to_pending() {
    let net = Uuid::new_v4();
    let cid_a = Uuid::new_v4();
    let cid_b = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let mut a = approved_with_lan(cid_a, net, "10.0.0.5/24", &["192.168.10.0/24"]);
    a.display_name = "a".into();
    let mut b = approved_with_lan(cid_b, net, "10.0.0.6/24", &[]);
    b.display_name = "b".into();
    cfg.approved_clients.push(a);
    cfg.approved_clients.push(b);
    let h = spawn(cfg).await;

    assert!(h.hub.delete_net(net).await);

    // Both clients now in the pending pool.
    let devices = h.hub.device_list(None).await;
    assert_eq!(devices.len(), 2);
    assert!(devices.iter().all(|d| d.net_uuid.is_none()));
    let names: std::collections::HashSet<&str> =
        devices.iter().map(|d| d.display_name.as_str()).collect();
    assert!(names.contains("a"));
    assert!(names.contains("b"));
    h.hub.shutdown().await;
}

#[tokio::test]
async fn set_bridge_ip_rejects_non_16_or_24_prefix() {
    let net = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    let h = spawn(cfg).await;

    // /22 is not a valid Phase-3 bridge prefix.
    let r = h.hub.set_net_bridge_ip(net, "10.0.0.1/22".into()).await;
    assert!(matches!(r, bifrost_core::SetNetBridgeIpResult::Invalid(_)));
    h.hub.shutdown().await;
}

#[tokio::test]
async fn set_bridge_ip_24_to_16_rewrites_client_tap_ip_prefix() {
    let net = Uuid::new_v4();
    let cid = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    let mut net_rec = NetRecord::new("n", net);
    net_rec.bridge_ip = "10.0.0.1/24".into();
    cfg.networks.push(net_rec);
    cfg.approved_clients
        .push(approved(cid, net, "10.0.0.5/24"));
    let h = spawn(cfg).await;

    let r = h.hub.set_net_bridge_ip(net, "10.0.0.1/16".into()).await;
    assert!(matches!(r, bifrost_core::SetNetBridgeIpResult::Ok(_)));

    let devices = h.hub.device_list(Some(net)).await;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].tap_ip.as_deref(), Some("10.0.0.5/16"));

    h.hub.shutdown().await;
}

#[tokio::test]
async fn routes_dirty_only_fires_when_all_clients_online() {
    // Phase 3.x — the push-pulse is a *completion-cue*. While any
    // client in the network is still pending an IP or the admit
    // toggle, the hub stays dirty=false even when their `lan_subnets`
    // aren't in last_pushed. The operator's attention belongs on
    // the IP picker (which pulses amber) and the locked admit
    // switch, not on a route push that wouldn't carry the in-flight
    // client's subnets anyway. Once everyone goes online, the
    // pulse fires.
    let net = Uuid::new_v4();
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("n", net));
    // c1 already admitted with valid IP and routes. At startup the
    // hub seeds last_pushed = derived, so this network is
    // simultaneously fully-online AND derived==pushed → dirty=false.
    cfg.approved_clients.push(ApprovedClient {
        client_uuid: c1,
        net_uuid: net,
        tap_ip: "10.0.0.5/24".into(),
        display_name: String::new(),
        lan_subnets: vec!["192.168.50.0/24".into()],
        admitted: true,
    });
    // c2 dragged in earlier but still pending (no IP, not admitted).
    cfg.approved_clients.push(ApprovedClient {
        client_uuid: c2,
        net_uuid: net,
        tap_ip: String::new(),
        display_name: String::new(),
        lan_subnets: vec!["192.168.60.0/24".into()],
        admitted: false,
    });
    let h = spawn(cfg).await;
    let mut events = h.hub.subscribe();

    // Some client is still mid-setup, so the gate forces dirty=false
    // even though c2's subnets aren't in last_pushed.
    let snap = h.hub.list().await.unwrap();
    assert!(
        !snap.routes_dirty.contains(&net),
        "gate should suppress pulse while c2 is unadmitted"
    );

    // Set c2's IP — still unadmitted, gate still suppresses.
    let _ = h
        .hub
        .device_set(
            c2,
            net,
            DeviceUpdate {
                tap_ip: Some("10.0.0.6/24".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(
        !h.hub.list().await.unwrap().routes_dirty.contains(&net),
        "still suppressed: c2.admitted=false"
    );

    // Admit c2 — now both online; derived gained c2's route, so
    // derived ≠ pushed and the gate opens. Pulse fires.
    let result = h
        .hub
        .device_set(
            c2,
            net,
            DeviceUpdate {
                admitted: Some(true),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, bifrost_core::DeviceSetResult::Ok(_)));
    let evt = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::RoutesDirty { network, dirty } if *network == net && *dirty)
    })
    .await
    .expect("expected RoutesDirty=true after last client admitted");
    let HubEvent::RoutesDirty { dirty, .. } = evt else {
        panic!("event filter mismatch");
    };
    assert!(dirty);
    assert!(h.hub.list().await.unwrap().routes_dirty.contains(&net));

    // Push — last_pushed catches up to derived, gate stays open
    // (still all-online), so dirty flips to false.
    let _ = h.hub.device_push(net).await;
    let evt = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::RoutesDirty { network, dirty } if *network == net && !*dirty)
    })
    .await
    .expect("expected RoutesDirty=false after push");
    match evt {
        HubEvent::RoutesDirty { network, dirty } => {
            assert_eq!(network, net);
            assert!(!dirty);
        }
        _ => unreachable!(),
    }
    assert!(!h.hub.list().await.unwrap().routes_dirty.contains(&net));

    h.hub.shutdown().await;
}

#[tokio::test]
async fn dragging_client_into_net_does_not_pulse_until_all_admitted() {
    // Phase 3.x — drag-in puts the client at admitted=false, tap_ip="".
    // Because the destination now has an unadmitted client, the gate
    // suppresses the push pulse. Setting the IP alone is not enough
    // either — only flipping the admit toggle (which the WebUI
    // unlocks once tap_ip is set) brings the network all-online and
    // re-opens the gate.
    let net_a = Uuid::new_v4();
    let net_b = Uuid::new_v4();
    let client = Uuid::new_v4();
    let mut cfg = build_cfg(60);
    cfg.networks.push(NetRecord::new("a", net_a));
    cfg.networks.push(NetRecord::new("b", net_b));
    // Client starts admitted in net_a with valid IP; at startup
    // derived seeds into last_pushed, so net_a is dirty=false.
    cfg.approved_clients.push(ApprovedClient {
        client_uuid: client,
        net_uuid: net_a,
        tap_ip: "10.0.0.5/24".into(),
        display_name: String::new(),
        lan_subnets: vec!["192.168.77.0/24".into()],
        admitted: true,
    });
    let h = spawn(cfg).await;

    let snap = h.hub.list().await.unwrap();
    assert!(!snap.routes_dirty.contains(&net_a));
    assert!(!snap.routes_dirty.contains(&net_b));

    let mut events = h.hub.subscribe();

    // Drag to net_b — handle_assign_client clears admitted+tap_ip,
    // so net_b now has one pending client. Gate suppresses pulse.
    let r = h.hub.assign_client(client, Some(net_b)).await;
    assert!(matches!(r, bifrost_core::AssignClientResult::Ok(_)));
    let snap = h.hub.list().await.unwrap();
    assert!(
        !snap.routes_dirty.contains(&net_a),
        "net_a empty after drag → dirty=false"
    );
    assert!(
        !snap.routes_dirty.contains(&net_b),
        "net_b has unadmitted client → gate suppresses pulse"
    );

    // Set the IP. Still admitted=false → still suppressed.
    let _ = h
        .hub
        .device_set(
            client,
            net_b,
            DeviceUpdate {
                tap_ip: Some("10.0.1.5/24".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(!h.hub.list().await.unwrap().routes_dirty.contains(&net_b));

    // Admit. Now all-online; derived gained the client's route,
    // pushed is empty for net_b → derived ≠ pushed → dirty=true.
    let _ = h
        .hub
        .device_set(
            client,
            net_b,
            DeviceUpdate {
                admitted: Some(true),
                ..Default::default()
            },
        )
        .await;
    let evt = next_matching(&mut events, Duration::from_millis(500), |e| {
        matches!(e, HubEvent::RoutesDirty { network, dirty } if *network == net_b && *dirty)
    })
    .await
    .expect("expected net_b RoutesDirty=true after admit");
    let HubEvent::RoutesDirty { dirty, .. } = evt else {
        panic!("event filter mismatch");
    };
    assert!(dirty);
    assert!(h.hub.list().await.unwrap().routes_dirty.contains(&net_b));

    h.hub.shutdown().await;
}
