//! End-to-end test for `bifrost-server`'s connection task.
//!
//! Bypasses real TCP via `tokio::io::duplex` so we can observe the
//! full lifecycle (Hello/HelloAck/Join/JoinOk + Eth round-trip) in a
//! deterministic, hermetic way.
//!
//! The "fake client" is a `Framed<DuplexStream, FrameCodec>` we drive
//! by hand; the server side runs the real `conn::run` against the other
//! half of the same duplex pipe and a real [`Hub`] backed by mock
//! platform/bridge.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::{ApprovedClient, NetRecord, ServerConfig};
use bifrost_core::Hub;
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Bridge, Platform};
use bifrost_proto::{Frame, FrameCodec, PROTOCOL_VERSION};
use bifrost_server::conn;
use futures::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::io::duplex;
use tokio_util::codec::Framed;
use uuid::Uuid;

const SHORT: Duration = Duration::from_millis(300);

struct Harness {
    hub: bifrost_core::HubHandle,
    platform: Arc<MockPlatform>,
    bridge: Arc<MockBridge>,
    save_dir: PathBuf,
    server_id: Uuid,
    #[allow(dead_code)]
    _tmp: TempDir,
    #[allow(dead_code)]
    hub_join: tokio::task::JoinHandle<()>,
}

async fn spawn_hub(approved: Vec<(Uuid, Uuid, &str)>, networks: Vec<Uuid>) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = ServerConfig::default();
    cfg.bridge.disconnect_timeout = 60;
    cfg.server.save_dir = tmp.path().join("recv").to_string_lossy().into_owned();
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
    let save_dir = PathBuf::from(&cfg.server.save_dir);

    let (hub, handle) = Hub::new(
        cfg,
        None,
        platform.clone() as Arc<dyn Platform>,
        bridge.clone() as Arc<dyn Bridge>,
    );
    let hub_join = tokio::spawn(hub.run());
    Harness {
        hub: handle,
        platform,
        bridge,
        save_dir,
        server_id: Uuid::new_v4(),
        _tmp: tmp,
        hub_join,
    }
}

/// Hand the server side of a duplex pipe to `conn::run` and return the
/// client side's framed pair.
async fn open_conn(h: &Harness, addr: &str) -> Framed<tokio::io::DuplexStream, FrameCodec> {
    let (server_side, client_side) = duplex(64 * 1024);
    let hub = h.hub.clone();
    let save_dir = h.save_dir.clone();
    let server_id = h.server_id;
    tokio::spawn(conn::run(server_side, addr.to_owned(), hub, server_id, save_dir));
    Framed::new(client_side, FrameCodec::new())
}

async fn recv_frame(framed: &mut Framed<tokio::io::DuplexStream, FrameCodec>) -> Frame {
    tokio::time::timeout(SHORT, framed.next())
        .await
        .expect("frame timed out")
        .expect("stream closed")
        .expect("decode error")
}

