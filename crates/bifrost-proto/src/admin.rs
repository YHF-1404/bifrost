//! Admin RPC protocol over a Unix socket.
//!
//! The daemon (server or client) listens on a per-binary `.sock` file
//! and accepts one **request → response → close** exchange per
//! connection. Each direction is one length-prefixed postcard frame:
//!
//! ```text
//! [u32 BE total_len][postcard bytes…]
//! ```
//!
//! Two distinct request/response pairs live here, one per binary. They
//! are *not* compatible with the on-the-wire VPN [`Frame`] — the admin
//! socket is a separate, local-only control channel.
//!
//! [`Frame`]: crate::Frame

use std::io;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

/// Maximum admin frame payload. 8 MiB is plenty for `sendfile` data.
pub const MAX_ADMIN_FRAME: usize = 8 * 1024 * 1024;

// ── Server admin protocol ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerAdminReq {
    MakeNet { name: String },
    Approve { sid: u64 },
    Deny { sid: u64 },
    SetIp { prefix: String, ip: String },
    RouteAdd { dst: String, via: String },
    RouteDel { dst: String },
    RoutePush,
    List,
    Send { msg: String },
    SendFile { name: String, data: Vec<u8> },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerAdminResp {
    Ok,
    /// Generic counted result (e.g. broadcasts reaching N clients).
    Count(u64),
    NetCreated {
        uuid: Uuid,
    },
    SetIpOk {
        client_uuid: Uuid,
        live: bool,
    },
    SetIpAmbiguous(Vec<Uuid>),
    SetIpInvalid,
    NotFound,
    Snapshot(SnapshotData),
    Error(String),
}

/// Wire-friendly mirror of `bifrost_core::HubSnapshot` — kept in proto
/// to avoid an admin-side dep on `bifrost-core`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotData {
    pub networks: Vec<NetEntry>,
    pub sessions: Vec<SessionEntry>,
    pub pending: Vec<PendingEntry>,
    pub routes: Vec<RouteRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetEntry {
    pub name: String,
    pub uuid: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub sid: u64,
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
    pub tap_name: String,
    pub tap_ip: Option<String>,
    pub bound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEntry {
    pub sid: u64,
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRow {
    pub dst: String,
    pub via: String,
}

// ── Client admin protocol ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientAdminReq {
    Join { net_uuid: Uuid },
    Leave,
    Status,
    Send { msg: String },
    SendFile { name: String, data: Vec<u8> },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientAdminResp {
    Ok,
    Status {
        client_uuid: Uuid,
        connected: bool,
        joined_network: Option<Uuid>,
        tap_name: Option<String>,
        tap_ip: Option<String>,
    },
    Error(String),
}

// ── Frame I/O helpers ──────────────────────────────────────────────────────

/// Failure modes for admin-frame I/O.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("frame too large: {0} bytes (max {1})")]
    FrameTooLarge(usize, usize),
}

/// Encode `value` as a single length-prefixed postcard frame and write it.
pub async fn write_admin<W, T>(w: &mut W, value: &T) -> Result<(), AdminError>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let bytes = postcard::to_allocvec(value)?;
    if bytes.len() > MAX_ADMIN_FRAME {
        return Err(AdminError::FrameTooLarge(bytes.len(), MAX_ADMIN_FRAME));
    }
    let len = bytes.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&bytes).await?;
    Ok(())
}

/// Read one length-prefixed postcard frame and decode it as `T`.
pub async fn read_admin<R, T>(r: &mut R) -> Result<T, AdminError>
where
    R: AsyncReadExt + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_ADMIN_FRAME {
        return Err(AdminError::FrameTooLarge(len, MAX_ADMIN_FRAME));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_server_request() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let req = ServerAdminReq::MakeNet {
            name: "hml-net".into(),
        };
        write_admin(&mut a, &req).await.unwrap();
        let got: ServerAdminReq = read_admin(&mut b).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn roundtrip_server_response_snapshot() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let resp = ServerAdminResp::Snapshot(SnapshotData {
            networks: vec![NetEntry {
                name: "n".into(),
                uuid: Uuid::new_v4(),
            }],
            sessions: vec![],
            pending: vec![],
            routes: vec![RouteRow {
                dst: "10.0.0.0/24".into(),
                via: "10.0.0.1".into(),
            }],
        });
        write_admin(&mut a, &resp).await.unwrap();
        let got: ServerAdminResp = read_admin(&mut b).await.unwrap();
        assert_eq!(got, resp);
    }

    #[tokio::test]
    async fn roundtrip_client_request() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let req = ClientAdminReq::Join {
            net_uuid: Uuid::new_v4(),
        };
        write_admin(&mut a, &req).await.unwrap();
        let got: ClientAdminReq = read_admin(&mut b).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn rejects_oversized_payload_on_encode() {
        let (mut a, _b) = tokio::io::duplex(1024);
        // 9 MiB string blows past MAX_ADMIN_FRAME (8 MiB).
        let big = "x".repeat(9 * 1024 * 1024);
        let resp = ServerAdminResp::Error(big);
        let err = write_admin(&mut a, &resp).await.unwrap_err();
        assert!(matches!(err, AdminError::FrameTooLarge(_, _)));
    }
}
