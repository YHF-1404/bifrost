//! End-to-end controller tests for [`bifrost_client::app::App`].
//!
//! Each test plays the role of both the connection task (by feeding
//! [`ConnEvent`]s and reading the `out_rx` queue of frames the
//! controller wants to send) and the user (by sending [`UserCmd`]s).
//! The local TAP is the [`MockPlatform`] / [`MockTap`] from
//! `bifrost-net` so the data plane can be observed end-to-end without
//! kernel access.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bifrost_client::app::{App, AppPorts};
use bifrost_client::conn::ConnEvent;
use bifrost_client::repl::UserCmd;
use bifrost_core::config::ClientConfig;
use bifrost_net::mock::MockPlatform;
use bifrost_net::{Platform, Tap};
use bifrost_proto::{Frame, PROTOCOL_VERSION};
use tempfile::TempDir;
use tokio::sync::mpsc;
use uuid::Uuid;

const SHORT: Duration = Duration::from_millis(200);

// ── Harness ───────────────────────────────────────────────────────────────

struct Harness {
    out_rx: mpsc::Receiver<Frame>,
    events_tx: mpsc::Sender<ConnEvent>,
    user_tx: mpsc::Sender<UserCmd>,
    platform: Arc<MockPlatform>,
    cfg_path: PathBuf,
    /// Hold the tempdir so it doesn't get reaped mid-test.
    _tmp: TempDir,
    /// Hold the App's join handle to know when it exits.
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn spawn_app(joined_network: Option<Uuid>) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("client.toml");

    let mut cfg = ClientConfig::default();
    cfg.client.uuid = Uuid::new_v4().to_string();
    cfg.client.save_dir = tmp.path().join("recv").to_string_lossy().into_owned();
    if let Some(net) = joined_network {
        cfg.client.joined_network = net.to_string();
    }
    cfg.save(&cfg_path).await.unwrap();

    let platform = MockPlatform::new();
    let (out_tx, out_rx) = mpsc::channel(64);
    let (events_tx, events_rx) = mpsc::channel(64);
    let (user_tx, user_rx) = mpsc::channel(64);

    let app = App::new(AppPorts {
        cfg,
        cfg_path: cfg_path.clone(),
        platform: platform.clone() as Arc<dyn Platform>,
        out_tx,
        events_rx,
        user_rx,
    });
    let join = tokio::spawn(app.run());

    Harness {
        out_rx,
        events_tx,
        user_tx,
        platform,
        cfg_path,
        _tmp: tmp,
        join,
    }
}

async fn recv_frame(rx: &mut mpsc::Receiver<Frame>) -> Frame {
    tokio::time::timeout(SHORT, rx.recv())
        .await
        .expect("frame timed out")
        .expect("out_rx closed")
}

async fn try_recv(rx: &mut mpsc::Receiver<Frame>, dur: Duration) -> Option<Frame> {
    tokio::time::timeout(dur, rx.recv()).await.ok().flatten()
}

/// Walk through Connected → Hello/HelloAck → JoinOk for `net`.
/// Returns the harness so the caller can keep driving.
async fn join_network(
    h: &mut Harness,
    net: Uuid,
    tap_suffix: &str,
    ip: Option<&str>,
) {
    h.events_tx.send(ConnEvent::Connected).await.unwrap();

    // Expect Frame::Hello
    match recv_frame(&mut h.out_rx).await {
        Frame::Hello { version, caps, .. } => {
            assert_eq!(version, PROTOCOL_VERSION);
            assert_eq!(caps, 0);
        }
        other => panic!("expected Hello, got {other:?}"),
    }
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    // User issues join.
    h.user_tx.send(UserCmd::Join(net)).await.unwrap();

    // Expect Frame::Join
    match recv_frame(&mut h.out_rx).await {
        Frame::Join { net_uuid } => assert_eq!(net_uuid, net),
        other => panic!("expected Join, got {other:?}"),
    }

    // Server replies JoinOk
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::JoinOk {
            tap_suffix: tap_suffix.to_owned(),
            ip: ip.map(|s| s.to_owned()),
        }))
        .await
        .unwrap();
}

