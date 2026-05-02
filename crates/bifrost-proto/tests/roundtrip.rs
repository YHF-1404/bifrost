//! Integration tests for the on-wire protocol: every variant must
//! round-trip through `FrameCodec`, and the codec must behave correctly
//! across multi-frame buffers, partial reads, and a real `Framed` stream.

use bifrost_proto::{caps, Frame, FrameCodec, ProtoError, RouteEntry, MAX_FRAME_LEN, PROTOCOL_VERSION};
use bytes::{BufMut, BytesMut};
use futures::{SinkExt, StreamExt};
use tokio_util::codec::{Decoder, Encoder, Framed};
use uuid::Uuid;

/// Encode `frame`, decode it, assert equality and that no bytes remain.
fn roundtrip(frame: Frame) {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::new();
    codec.encode(frame.clone(), &mut buf).unwrap();
    let decoded = codec
        .decode(&mut buf)
        .unwrap()
        .expect("decoder produced no frame");
    assert_eq!(frame, decoded);
    assert!(buf.is_empty(), "trailing bytes left in buffer");
}

// ── Per-variant round-trips ───────────────────────────────────────────

#[test]
fn hello_roundtrip() {
    roundtrip(Frame::Hello {
        version: PROTOCOL_VERSION,
        client_uuid: Uuid::new_v4(),
        caps: 0,
    });
}

#[test]
fn hello_ack_roundtrip() {
    roundtrip(Frame::HelloAck {
        version: PROTOCOL_VERSION,
        server_id: Uuid::new_v4(),
        caps: caps::PCAP_STREAM,
    });
}

#[test]
fn join_roundtrip() {
    roundtrip(Frame::Join {
        net_uuid: Uuid::new_v4(),
    });
}

#[test]
fn join_ok_with_ip() {
    roundtrip(Frame::JoinOk {
        tap_suffix: "abc12345".into(),
        ip: Some("10.0.0.2/24".into()),
    });
}

#[test]
fn join_ok_without_ip() {
    roundtrip(Frame::JoinOk {
        tap_suffix: "abc12345".into(),
        ip: None,
    });
}

#[test]
fn join_deny_roundtrip() {
    roundtrip(Frame::JoinDeny {
        reason: "denied_by_admin".into(),
    });
}

#[test]
fn eth_typical_frame_roundtrip() {
    let frame: Vec<u8> = (0..1500).map(|i| (i % 256) as u8).collect();
    roundtrip(Frame::Eth(frame));
}

#[test]
fn eth_empty_frame_roundtrip() {
    roundtrip(Frame::Eth(Vec::new()));
}

#[test]
fn text_roundtrip() {
    roundtrip(Frame::Text("hello, 世界 🌉".into()));
}

#[test]
fn file_roundtrip() {
    roundtrip(Frame::File {
        name: "report.pdf".into(),
        data: vec![1, 2, 3, 4, 5],
    });
}

#[test]
fn set_ip_some_and_none() {
    roundtrip(Frame::SetIp {
        ip: Some("10.0.0.5/24".into()),
    });
    roundtrip(Frame::SetIp { ip: None });
}

#[test]
fn set_routes_roundtrip() {
    roundtrip(Frame::SetRoutes(vec![
        RouteEntry {
            dst: "192.168.10.0/24".into(),
            via: "10.0.0.1".into(),
        },
        RouteEntry {
            dst: "192.168.20.0/24".into(),
            via: "10.0.0.2".into(),
        },
    ]));
}

#[test]
fn empty_routes_roundtrip() {
    roundtrip(Frame::SetRoutes(Vec::new()));
}

#[test]
fn ping_pong_roundtrip() {
    roundtrip(Frame::Ping(0xDEAD_BEEF));
    roundtrip(Frame::Pong(0xDEAD_BEEF));
}

// ── Streaming behaviour ───────────────────────────────────────────────

#[test]
fn partial_frame_returns_none_until_complete() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::new();

    // Encode a frame, then feed it one byte at a time and observe that
    // the decoder politely waits for more bytes.
    let mut encoded = BytesMut::new();
    codec
        .encode(Frame::Text("partial".into()), &mut encoded)
        .unwrap();

    for byte in &encoded[..encoded.len() - 1] {
        buf.put_u8(*byte);
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }
    // Last byte completes the frame.
    buf.put_u8(*encoded.last().unwrap());
    let frame = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(frame, Frame::Text("partial".into()));
}

#[test]
fn multiple_frames_in_one_buffer() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::new();
    let frames = [Frame::Ping(1), Frame::Ping(2), Frame::Ping(3)];
    for f in &frames {
        codec.encode(f.clone(), &mut buf).unwrap();
    }
    for f in &frames {
        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), *f);
    }
    assert!(codec.decode(&mut buf).unwrap().is_none());
}

#[test]
fn declared_length_above_max_is_rejected() {
    let mut codec = FrameCodec::with_max_len(64);
    let mut buf = BytesMut::new();
    buf.put_u32(200);
    let err = codec.decode(&mut buf).unwrap_err();
    assert!(matches!(err, ProtoError::FrameTooLarge(200, 64)));
}

#[test]
fn encoding_payload_above_max_is_rejected() {
    // Force a tiny limit so even a small Eth frame overflows it.
    let mut codec = FrameCodec::with_max_len(8);
    let mut buf = BytesMut::new();
    let err = codec
        .encode(Frame::Eth(vec![0; 1500]), &mut buf)
        .unwrap_err();
    assert!(matches!(err, ProtoError::FrameTooLarge(_, 8)));
    assert!(buf.is_empty(), "no bytes should be written on rejection");
}

#[test]
fn malformed_payload_returns_postcard_error() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::new();
    // Declare 4 bytes of payload but provide complete junk that postcard
    // cannot interpret as any Frame variant.
    buf.put_u32(4);
    buf.put_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let err = codec.decode(&mut buf).unwrap_err();
    assert!(matches!(err, ProtoError::Postcard(_)));
}

// Sanity guard so an accidental `* * 1024` change doesn't slip past review.
// Evaluated at compile time so a regression breaks the build, not a test run.
const _: () = assert!(MAX_FRAME_LEN >= 64 * 1024);
const _: () = assert!(MAX_FRAME_LEN <= 1024 * 1024);

// ── End-to-end: a real Framed adapter over an in-memory pipe ──────────

#[tokio::test]
async fn framed_roundtrip_over_in_memory_pipe() {
    let (a, b) = tokio::io::duplex(64 * 1024);
    let mut tx = Framed::new(a, FrameCodec::new());
    let mut rx = Framed::new(b, FrameCodec::new());

    let outgoing = vec![
        Frame::Hello {
            version: PROTOCOL_VERSION,
            client_uuid: Uuid::new_v4(),
            caps: 0,
        },
        Frame::Eth(vec![0xAA; 1500]),
        Frame::SetRoutes(vec![RouteEntry {
            dst: "192.168.1.0/24".into(),
            via: "10.0.0.1".into(),
        }]),
        Frame::Ping(42),
    ];

    for f in &outgoing {
        tx.send(f.clone()).await.unwrap();
    }
    drop(tx); // half-close; rx should read EOF after consuming the frames

    let mut received = Vec::new();
    while let Some(item) = rx.next().await {
        received.push(item.unwrap());
    }
    assert_eq!(received, outgoing);
}
