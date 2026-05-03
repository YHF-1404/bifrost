//! End-to-end tests for the [`SessionTask`] state machine, exercised
//! against the in-memory [`MockTap`].
//!
//! These tests assert *behavior at the channel boundary*: what flows
//! out of `evt_rx` and `conn_rx` for any given input on `cmd_tx` plus
//! frames injected on the mock TAP. Internal `LoopState` is deliberately
//! not introspected — that's an implementation detail.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::{DeathReason, SessionCmd, SessionEvt, SessionId, SessionTask};
use bifrost_net::mock::MockTap;
use bifrost_net::Tap;
use bifrost_proto::{Frame, RouteEntry as WireRoute};
use tokio::sync::mpsc;
use uuid::Uuid;

const SHORT: Duration = Duration::from_millis(150);

struct Harness {
    cmd_tx: mpsc::Sender<SessionCmd>,
    evt_rx: mpsc::Receiver<SessionEvt>,
    conn_rx: mpsc::Receiver<Frame>,
    tap: Arc<MockTap>,
    /// Counters shared with the running task — tests can observe.
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
    /// Held to keep the session task alive for the duration of the test.
    /// Tests don't `.await` on it because death is observed via `evt_rx`.
    #[allow(dead_code)]
    join: tokio::task::JoinHandle<()>,
}

fn spawn(disconnect_timeout: Option<Duration>) -> Harness {
    let tap = MockTap::new("tap-test");
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (evt_tx, evt_rx) = mpsc::channel(8);
    let (conn_tx, conn_rx) = mpsc::channel::<Frame>(16);
    let bytes_in = Arc::new(AtomicU64::new(0));
    let bytes_out = Arc::new(AtomicU64::new(0));

    let task = SessionTask::new(
        SessionId(1),
        Uuid::nil(),
        Uuid::nil(),
        tap.clone() as Arc<dyn Tap>,
        cmd_rx,
        evt_tx,
        disconnect_timeout,
        bytes_in.clone(),
        bytes_out.clone(),
    );
    let join = tokio::spawn(task.run(conn_tx));

    Harness {
        cmd_tx,
        evt_rx,
        conn_rx,
        tap,
        bytes_in,
        bytes_out,
        join,
    }
}

/// Wait for the session to die and return its reason. Takes the
/// channel by mut-ref so callers can have moved other `Harness` fields
/// out of the way (e.g. by `drop(h.cmd_tx)`).
async fn await_death(evt_rx: &mut mpsc::Receiver<SessionEvt>) -> DeathReason {
    let evt = tokio::time::timeout(SHORT, evt_rx.recv())
        .await
        .expect("died event timed out")
        .expect("evt_rx closed without death event");
    let SessionEvt::Died { reason, .. } = evt;
    reason
}

// ── Joined-state behavior ──────────────────────────────────────────────────

#[tokio::test]
async fn joined_forwards_tap_to_conn() {
    let mut h = spawn(Some(Duration::from_secs(60)));

    h.tap.inject_frame(b"abcdef".to_vec()).await;
    let f = tokio::time::timeout(SHORT, h.conn_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f, Frame::Eth(b"abcdef".to_vec()));

    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubKill);
}

#[tokio::test]
async fn joined_eth_in_writes_to_tap() {
    let mut h = spawn(Some(Duration::from_secs(60)));

    h.cmd_tx
        .send(SessionCmd::EthIn(b"hello-tap".to_vec()))
        .await
        .unwrap();

    let written = h.tap.pop_written_timeout(SHORT).await.expect("no write");
    assert_eq!(written, b"hello-tap");

    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubKill);
}

#[tokio::test]
async fn byte_counters_track_both_directions() {
    let mut h = spawn(Some(Duration::from_secs(60)));

    // Conn → TAP: 9 bytes inbound.
    h.cmd_tx
        .send(SessionCmd::EthIn(b"hello-tap".to_vec()))
        .await
        .unwrap();
    let _ = h.tap.pop_written_timeout(SHORT).await.expect("no write");

    // TAP → Conn: 11 bytes outbound.
    h.tap.inject_frame(b"world-from!".to_vec()).await;
    match recv_frame(&mut h.conn_rx, SHORT).await {
        Some(Frame::Eth(e)) => assert_eq!(e, b"world-from!"),
        other => panic!("expected Eth, got {other:?}"),
    }

    // Counters reflect payload bytes only, in atomic order.
    assert_eq!(h.bytes_in.load(Ordering::Relaxed), 9);
    assert_eq!(h.bytes_out.load(Ordering::Relaxed), 11);

    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubKill);
}

async fn recv_frame(rx: &mut mpsc::Receiver<Frame>, timeout: Duration) -> Option<Frame> {
    tokio::time::timeout(timeout, rx.recv()).await.ok().flatten()
}

#[tokio::test]
async fn set_ip_and_set_routes_apply_to_tap() {
    let mut h = spawn(Some(Duration::from_secs(60)));

    h.cmd_tx
        .send(SessionCmd::SetIp(Some("10.0.0.5/24".to_owned())))
        .await
        .unwrap();
    h.cmd_tx
        .send(SessionCmd::SetRoutes(vec![
            WireRoute {
                dst: "192.168.1.0/24".to_owned(),
                via: "10.0.0.1".to_owned(),
            },
            WireRoute {
                dst: "garbage-route".to_owned(), // dropped silently
                via: "10.0.0.2".to_owned(),
            },
        ]))
        .await
        .unwrap();

    // Force a sync point: send something with a guaranteed reaction.
    h.cmd_tx
        .send(SessionCmd::EthIn(b"sync".to_vec()))
        .await
        .unwrap();
    let _ = h.tap.pop_written_timeout(SHORT).await.unwrap();

    let snap = h.tap.snapshot().await;
    assert_eq!(snap.ip.unwrap().to_string(), "10.0.0.5/24");
    assert_eq!(snap.routes.len(), 1, "malformed route must be dropped");
    assert_eq!(snap.routes[0].dst.to_string(), "192.168.1.0/24");

    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubKill);
}

