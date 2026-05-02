//! End-to-end tests for the client admin Unix socket.
//!
//! Spawns a real `App` plus the admin socket, then drives commands the
//! way `bifrost-client admin <cmd>` would over the wire.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bifrost_client::admin;
use bifrost_client::app::{App, AppPorts};
use bifrost_client::conn::ConnEvent;
use bifrost_client::repl::UserCmd;
use bifrost_core::config::ClientConfig;
use bifrost_net::mock::MockPlatform;
use bifrost_net::Platform;
use bifrost_proto::admin::{ClientAdminReq, ClientAdminResp};
use bifrost_proto::{Frame, PROTOCOL_VERSION};
use tempfile::TempDir;
use tokio::sync::mpsc;
use uuid::Uuid;

struct Harness {
    socket: PathBuf,
    out_rx: mpsc::Receiver<Frame>,
    events_tx: mpsc::Sender<ConnEvent>,
    user_tx: mpsc::Sender<UserCmd>,
    #[allow(dead_code)]
    platform: Arc<MockPlatform>,
    #[allow(dead_code)]
    shutdown_rx: mpsc::Receiver<()>,
    #[allow(dead_code)]
    app_join: tokio::task::JoinHandle<anyhow::Result<()>>,
    #[allow(dead_code)]
    _tmp: TempDir,
}

async fn spawn() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("client.toml");
    let socket = tmp.path().join("admin.sock");

    let mut cfg = ClientConfig::default();
    cfg.client.uuid = Uuid::new_v4().to_string();
    cfg.client.save_dir = tmp.path().join("recv").to_string_lossy().into_owned();
    cfg.admin.socket = socket.to_string_lossy().into_owned();
    cfg.save(&cfg_path).await.unwrap();

    let platform = MockPlatform::new();
    let (out_tx, out_rx) = mpsc::channel(64);
    let (events_tx, events_rx) = mpsc::channel(64);
    let (user_tx, user_rx) = mpsc::channel(64);

    let app = App::new(AppPorts {
        cfg,
        cfg_path,
        platform: platform.clone() as Arc<dyn Platform>,
        out_tx,
        events_rx,
        user_rx,
    });
    let app_join = tokio::spawn(app.run());

    let (shutdown_tx, shutdown_rx) = mpsc::channel(2);
    let socket_clone = socket.clone();
    let admin_user_tx = user_tx.clone();
    tokio::spawn(async move {
        let _ = admin::serve(socket_clone, admin_user_tx, shutdown_tx).await;
    });
    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    Harness {
        socket,
        out_rx,
        events_tx,
        user_tx,
        platform,
        shutdown_rx,
        app_join,
        _tmp: tmp,
    }
}

async fn rpc(socket: &std::path::Path, req: ClientAdminReq) -> ClientAdminResp {
    admin::round_trip(socket, req).await.expect("admin RPC")
}

#[tokio::test]
async fn join_via_admin_socket_emits_join_frame() {
    let mut h = spawn().await;
    let net = Uuid::new_v4();

    // Bring the App into the "hello-acked" state so it actually emits Join.
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    let _ = h.out_rx.recv().await; // Hello
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    let resp = rpc(&h.socket, ClientAdminReq::Join { net_uuid: net }).await;
    assert!(matches!(resp, ClientAdminResp::Ok));

    match tokio::time::timeout(Duration::from_millis(300), h.out_rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Frame::Join { net_uuid } => assert_eq!(net_uuid, net),
        other => panic!("expected Join, got {other:?}"),
    }

    // Quit cleanly so the app task exits.
    h.user_tx.send(UserCmd::Quit).await.unwrap();
}

#[tokio::test]
async fn status_returns_current_state() {
    let mut h = spawn().await;
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    let _ = h.out_rx.recv().await;
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    // Allow the App to process HelloAck.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let resp = rpc(&h.socket, ClientAdminReq::Status).await;
    match resp {
        ClientAdminResp::Status {
            connected,
            joined_network,
            tap_name,
            ..
        } => {
            assert!(connected);
            assert!(joined_network.is_none());
            assert!(tap_name.is_none());
        }
        other => panic!("expected Status, got {other:?}"),
    }
    h.user_tx.send(UserCmd::Quit).await.unwrap();
}

#[tokio::test]
async fn send_via_admin_emits_text_frame() {
    let mut h = spawn().await;
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    let _ = h.out_rx.recv().await;
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    let resp = rpc(
        &h.socket,
        ClientAdminReq::Send {
            msg: "ping from admin".into(),
        },
    )
    .await;
    assert!(matches!(resp, ClientAdminResp::Ok));

    match tokio::time::timeout(Duration::from_millis(300), h.out_rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Frame::Text(s) => assert_eq!(s, "ping from admin"),
        other => panic!("expected Text, got {other:?}"),
    }
    h.user_tx.send(UserCmd::Quit).await.unwrap();
}

#[tokio::test]
async fn sendfile_via_admin_emits_file_frame() {
    let mut h = spawn().await;
    h.events_tx.send(ConnEvent::Connected).await.unwrap();
    let _ = h.out_rx.recv().await;
    h.events_tx
        .send(ConnEvent::FrameIn(Frame::HelloAck {
            version: PROTOCOL_VERSION,
            server_id: Uuid::new_v4(),
            caps: 0,
        }))
        .await
        .unwrap();

    let resp = rpc(
        &h.socket,
        ClientAdminReq::SendFile {
            name: "report.bin".into(),
            data: vec![1, 2, 3, 4],
        },
    )
    .await;
    assert!(matches!(resp, ClientAdminResp::Ok));

    match tokio::time::timeout(Duration::from_millis(300), h.out_rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Frame::File { name, data } => {
            assert_eq!(name, "report.bin");
            assert_eq!(data, vec![1, 2, 3, 4]);
        }
        other => panic!("expected File, got {other:?}"),
    }
    h.user_tx.send(UserCmd::Quit).await.unwrap();
}

#[tokio::test]
async fn shutdown_via_admin_signals_main() {
    let mut h = spawn().await;
    let resp = rpc(&h.socket, ClientAdminReq::Shutdown).await;
    assert!(matches!(resp, ClientAdminResp::Ok));

    let signal = tokio::time::timeout(Duration::from_millis(500), h.shutdown_rx.recv()).await;
    assert!(signal.is_ok(), "shutdown_rx must receive signal");
}