async fn quit(h: Harness) {
    h.user_tx.send(UserCmd::Quit).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), h.join)
        .await
        .expect("app didn't exit in time")
        .expect("app panicked");
    result.expect("app returned an error");
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn handshake_then_join_creates_local_tap() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();

    join_network(&mut h, net, "abc12345", Some("10.0.0.5/24")).await;

    // Give App a moment to process JoinOk → spawn SessionTask.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(h.platform.taps_count().await, 1);
    let tap = h.platform.last_tap().await.unwrap();
    assert_eq!(tap.name(), "tapabc12345");
    assert_eq!(
        tap.snapshot().await.ip.unwrap().to_string(),
        "10.0.0.5/24"
    );

    // joined_network must have been persisted.
    let on_disk = ClientConfig::load(&h.cfg_path).await.unwrap();
    assert_eq!(on_disk.client.joined_network, net.to_string());
    assert_eq!(on_disk.tap.ip, "10.0.0.5/24");

    quit(h).await;
}

#[tokio::test]
async fn server_eth_frames_are_written_to_tap() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();
    join_network(&mut h, net, "00112233", None).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let tap = h.platform.last_tap().await.unwrap();
    let payload = b"frame-from-server".to_vec();

    h.events_tx
        .send(ConnEvent::FrameIn(Frame::Eth(payload.clone())))
        .await
        .unwrap();

    let written = tap.pop_written_timeout(SHORT).await.expect("no write");
    assert_eq!(written, payload);

    quit(h).await;
}

#[tokio::test]
async fn tap_frames_flow_back_out_to_server() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();
    join_network(&mut h, net, "deadbeef", None).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let tap = h.platform.last_tap().await.unwrap();
    tap.inject_frame(b"frame-to-server".to_vec()).await;

    match recv_frame(&mut h.out_rx).await {
        Frame::Eth(b) => assert_eq!(b, b"frame-to-server"),
        other => panic!("expected Eth, got {other:?}"),
    }

    quit(h).await;
}

#[tokio::test]
async fn auto_rejoin_after_disconnect() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();
    join_network(&mut h, net, "11223344", None).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let tap = h.platform.last_tap().await.unwrap();
    let initial_taps = h.platform.taps_count().await;

    // Drop the connection.
    h.events_tx.send(ConnEvent::Disconnected).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Reconnect: Connected → controller emits Hello.
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    match recv_frame(&mut h.out_rx).await {
        Frame::Hello { .. } => {}
        other => panic!("expected Hello, got {other:?}"),
    }
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    // After HelloAck the controller should re-emit Join automatically.
    match recv_frame(&mut h.out_rx).await {
        Frame::Join { net_uuid } => assert_eq!(net_uuid, net),
        other => panic!("expected Join, got {other:?}"),
    }

    // Server replies JoinOk → controller rebinds the existing session.
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::JoinOk {
            tap_suffix: "11223344".into(),
            ip: None,
        }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    // No new TAP must have been created.
    assert_eq!(h.platform.taps_count().await, initial_taps);

    // Frames still flow.
    tap.inject_frame(b"after-rejoin".to_vec()).await;
    match recv_frame(&mut h.out_rx).await {
        Frame::Eth(b) => assert_eq!(b, b"after-rejoin"),
        other => panic!("expected Eth, got {other:?}"),
    }

    quit(h).await;
}

#[tokio::test]
async fn join_deny_clears_intent_no_tap() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();

    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    let _ = recv_frame(&mut h.out_rx).await; // Hello
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();
    h.user_tx.send(UserCmd::Join(net)).await.unwrap();
    let _ = recv_frame(&mut h.out_rx).await; // Join
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::JoinDeny {
            reason: "denied_by_admin".into(),
        }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    assert_eq!(h.platform.taps_count().await, 0);
    let on_disk = ClientConfig::load(&h.cfg_path).await.unwrap();
    assert_eq!(on_disk.client.joined_network, "");

    quit(h).await;
}

#[tokio::test]
async fn leave_destroys_tap() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();
    join_network(&mut h, net, "55667788", None).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let tap = h.platform.last_tap().await.unwrap();

    h.user_tx.send(UserCmd::Leave).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    assert!(tap.snapshot().await.destroyed);
    let on_disk = ClientConfig::load(&h.cfg_path).await.unwrap();
    assert_eq!(on_disk.client.joined_network, "");

    quit(h).await;
}