#[tokio::test]
async fn set_ip_with_bare_ip_promotes_to_host_prefix() {
    let mut h = spawn(Some(Duration::from_secs(60)));

    h.cmd_tx
        .send(SessionCmd::SetIp(Some("10.0.0.7".to_owned())))
        .await
        .unwrap();
    // Sync.
    h.cmd_tx
        .send(SessionCmd::EthIn(b"x".to_vec()))
        .await
        .unwrap();
    let _ = h.tap.pop_written_timeout(SHORT).await.unwrap();

    assert_eq!(
        h.tap.snapshot().await.ip.unwrap().to_string(),
        "10.0.0.7/32"
    );

    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    let _ = await_death(&mut h.evt_rx).await;
}

#[tokio::test]
async fn set_ip_with_none_clears_address() {
    let mut h = spawn(Some(Duration::from_secs(60)));
    h.cmd_tx
        .send(SessionCmd::SetIp(Some("10.0.0.7/24".to_owned())))
        .await
        .unwrap();
    h.cmd_tx.send(SessionCmd::SetIp(None)).await.unwrap();

    h.cmd_tx
        .send(SessionCmd::EthIn(b"x".to_vec()))
        .await
        .unwrap();
    let _ = h.tap.pop_written_timeout(SHORT).await.unwrap();

    assert_eq!(h.tap.snapshot().await.ip, None);

    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    let _ = await_death(&mut h.evt_rx).await;
}

// ── Disconnect / reconnect / timeout ───────────────────────────────────────

#[tokio::test]
async fn unbind_then_timeout_dies_with_timeout_reason() {
    let mut h = spawn(Some(Duration::from_millis(80)));

    h.cmd_tx.send(SessionCmd::UnbindConn).await.unwrap();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::Timeout);
    assert!(h.tap.snapshot().await.destroyed);
}

#[tokio::test]
async fn unbind_then_rebind_returns_to_joined() {
    let mut h = spawn(Some(Duration::from_millis(500)));
    h.cmd_tx.send(SessionCmd::UnbindConn).await.unwrap();

    // Replace the conn channel while disconnected.
    let (new_conn_tx, mut new_conn_rx) = mpsc::channel::<Frame>(8);
    h.cmd_tx
        .send(SessionCmd::BindConn(new_conn_tx))
        .await
        .unwrap();

    // Frames now go to the new conn.
    h.tap.inject_frame(b"after-rebind".to_vec()).await;
    let f = tokio::time::timeout(SHORT, new_conn_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f, Frame::Eth(b"after-rebind".to_vec()));

    // The old `conn_rx` should observe its channel as closed — the
    // session dropped the old sender when it accepted BindConn.
    let stale = tokio::time::timeout(Duration::from_millis(50), h.conn_rx.recv()).await;
    assert!(
        matches!(stale, Ok(None)),
        "old conn_rx should see channel closed, got {stale:?}"
    );

    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubKill);
}

#[tokio::test]
async fn dropping_conn_rx_implicitly_unbinds_then_times_out() {
    // Hub never sends UnbindConn — the conn task simply went away.
    let mut h = spawn(Some(Duration::from_millis(80)));

    drop(h.conn_rx); // simulate conn task dying

    // Push a frame so the session's TAP-read path notices the dead channel.
    h.tap.inject_frame(b"trigger".to_vec()).await;

    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::Timeout);
}

#[tokio::test]
async fn no_timeout_stays_disconnected_indefinitely() {
    // Client-style: no disconnect deadline. Without an explicit Kill
    // the session must remain alive even after a long unbind.
    let mut h = spawn(None);
    h.cmd_tx.send(SessionCmd::UnbindConn).await.unwrap();

    // The session must NOT have died after a generous wait.
    let evt = tokio::time::timeout(Duration::from_millis(150), h.evt_rx.recv()).await;
    assert!(evt.is_err(), "session must not time out when disconnect is None");

    // It must still respond to Kill.
    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubKill);
}

// ── Termination paths ──────────────────────────────────────────────────────

#[tokio::test]
async fn kill_in_disconnected_dies_immediately() {
    let mut h = spawn(Some(Duration::from_secs(10)));
    h.cmd_tx.send(SessionCmd::UnbindConn).await.unwrap();
    h.cmd_tx.send(SessionCmd::Kill).await.unwrap();

    let started = std::time::Instant::now();
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubKill);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "kill should not wait for the disconnect deadline"
    );
}

#[tokio::test]
async fn dropping_cmd_rx_yields_hub_gone_in_joined() {
    let mut h = spawn(Some(Duration::from_secs(10)));
    drop(h.cmd_tx);
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubGone);
    assert!(h.tap.snapshot().await.destroyed);
}

#[tokio::test]
async fn dropping_cmd_rx_yields_hub_gone_in_disconnected() {
    let mut h = spawn(Some(Duration::from_secs(10)));
    h.cmd_tx.send(SessionCmd::UnbindConn).await.unwrap();
    drop(h.cmd_tx);
    assert_eq!(await_death(&mut h.evt_rx).await, DeathReason::HubGone);
}
