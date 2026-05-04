//! Bifrost VPN protocol — version 1.
//!
//! # Wire format
//!
//! ```text
//! ┌────────────────────────┬──────────────────────────────────────┐
//! │  u32 BE  payload_len   │  postcard-encoded `Frame`            │
//! └────────────────────────┴──────────────────────────────────────┘
//! ```
//!
//! The 4-byte length prefix counts the postcard payload only (it does not
//! include itself). Frames whose declared length exceeds
//! [`MAX_FRAME_LEN`] are rejected by [`FrameCodec`].
//!
//! # Handshake
//!
//! Immediately after the TCP / SOCKS5 connection succeeds, the **client**
//! sends [`Frame::Hello`]; the **server** replies with [`Frame::HelloAck`].
//! If `version` does not match on either side, the peer closes the
//! connection without a body. After a successful HelloAck, the client may
//! send [`Frame::Join`].
//!
//! # Capabilities
//!
//! The `caps` bitmask in `Hello`/`HelloAck` is reserved for future
//! transport-layer upgrades (Noise, TLS, pcap streaming). v1 senders MUST
//! emit `caps = 0`; v1 receivers MUST ignore unknown bits. See
//! [`caps`](crate::types::caps).

#![forbid(unsafe_code)]

pub mod admin;
pub mod codec;
pub mod error;
pub mod frame;
pub mod types;

pub use codec::FrameCodec;
pub use error::ProtoError;
pub use frame::Frame;
pub use types::{caps, RouteEntry};

/// Current protocol version. Bumped on any wire-incompatible change.
///
/// * v1 — initial.
/// * v2 — added [`Frame::AssignNet`] for server-driven network
///   assignment (Phase 3 unified WebUI). Old clients get a
///   `JoinDeny { reason: "version_mismatch:..." }` on Hello.
pub const PROTOCOL_VERSION: u16 = 2;

/// Maximum encoded payload size in bytes (postcard output, header excluded).
///
/// 65 KiB comfortably accommodates a max-jumbo Ethernet frame plus
/// per-frame metadata, while keeping a single allocation small.
pub const MAX_FRAME_LEN: usize = 65 * 1024;