#[tokio::test]
async fn join_before_helloack_defers_until_handshake() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();

    // User issues join before the connection is even up.
    h.user_tx.send(UserCmd::Join(net)).await.unwrap();
    // No frame should be emitted yet.
    assert!(try_recv(&mut h.out_rx, Duration::from_millis(50))
        .await
        .is_none());

    // Now simulate the connection.
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    match recv_frame(&mut h.out_rx).await {
        Frame::Hello { .. } => {}
        other => panic!("expected Hello, got {other:?}"),
    }
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    // After HelloAck, the deferred Join should fire.
    match recv_frame(&mut h.out_rx).await {
        Frame::Join { net_uuid } => assert_eq!(net_uuid, net),
        other => panic!("expected Join, got {other:?}"),
    }

    quit(h).await;
}

#[tokio::test]
async fn persisted_joined_network_triggers_auto_join_on_first_handshake() {
    let net = Uuid::new_v4();
    let mut h = spawn_app(Some(net)).await;

    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    match recv_frame(&mut h.out_rx).await {
        Frame::Hello { .. } => {}
        other => panic!("expected Hello, got {other:?}"),
    }
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    // No user command — Join must fire because of persisted state.
    match recv_frame(&mut h.out_rx).await {
        Frame::Join { net_uuid } => assert_eq!(net_uuid, net),
        other => panic!("expected Join, got {other:?}"),
    }

    quit(h).await;
}

#[tokio::test]
async fn set_ip_and_set_routes_propagate_to_tap_and_disk() {
    let mut h = spawn_app(None).await;
    let net = Uuid::new_v4();
    join_network(&mut h, net, "abcdef00", Some("10.0.0.5/24")).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let tap = h.platform.last_tap().await.unwrap();

    h.events_tx
        .send(ConnEvent::FrameIn(Frame::SetIp {
            ip: Some("10.0.0.42/24".into()),
        }))
        .await
        .unwrap();
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::SetRoutes(vec![
            bifrost_proto::RouteEntry {
                dst: "192.168.10.0/24".into(),
                via: "10.0.0.1".into(),
            },
        ])))
        .await
        .unwrap();
    // Sync via an inbound Eth that we expect to land on the TAP.
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::Eth(b"sync".to_vec())))
        .await
        .unwrap();
    let _ = tap.pop_written_timeout(SHORT).await.unwrap();

    let snap = tap.snapshot().await;
    assert_eq!(snap.ip.unwrap().to_string(), "10.0.0.42/24");
    assert_eq!(snap.routes.len(), 1);
    assert_eq!(snap.routes[0].dst.to_string(), "192.168.10.0/24");

    let on_disk = ClientConfig::load(&h.cfg_path).await.unwrap();
    assert_eq!(on_disk.tap.ip, "10.0.0.42/24");
    assert_eq!(on_disk.routes.len(), 1);

    quit(h).await;
}

#[tokio::test]
async fn ping_is_answered_with_pong() {
    let mut h = spawn_app(None).await;
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    let _ = recv_frame(&mut h.out_rx).await; // Hello
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    h.events_tx
        .send(ConnEvent::FrameIn(Frame::Ping(0xCAFEBABE)))
        .await
        .unwrap();

    match recv_frame(&mut h.out_rx).await {
        Frame::Pong(n) => assert_eq!(n, 0xCAFEBABE),
        other => panic!("expected Pong, got {other:?}"),
    }

    quit(h).await;
}

#[tokio::test]
async fn send_text_emits_frame() {
    let mut h = spawn_app(None).await;
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    let _ = recv_frame(&mut h.out_rx).await; // Hello
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    h.user_tx
        .send(UserCmd::SendText("hello world".into()))
        .await
        .unwrap();

    match recv_frame(&mut h.out_rx).await {
        Frame::Text(s) => assert_eq!(s, "hello world"),
        other => panic!("expected Text, got {other:?}"),
    }

    quit(h).await;
}

#[tokio::test]
async fn quit_with_no_session_returns_cleanly() {
    let h = spawn_app(None).await;
    quit(h).await;
}
