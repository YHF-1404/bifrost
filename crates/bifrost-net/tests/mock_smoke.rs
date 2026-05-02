//! Smoke tests for the mock backend. These guard the test doubles
//! `bifrost-core` builds on, so any regression here surfaces as a clear
//! local failure before propagating.

use std::time::Duration;

use bifrost_net::mock::{MockBridge, MockPlatform, MockTap};
use bifrost_net::{Bridge, ParseError, Platform, RouteEntry, Tap};

#[tokio::test]
async fn route_entry_parses_valid_inputs() {
    let r = RouteEntry::parse("192.168.10.0/24", "10.0.0.1").unwrap();
    assert_eq!(r.dst.to_string(), "192.168.10.0/24");
    assert_eq!(r.via.to_string(), "10.0.0.1");
}

#[tokio::test]
async fn route_entry_rejects_bad_input() {
    assert!(matches!(
        RouteEntry::parse("not a cidr", "10.0.0.1"),
        Err(ParseError::BadCidr(_))
    ));
    assert!(matches!(
        RouteEntry::parse("10.0.0.0/24", "999.999.999.999"),
        Err(ParseError::BadIp(_))
    ));
}

#[tokio::test]
async fn mock_tap_read_write_cycle() {
    let tap = MockTap::new("tap-test");

    // user-space write goes to the to_kernel queue
    tap.write(&[1, 2, 3, 4]).await.unwrap();
    assert_eq!(tap.pop_written().await, Some(vec![1, 2, 3, 4]));
    assert_eq!(tap.pop_written().await, None);

    // kernel-side inject becomes the read result
    tap.inject_frame(vec![9, 8, 7]).await;
    let mut buf = [0u8; 16];
    let n = tap.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], &[9, 8, 7]);
}

#[tokio::test]
async fn mock_tap_set_ip_and_routes_persist() {
    let tap = MockTap::new("tap-test");
    let net: ipnet::IpNet = "10.0.0.5/24".parse().unwrap();
    tap.set_ip(Some(net)).await.unwrap();
    let routes = vec![RouteEntry::parse("192.168.1.0/24", "10.0.0.1").unwrap()];
    tap.apply_routes(&routes).await.unwrap();

    let snap = tap.snapshot().await;
    assert_eq!(snap.ip, Some(net));
    assert_eq!(snap.routes, routes);
    assert!(!snap.destroyed);
}

#[tokio::test]
async fn mock_tap_destroy_blocks_further_writes() {
    let tap = MockTap::new("tap-test");
    tap.destroy().await.unwrap();
    let err = tap.write(&[0; 4]).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    assert!(tap.snapshot().await.destroyed);
}

#[tokio::test]
async fn mock_bridge_add_remove_idempotent() {
    let br = MockBridge::new("br0");
    br.add_tap("tap1").await.unwrap();
    br.add_tap("tap1").await.unwrap(); // duplicate ignored
    br.add_tap("tap2").await.unwrap();
    assert_eq!(br.snapshot().await.ports, vec!["tap1", "tap2"]);

    br.remove_tap("tap1").await.unwrap();
    br.remove_tap("tap1").await.unwrap(); // idempotent
    assert_eq!(br.snapshot().await.ports, vec!["tap2"]);

    br.destroy().await.unwrap();
    assert!(br.snapshot().await.destroyed);
}

#[tokio::test]
async fn mock_platform_creates_and_remembers() {
    let p = MockPlatform::new();
    assert_eq!(p.taps_count().await, 0);
    let _t1 = p.create_tap("tap-a", None).await.unwrap();
    let _t2 = p.create_tap("tap-b", Some("10.0.0.5/24".parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(p.taps_count().await, 2);
    assert_eq!(p.last_tap().await.unwrap().name(), "tap-b");

    let _br = p.create_bridge("br0", None).await.unwrap();
    assert_eq!(p.last_bridge().await.unwrap().name(), "br0");
}

#[tokio::test]
async fn mock_tap_read_waits_for_inject() {
    // A consumer reading before any frame is injected should block; we
    // verify by forcing the read into a timeout.
    let tap = MockTap::new("tap-test");
    let mut buf = [0u8; 16];
    let r = tokio::time::timeout(Duration::from_millis(50), tap.read(&mut buf)).await;
    assert!(r.is_err(), "read should not return until a frame is injected");
}
