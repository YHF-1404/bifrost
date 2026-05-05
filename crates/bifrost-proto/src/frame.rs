use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::RouteEntry;

/// A single Bifrost protocol frame.
///
/// The wire encoding is whatever postcard produces for this enum. The
/// discriminant is encoded as a single varint byte for variants 0..=127,
/// so each frame on the wire begins with a 4-byte length, then a 1-byte
/// implicit tag, then the variant body.
///
/// All variant fields are owned (no borrows) to keep the `'static`
/// boundary clean — frames cross task boundaries via channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frame {
    // ── Handshake ─────────────────────────────────────────────────────
    /// **C → S, first frame.** Negotiate protocol version and announce
    /// the client's stable identity.
    Hello {
        version: u16,
        client_uuid: Uuid,
        caps: u32,
    },

    /// **S → C, in response to `Hello`.** Confirm the negotiated version.
    HelloAck {
        version: u16,
        server_id: Uuid,
        caps: u32,
    },

    // ── Network membership ────────────────────────────────────────────
    /// **C → S.** Request to join the virtual network identified by `net_uuid`.
    Join { net_uuid: Uuid },

    /// **S → C.** Approval response carrying the TAP suffix the client
    /// should use locally and an optional pre-assigned IP/CIDR.
    JoinOk {
        tap_suffix: String,
        ip: Option<String>,
    },

    /// **S → C.** Rejection with a short, machine-readable reason.
    JoinDeny { reason: String },

    // ── Data plane ────────────────────────────────────────────────────
    /// **Bidirectional.** A raw Ethernet frame.
    ///
    /// `#[serde(with = "serde_bytes")]` is critical for performance:
    /// without it, serde's default `Vec<u8>` impl calls `serialize_u8`
    /// element-by-element, which postcard turns into 1500 individual
    /// `try_push` calls per frame — `perf` showed `FrameCodec::encode`
    /// at ~10 % of cycles on the upload hot path almost entirely from
    /// this. The attribute switches it to `serialize_bytes`, which
    /// postcard handles as one varint-length + a single `try_extend`.
    Eth(#[serde(with = "serde_bytes")] Vec<u8>),

    /// **Bidirectional.** Free-form text broadcast (REPL `send`).
    Text(String),

    /// **Bidirectional.** File payload broadcast (REPL `sendfile`).
    File {
        name: String,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },

    // ── Online configuration push ─────────────────────────────────────
    /// **S → C, Phase 3.** Reassign the client to a different virtual
    /// network (or to no network at all).
    ///
    /// Sent by the server when an admin drags the client onto another
    /// network in the WebUI, or removes it from its current network.
    /// On receipt the client tears down any current TAP/session, sets
    /// its on-disk `joined_network` to the new value, and (if `Some`)
    /// sends a fresh `Join` for the new network.
    ///
    /// `net_uuid = None` means "you are currently unassigned; sit
    /// idle until told otherwise." The client persists this state too,
    /// so a restart doesn't auto-rejoin a stale network.
    AssignNet { net_uuid: Option<Uuid> },

    /// **S → C.** Replace the TAP IP. `None` clears any existing address.
    SetIp { ip: Option<String> },

    /// **S → C.** Replace the client's routing table.
    SetRoutes(Vec<RouteEntry>),

    // ── Liveness ──────────────────────────────────────────────────────
    /// **Bidirectional.** Heartbeat. Echo back via `Pong` with the same nonce.
    Ping(u64),

    /// **Bidirectional.** Reply to a corresponding `Ping`.
    Pong(u64),
}
