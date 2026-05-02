//! Single source of truth for "what each REPL / admin command does".
//!
//! Both the in-process REPL (when `--repl` is on) and the Unix-socket
//! admin server route requests through [`dispatch`] so behavior stays
//! identical between the two paths.

use bifrost_core::{HubHandle, SessionId, SetClientIpResult};
use bifrost_proto::admin::{
    NetEntry, PendingEntry, RouteRow, ServerAdminReq, ServerAdminResp, SessionEntry, SnapshotData,
};

/// Translate one [`ServerAdminReq`] into the matching [`HubHandle`]
/// call(s) and return the wire response.
pub async fn dispatch(hub: &HubHandle, req: ServerAdminReq) -> ServerAdminResp {
    match req {
        ServerAdminReq::MakeNet { name } => match hub.make_net(name).await {
            Some(uuid) => ServerAdminResp::NetCreated { uuid },
            None => ServerAdminResp::Error("hub gone".into()),
        },
        ServerAdminReq::Approve { sid } => {
            if hub.approve(SessionId(sid)).await {
                ServerAdminResp::Ok
            } else {
                ServerAdminResp::NotFound
            }
        }
        ServerAdminReq::Deny { sid } => {
            if hub.deny(SessionId(sid)).await {
                ServerAdminResp::Ok
            } else {
                ServerAdminResp::NotFound
            }
        }
        ServerAdminReq::SetIp { prefix, ip } => match hub.set_client_ip(prefix, ip).await {
            SetClientIpResult::Ok { client_uuid, live } => {
                ServerAdminResp::SetIpOk { client_uuid, live }
            }
            SetClientIpResult::NotFound => ServerAdminResp::NotFound,
            SetClientIpResult::Ambiguous(uuids) => ServerAdminResp::SetIpAmbiguous(uuids),
            SetClientIpResult::InvalidIp => ServerAdminResp::SetIpInvalid,
        },
        ServerAdminReq::RouteAdd { dst, via } => match hub.route_add(dst, via).await {
            Ok(()) => ServerAdminResp::Ok,
            Err(e) => ServerAdminResp::Error(e),
        },
        ServerAdminReq::RouteDel { dst } => {
            if hub.route_del(dst).await {
                ServerAdminResp::Ok
            } else {
                ServerAdminResp::NotFound
            }
        }
        ServerAdminReq::RoutePush => ServerAdminResp::Count(hub.route_push().await as u64),
        ServerAdminReq::List => match hub.list().await {
            Some(snap) => ServerAdminResp::Snapshot(SnapshotData {
                networks: snap
                    .networks
                    .iter()
                    .map(|n| NetEntry {
                        name: n.name.clone(),
                        uuid: n.uuid,
                    })
                    .collect(),
                sessions: snap
                    .sessions
                    .iter()
                    .map(|s| SessionEntry {
                        sid: s.sid.0,
                        client_uuid: s.client_uuid,
                        net_uuid: s.net_uuid,
                        tap_name: s.tap_name.clone(),
                        tap_ip: s.tap_ip.clone(),
                        bound: s.bound_conn.is_some(),
                    })
                    .collect(),
                pending: snap
                    .pending
                    .iter()
                    .map(|p| PendingEntry {
                        sid: p.sid.0,
                        client_uuid: p.client_uuid,
                        net_uuid: p.net_uuid,
                    })
                    .collect(),
                routes: snap
                    .routes
                    .iter()
                    .map(|r| RouteRow {
                        dst: r.dst.clone(),
                        via: r.via.clone(),
                    })
                    .collect(),
            }),
            None => ServerAdminResp::Error("hub gone".into()),
        },
        ServerAdminReq::Send { msg } => ServerAdminResp::Count(hub.broadcast_text(msg).await as u64),
        ServerAdminReq::SendFile { name, data } => {
            ServerAdminResp::Count(hub.broadcast_file(name, data).await as u64)
        }
        ServerAdminReq::Shutdown => {
            hub.shutdown().await;
            ServerAdminResp::Ok
        }
    }
}

/// Render a response as a multi-line block of human-readable text.
pub fn format_resp(resp: &ServerAdminResp) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    match resp {
        ServerAdminResp::Ok => s.push_str("ok"),
        ServerAdminResp::Count(n) => s.push_str(&format!("{n}")),
        ServerAdminResp::NetCreated { uuid } => s.push_str(&format!("network created  uuid={uuid}")),
        ServerAdminResp::SetIpOk { client_uuid, live } => {
            let where_ = if *live { "online — pushed" } else { "offline — saved" };
            s.push_str(&format!("setip {client_uuid} ({where_})"));
        }
        ServerAdminResp::SetIpAmbiguous(uuids) => {
            s.push_str(&format!("ambiguous prefix; {} matches:\n", uuids.len()));
            for u in uuids {
                let _ = writeln!(s, "  {u}");
            }
        }
        ServerAdminResp::SetIpInvalid => s.push_str("invalid ip/cidr"),
        ServerAdminResp::NotFound => s.push_str("not found"),
        ServerAdminResp::Error(e) => s.push_str(&format!("error: {e}")),
        ServerAdminResp::Snapshot(snap) => {
            let _ = writeln!(s, "── networks ──");
            if snap.networks.is_empty() {
                let _ = writeln!(s, "  (none)");
            }
            for n in &snap.networks {
                let _ = writeln!(s, "  {} {}", n.name, n.uuid);
            }
            let _ = writeln!(s, "── sessions ──");
            if snap.sessions.is_empty() {
                let _ = writeln!(s, "  (none)");
            }
            for ss in &snap.sessions {
                let _ = writeln!(
                    s,
                    "  sid={} client={} net={} tap={} ip={} bound={}",
                    ss.sid,
                    short(&ss.client_uuid),
                    short(&ss.net_uuid),
                    ss.tap_name,
                    ss.tap_ip.as_deref().unwrap_or("-"),
                    ss.bound,
                );
            }
            let _ = writeln!(s, "── pending ──");
            if snap.pending.is_empty() {
                let _ = writeln!(s, "  (none)");
            }
            for p in &snap.pending {
                let _ = writeln!(
                    s,
                    "  sid={} client={} net={}",
                    p.sid,
                    short(&p.client_uuid),
                    short(&p.net_uuid),
                );
            }
            let _ = writeln!(s, "── routes ──");
            if snap.routes.is_empty() {
                let _ = writeln!(s, "  (none)");
            }
            for r in &snap.routes {
                let _ = writeln!(s, "  {} via {}", r.dst, r.via);
            }
        }
    }
    s
}

fn short(u: &uuid::Uuid) -> String {
    u.simple().to_string()[..8].to_owned()
}
