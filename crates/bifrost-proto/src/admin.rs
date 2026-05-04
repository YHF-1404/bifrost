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
    RenameNet { net_uuid: Uuid, name: String },
    DeleteNet { net_uuid: Uuid },
    /// Mutate one or more fields of a client. `None` on a field means
    /// "leave unchanged"; setting `tap_ip = Some("")` or
    /// `lan_subnets = Some(vec![])` clears the field. A pending
    /// (unassigned) client only accepts `name` and `lan_subnets`.
    DeviceSet {
        client_uuid: Uuid,
        name: Option<String>,
        admitted: Option<bool>,
        tap_ip: Option<String>,
        lan_subnets: Option<Vec<String>>,
    },
    /// **Phase 3.** Assign a client to a network (or detach it).
    /// `net_uuid = None` moves the client to the pending pool.
    /// `Some(nid)` makes the client a (pending, admitted=false) member
    /// of that network — admin then sets `tap_ip` and flips `admitted`.
    AssignClient {
        client_uuid: Uuid,
        net_uuid: Option<Uuid>,
    },
    /// Re-derive routes for a network and push to all currently joined
    /// clients in it.
    DevicePush {
        net_uuid: Uuid,
    },
    /// List devices. `None` = all networks **plus** pending clients;
    /// `Some(uuid)` = filter to a single network.
    DeviceList {
        net_uuid: Option<Uuid>,
    },
    List,
    Send {
        msg: String,
    },
    SendFile {
        name: String,
        data: Vec<u8>,
    },
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
    /// Result of a `DeviceSet` request — full updated record.
    Device(DeviceEntry),
    /// Result of a `DeviceList` request.
    Devices(Vec<DeviceEntry>),
    /// Result of a `DevicePush` request — the routes that were derived
    /// and pushed, plus how many live clients received them.
    Pushed {
        count: u64,
        routes: Vec<RouteRow>,
    },
    NotFound,
    /// `DeviceSet`: bad CIDR/IP given.
    InvalidIp,
    /// `DeviceSet`: tap_ip collides with another device in the same net.
    Conflict {
        msg: String,
    },
    Snapshot(SnapshotData),
    Error(String),
}

/// Wire-friendly mirror of `bifrost_core::HubSnapshot` — kept in proto
/// to avoid an admin-side dep on `bifrost-core`.
///
/// As of v0.1 the snapshot no longer carries a global routes table;
/// derived routes are queryable per-network via `DevicePush`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotData {
    pub networks: Vec<NetEntry>,
    pub sessions: Vec<SessionEntry>,
    pub pending: Vec<PendingEntry>,
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
    pub client_uuid: Uuid,
    pub net_uuid: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRow {
    pub dst: String,
    pub via: String,
}

/// Combined view of a client — both persistent (`approved_clients`
/// row, or `pending_clients` row in Phase 3) and runtime (current
/// session) state. Used by `DeviceList` / `DeviceSet` responses and
/// the WebUI.
///
/// `net_uuid = None` distinguishes a Phase-3 pending (unassigned)
/// client from one that lives in a specific network. Pending entries
/// have `admitted = false`, `tap_ip = None`, and never carry session
/// state (`online` reflects only whether the conn is alive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub client_uuid: Uuid,
    pub net_uuid: Option<Uuid>,
    pub display_name: String,
    pub admitted: bool,
    pub tap_ip: Option<String>,
    pub lan_subnets: Vec<String>,
    /// True iff the hub currently has a live `SessionTask` joined for
    /// `(client_uuid, net_uuid)`, OR (for pending entries) the client
    /// has an open conn awaiting assignment.
    pub online: bool,
    /// Current session id while online, else `None`.
    pub sid: Option<u64>,
    /// Kernel TAP name while online, else `None`.
    pub tap_name: Option<String>,
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
        });
        write_admin(&mut a, &resp).await.unwrap();
        let got: ServerAdminResp = read_admin(&mut b).await.unwrap();
        assert_eq!(got, resp);
    }

    #[tokio::test]
    async fn roundtrip_device_set_request() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let req = ServerAdminReq::DeviceSet {
            client_uuid: Uuid::new_v4(),
            name: Some("router".into()),
            admitted: Some(true),
            tap_ip: Some("10.0.0.5/24".into()),
            lan_subnets: Some(vec!["192.168.10.0/24".into()]),
        };
        write_admin(&mut a, &req).await.unwrap();
        let got: ServerAdminReq = read_admin(&mut b).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn roundtrip_devices_response() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let resp = ServerAdminResp::Devices(vec![DeviceEntry {
            client_uuid: Uuid::new_v4(),
            net_uuid: Some(Uuid::new_v4()),
            display_name: "router".into(),
            admitted: true,
            tap_ip: Some("10.0.0.5/24".into()),
            lan_subnets: vec!["192.168.10.0/24".into()],
            online: true,
            sid: Some(7),
            tap_name: Some("tape251a7ee".into()),
        }]);
        write_admin(&mut a, &resp).await.unwrap();
        let got: ServerAdminResp = read_admin(&mut b).await.unwrap();
        assert_eq!(got, resp);
    }

    #[tokio::test]
    async fn roundtrip_assign_client() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let req = ServerAdminReq::AssignClient {
            client_uuid: Uuid::new_v4(),
            net_uuid: Some(Uuid::new_v4()),
        };
        write_admin(&mut a, &req).await.unwrap();
        let got: ServerAdminReq = read_admin(&mut b).await.unwrap();
        assert_eq!(got, req);
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
