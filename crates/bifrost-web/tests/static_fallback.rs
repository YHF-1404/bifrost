//! Tests that the embedded SPA is served correctly:
//!
//! * GET / returns the embedded index.html with no-cache.
//! * Deep links (/networks/:id) fall back to index.html.
//! * /api/* still wins against the fallback.
//!
//! These tests rely on `web/dist/` being populated. In CI the order
//! is:
//!
//! 1. cd web && npm run build
//! 2. cargo test --workspace
//!
//! On a fresh clone with no frontend build the tests pass anyway —
//! `build.rs` writes a placeholder that still satisfies "GET / →
//! 200 with HTML body".

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bifrost_core::config::ServerConfig;
use bifrost_core::Hub;
use bifrost_net::mock::{MockBridge, MockPlatform};
use bifrost_net::{Bridge, Platform};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

async fn spawn() -> SocketAddr {
    let cfg = ServerConfig::default();
    let platform = MockPlatform::new();
    let bridge = MockBridge::new(&cfg.bridge.name);
    let (hub, handle) = Hub::new(
        cfg,
        None,
        platform.clone() as Arc<dyn Platform>,
        bridge.clone() as Arc<dyn Bridge>,
    );
    tokio::spawn(hub.run());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let (st, _rx) = mpsc::channel::<()>(1);
    let state_dir = tempfile::tempdir().unwrap().keep();
    tokio::spawn(async move {
        let _ = bifrost_web::serve(addr, handle, state_dir, st).await;
    });
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    addr
}

#[tokio::test]
async fn root_serves_index_html() {
    let addr = spawn().await;
    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ct.starts_with("text/html"), "got content-type: {ct}");
    let cache = resp
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(cache, "no-cache");
    let body = resp.text().await.unwrap();
    assert!(body.contains("<!doctype html>"), "body was: {body:?}");
}

#[tokio::test]
async fn deep_link_falls_back_to_index() {
    let addr = spawn().await;
    let resp = reqwest::get(format!("http://{addr}/networks/abc-def"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html"),
    );
}

#[tokio::test]
async fn api_routes_win_over_static_fallback() {
    let addr = spawn().await;
    let resp = reqwest::get(format!("http://{addr}/api/networks"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ct.starts_with("application/json"), "got: {ct}");
}
