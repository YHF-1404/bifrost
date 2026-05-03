//! Single source of truth for "what each REPL / admin command does".
//!
//! Both the in-process REPL (when `--repl` is on) and the Unix-socket
//! admin server route requests through [`dispatch`] so behavior stays
//! identical between the two paths.

use bifrost_core::{DevicePushResult, DeviceSetResult, DeviceUpdate, HubHandle, SessionId};
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
        ServerAdminReq::DeviceSet {
            client_uuid,
            name,
            admitted,
            tap_ip,
            lan_subnets,
        } => {
            // The CLI/HTTP caller must give us a (client, net) pair.
            // For convenience, if no `net_uuid` was provided we look
            // for a single matching approved-clients row.
            let net_uuid = match resolve_single_net(hub, client_uuid).await {
                Ok(n) => n,
                Err(resp) => return resp,
            };
            let update = DeviceUpdate {
                name,
                admitted,
                tap_ip,
                lan_subnets,
            };
            match hub.device_set(client_uuid, net_uuid, update).await {
                DeviceSetResult::Ok(d) => ServerAdminResp::Device(d),
                DeviceSetResult::NotFound => ServerAdminResp::NotFound,
                DeviceSetResult::InvalidIp => ServerAdminResp::InvalidIp,
                DeviceSetResult::Conflict { msg } => ServerAdminResp::Conflict { msg },
            }
        }
        ServerAdminReq::DevicePush { net_uuid } => {
            let DevicePushResult { routes, count } = hub.device_push(net_uuid).await;
            ServerAdminResp::Pushed {
                count,
                routes: routes
                    .into_iter()
                    .map(|r| RouteRow {
                        dst: r.dst,
                        via: r.via,
                    })
                    .collect(),
            }
        }
        ServerAdminReq::DeviceList { net_uuid } => {
            ServerAdminResp::Devices(hub.device_list(net_uuid).await)
        }
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

/// CLI ergonomics: when a caller supplies just a `client_uuid`, find the
/// unique `net_uuid` that pairs with it. Returns Err(Resp) on the
/// 0-match or >1-match cases so the caller can short-circuit.
async fn resolve_single_net(
    hub: &HubHandle,
    client_uuid: uuid::Uuid,
) -> Result<uuid::Uuid, ServerAdminResp> {
    let devices = hub.device_list(None).await;
    let candidates: Vec<_> = devices
        .iter()
        .filter(|d| d.client_uuid == client_uuid)
        .collect();
    match candidates.len() {
        0 => Err(ServerAdminResp::NotFound),
        1 => Ok(candidates[0].net_uuid),
        _ => Err(ServerAdminResp::Error(format!(
            "client {} appears in {} networks; specify net_uuid",
            client_uuid,
            candidates.len()
        ))),
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
        ServerAdminResp::Device(d) => {
            let _ = write!(
                s,
                "device {client} net={net} name={name:?} admitted={admit} ip={ip} \
                 lan={lan} online={on}",
                client = short(&d.client_uuid),
                net = short(&d.net_uuid),
                name = d.display_name,
                admit = d.admitted,
                ip = d.tap_ip.as_deref().unwrap_or("-"),
                lan = if d.lan_subnets.is_empty() {
                    "-".to_string()
                } else {
                    d.lan_subnets.join(",")
                },
                on = d.online,
            );
        }
        ServerAdminResp::Devices(list) => {
            let _ = writeln!(s, "── devices ──");
            if list.is_empty() {
                let _ = writeln!(s, "  (none)");
            }
            for d in list {
                let _ = writeln!(
                    s,
                    "  client={} net={} name={:?} admitted={} ip={} lan={} online={}",
                    short(&d.client_uuid),
                    short(&d.net_uuid),
                    d.display_name,
                    d.admitted,
                    d.tap_ip.as_deref().unwrap_or("-"),
                    if d.lan_subnets.is_empty() {
                        "-".to_string()
                    } else {
                        d.lan_subnets.join(",")
                    },
                    d.online,
                );
            }
        }
        ServerAdminResp::Pushed { count, routes } => {
            let _ = writeln!(s, "pushed to {count} client(s); {} route(s):", routes.len());
            for r in routes {
                let _ = writeln!(s, "  {} via {}", r.dst, r.via);
            }
        }
        ServerAdminResp::NotFound => s.push_str("not found"),
        ServerAdminResp::InvalidIp => s.push_str("invalid ip/cidr"),
        ServerAdminResp::Conflict { msg } => s.push_str(&format!("conflict: {msg}")),
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
        }
    }
    s
}

fn short(u: &uuid::Uuid) -> String {
    u.simple().to_string()[..8].to_owned()
}
