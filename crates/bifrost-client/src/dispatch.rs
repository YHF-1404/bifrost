//! Single source of truth for "what each REPL / admin command does".

use std::time::Duration;

use bifrost_proto::admin::{ClientAdminReq, ClientAdminResp};
use tokio::sync::{mpsc, oneshot};

use crate::repl::UserCmd;

/// Translate one [`ClientAdminReq`] into a fire-and-forget
/// [`UserCmd`] (or a `Status` query that needs a reply) and return the
/// matching [`ClientAdminResp`].
pub async fn dispatch(
    user_tx: &mpsc::Sender<UserCmd>,
    req: ClientAdminReq,
) -> ClientAdminResp {
    match req {
        ClientAdminReq::Join { net_uuid } => match user_tx.send(UserCmd::Join(net_uuid)).await {
            Ok(()) => ClientAdminResp::Ok,
            Err(_) => ClientAdminResp::Error("app gone".into()),
        },
        ClientAdminReq::Leave => match user_tx.send(UserCmd::Leave).await {
            Ok(()) => ClientAdminResp::Ok,
            Err(_) => ClientAdminResp::Error("app gone".into()),
        },
        ClientAdminReq::Send { msg } => match user_tx.send(UserCmd::SendText(msg)).await {
            Ok(()) => ClientAdminResp::Ok,
            Err(_) => ClientAdminResp::Error("app gone".into()),
        },
        ClientAdminReq::SendFile { name, data } => {
            match user_tx.send(UserCmd::SendFile { name, data }).await {
                Ok(()) => ClientAdminResp::Ok,
                Err(_) => ClientAdminResp::Error("app gone".into()),
            }
        }
        ClientAdminReq::Status => {
            let (tx, rx) = oneshot::channel();
            if user_tx.send(UserCmd::Status(tx)).await.is_err() {
                return ClientAdminResp::Error("app gone".into());
            }
            match tokio::time::timeout(Duration::from_secs(2), rx).await {
                Ok(Ok(snap)) => ClientAdminResp::Status {
                    client_uuid: snap.client_uuid,
                    connected: snap.connected,
                    joined_network: snap.joined_network,
                    tap_name: snap.tap_name,
                    tap_ip: snap.tap_ip,
                },
                _ => ClientAdminResp::Error("status query timed out".into()),
            }
        }
        ClientAdminReq::Shutdown => match user_tx.send(UserCmd::Quit).await {
            Ok(()) => ClientAdminResp::Ok,
            Err(_) => ClientAdminResp::Error("app gone".into()),
        },
    }
}

/// Render an admin response as user-facing text. Used by both the
/// `admin` subcommand and the daemon's logs.
pub fn format_resp(resp: &ClientAdminResp) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    match resp {
        ClientAdminResp::Ok => s.push_str("ok"),
        ClientAdminResp::Error(e) => s.push_str(&format!("error: {e}")),
        ClientAdminResp::Status {
            client_uuid,
            connected,
            joined_network,
            tap_name,
            tap_ip,
        } => {
            let _ = writeln!(s, "client_uuid:    {client_uuid}");
            let _ = writeln!(s, "connected:      {connected}");
            let _ = writeln!(
                s,
                "joined_network: {}",
                joined_network
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "(none)".into())
            );
            let _ = writeln!(s, "tap_name:       {}", tap_name.as_deref().unwrap_or("-"));
            let _ = write!(s, "tap_ip:         {}", tap_ip.as_deref().unwrap_or("-"));
        }
    }
    s
}