#[tokio::test]
async fn hello_returns_helloack() {
    let h = spawn_hub(vec![], vec![]).await;
    let mut client = open_conn(&h, "1.2.3.4:1").await;
    let cuuid = Uuid::new_v4();

    client
        .send(Frame::Hello {
            version: PROTOCOL_VERSION,
            client_uuid: cuuid,
            caps: 0,
        })
        .await
        .unwrap();

    match recv_frame(&mut client).await {
        Frame::HelloAck { version, server_id, .. } => {
            assert_eq!(version, PROTOCOL_VERSION);
            assert_eq!(server_id, h.server_id);
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }
    h.hub.shutdown().await;
}

#[tokio::test]
async fn version_mismatch_closes_connection() {
    let h = spawn_hub(vec![], vec![]).await;
    let mut client = open_conn(&h, "1.2.3.4:2").await;

    client
        .send(Frame::Hello {
            version: PROTOCOL_VERSION + 99,
            client_uuid: Uuid::new_v4(),
            caps: 0,
        })
        .await
        .unwrap();

    match recv_frame(&mut client).await {
        Frame::JoinDeny { reason } => assert!(reason.starts_with("version_mismatch")),
        other => panic!("expected JoinDeny, got {other:?}"),
    }
    // The next read should observe EOF (the conn task closed).
    let nxt = tokio::time::timeout(SHORT, client.next()).await.unwrap();
    assert!(matches!(nxt, None | Some(Err(_))));
    h.hub.shutdown().await;
}

#[tokio::test]
async fn full_join_flow_creates_tap_and_binds() {
    let net = Uuid::new_v4();
    let client_uuid = Uuid::new_v4();
    let h = spawn_hub(
        vec![(client_uuid, net, "10.0.0.5/24")],
        vec![net],
    )
    .await;

    let mut c = open_conn(&h, "1.2.3.4:3").await;
    c.send(Frame::Hello {
        version: PROTOCOL_VERSION,
        client_uuid,
        caps: 0,
    })
    .await
    .unwrap();
    let _ = recv_frame(&mut c).await; // HelloAck

    c.send(Frame::Join { net_uuid: net }).await.unwrap();
    match recv_frame(&mut c).await {
        Frame::JoinOk { ip, .. } => assert_eq!(ip.as_deref(), Some("10.0.0.5/24")),
        other => panic!("expected JoinOk, got {other:?}"),
    }

    // Brief wait for hub to finish session bookkeeping.
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(h.platform.taps_count().await, 1);
    let tap = h.platform.last_tap().await.unwrap();
    assert_eq!(h.bridge.snapshot().await.ports.len(), 1);

    // Eth from client → reaches bridge tap.
    c.send(Frame::Eth(b"client-to-server".to_vec()))
        .await
        .unwrap();
    let written = tap.pop_written_timeout(SHORT).await.expect("no tap write");
    assert_eq!(written, b"client-to-server");

    // Eth on TAP → echoes back to client over the wire.
    tap.inject_frame(b"server-to-client".to_vec()).await;
    match recv_frame(&mut c).await {
        Frame::Eth(b) => assert_eq!(b, b"server-to-client"),
        other => panic!("expected Eth, got {other:?}"),
    }

    h.hub.shutdown().await;
}

#[tokio::test]
async fn ping_is_answered_inline() {
    let h = spawn_hub(vec![], vec![]).await;
    let mut c = open_conn(&h, "1.2.3.4:4").await;
    c.send(Frame::Hello {
        version: PROTOCOL_VERSION,
        client_uuid: Uuid::new_v4(),
        caps: 0,
    })
    .await
    .unwrap();
    let _ = recv_frame(&mut c).await; // HelloAck

    c.send(Frame::Ping(0xCAFEBABE)).await.unwrap();
    match recv_frame(&mut c).await {
        Frame::Pong(n) => assert_eq!(n, 0xCAFEBABE),
        other => panic!("expected Pong, got {other:?}"),
    }
    h.hub.shutdown().await;
}

#[tokio::test]
async fn file_frame_lands_in_save_dir() {
    let h = spawn_hub(vec![], vec![]).await;
    let mut c = open_conn(&h, "1.2.3.4:5").await;
    c.send(Frame::Hello {
        version: PROTOCOL_VERSION,
        client_uuid: Uuid::new_v4(),
        caps: 0,
    })
    .await
    .unwrap();
    let _ = recv_frame(&mut c).await;

    c.send(Frame::File {
        name: "report.txt".into(),
        data: b"hello world".to_vec(),
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let path = h.save_dir.join("report.txt");
    assert!(path.exists());
    let contents = tokio::fs::read(&path).await.unwrap();
    assert_eq!(contents, b"hello world");

    h.hub.shutdown().await;
}

#[tokio::test]
async fn disconnect_then_reconnect_rebinds_no_new_tap() {
    let net = Uuid::new_v4();
    let client_uuid = Uuid::new_v4();
    let h = spawn_hub(
        vec![(client_uuid, net, "10.0.0.5/24")],
        vec![net],
    )
    .await;

    // First conn: full join.
    let mut c1 = open_conn(&h, "1.2.3.4:6").await;
    c1.send(Frame::Hello {
        version: PROTOCOL_VERSION,
        client_uuid,
        caps: 0,
    })
    .await
    .unwrap();
    let _ = recv_frame(&mut c1).await;
    c1.send(Frame::Join { net_uuid: net }).await.unwrap();
    let _ = recv_frame(&mut c1).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let initial_taps = h.platform.taps_count().await;

    // Drop the first conn.
    drop(c1);
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Second conn: same client_uuid + net → should reuse the session.
    let mut c2 = open_conn(&h, "1.2.3.4:7").await;
    c2.send(Frame::Hello {
        version: PROTOCOL_VERSION,
        client_uuid,
        caps: 0,
    })
    .await
    .unwrap();
    let _ = recv_frame(&mut c2).await;
    c2.send(Frame::Join { net_uuid: net }).await.unwrap();
    match recv_frame(&mut c2).await {
        Frame::JoinOk { .. } => {}
        other => panic!("expected JoinOk, got {other:?}"),
    }

    // No additional TAP must have been spawned.
    assert_eq!(h.platform.taps_count().await, initial_taps);

    h.hub.shutdown().await;
}
