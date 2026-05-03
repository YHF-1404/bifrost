//! Optional in-process REPL (`--repl`).
//!
//! Parses each line into a [`ServerAdminReq`], hands it to the async
//! pump in `main.rs` along with a oneshot for the rendered response,
//! and prints whatever comes back. This mirrors what `bifrost-server
//! admin <cmd>` does over the Unix socket — single source of truth in
//! [`crate::dispatch::dispatch`].

use std::path::PathBuf;

use bifrost_proto::admin::ServerAdminReq;
use rustyline::error::ReadlineError;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Lightweight ParseResult — kept for tests that exercise the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplCmd {
    Req(ServerAdminReq),
    Quit,
}

/// Run the REPL synchronously on the calling (blocking) thread.
///
/// Drops `tx` on exit so the async pump in `main.rs` knows to break.
pub fn run_blocking(tx: mpsc::Sender<(ServerAdminReq, oneshot::Sender<String>)>) {
    let mut rl: rustyline::Editor<(), _> = match rustyline::Editor::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[!] readline init failed: {e}");
            return;
        }
    };

    println!(
        "REPL — same commands as `bifrost-server admin <cmd>`:\n  \
         mknet <name> |\n  \
         device list [<net-uuid>] |\n  \
         device set <client-uuid> [name=X] [ip=Y/CIDR] [admit=true|false] [lan=A,B,...] |\n  \
         device push <net-uuid> |\n  \
         list | send <msg> | sendfile <path> | shutdown | quit"
    );

    loop {
        match rl.readline("> ") {
            Ok(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&line);
                match parse(&line) {
                    Ok(ReplCmd::Quit) => return,
                    Ok(ReplCmd::Req(req)) => {
                        let (ack_tx, ack_rx) = oneshot::channel();
                        if tx.blocking_send((req, ack_tx)).is_err() {
                            return;
                        }
                        match ack_rx.blocking_recv() {
                            Ok(s) => println!("{s}"),
                            Err(_) => return,
                        }
                    }
                    Err(e) => println!("[!] {e}"),
                }
            }
            Err(ReadlineError::Eof | ReadlineError::Interrupted) => return,
            Err(e) => {
                eprintln!("[!] readline error: {e}");
                return;
            }
        }
    }
}

/// Parse one command line into either a [`ServerAdminReq`] or `Quit`.
pub fn parse(line: &str) -> Result<ReplCmd, String> {
    let mut parts = line.splitn(2, ' ');
    let head = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    let req = match head {
        "mknet" => {
            if rest.is_empty() {
                return Err("usage: mknet <name>".into());
            }
            ServerAdminReq::MakeNet {
                name: rest.to_string(),
            }
        }
        "device" => parse_device(rest)?,
        "list" => ServerAdminReq::List,
        "send" => {
            if rest.is_empty() {
                return Err("usage: send <msg>".into());
            }
            ServerAdminReq::Send {
                msg: rest.to_string(),
            }
        }
        "sendfile" => {
            if rest.is_empty() {
                return Err("usage: sendfile <path>".into());
            }
            let path = PathBuf::from(rest);
            let data = std::fs::read(&path).map_err(|e| format!("read {path:?}: {e}"))?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            ServerAdminReq::SendFile { name, data }
        }
        "shutdown" => ServerAdminReq::Shutdown,
        "quit" | "exit" => return Ok(ReplCmd::Quit),
        "" => return Err("empty command".into()),
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(ReplCmd::Req(req))
}

/// `device list [<net-uuid>]`
/// `device push <net-uuid>`
/// `device set <client-uuid> [name=X] [ip=Y] [admit=true|false] [lan=A,B,...]`
fn parse_device(rest: &str) -> Result<ServerAdminReq, String> {
    let mut it = rest.splitn(2, ' ');
    let sub = it.next().unwrap_or("").trim();
    let tail = it.next().unwrap_or("").trim();
    match sub {
        "list" => {
            let net_uuid = if tail.is_empty() {
                None
            } else {
                Some(Uuid::parse_str(tail).map_err(|e| format!("bad net uuid: {e}"))?)
            };
            Ok(ServerAdminReq::DeviceList { net_uuid })
        }
        "push" => {
            if tail.is_empty() {
                return Err("usage: device push <net-uuid>".into());
            }
            let net_uuid = Uuid::parse_str(tail).map_err(|e| format!("bad net uuid: {e}"))?;
            Ok(ServerAdminReq::DevicePush { net_uuid })
        }
        "set" => parse_device_set(tail),
        _ => Err("usage: device list|set|push".into()),
    }
}

fn parse_device_set(tail: &str) -> Result<ServerAdminReq, String> {
    let mut it = tail.split_whitespace();
    let client = it
        .next()
        .ok_or("usage: device set <client-uuid> [name=X] [ip=Y] [admit=true|false] [lan=A,B,...]")?;
    let client_uuid =
        Uuid::parse_str(client).map_err(|e| format!("bad client uuid: {e}"))?;

    let mut name: Option<String> = None;
    let mut admitted: Option<bool> = None;
    let mut tap_ip: Option<String> = None;
    let mut lan_subnets: Option<Vec<String>> = None;
    for kv in it {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| format!("bad pair {kv:?}, expected key=value"))?;
        match k {
            "name" => name = Some(v.to_string()),
            "ip" => tap_ip = Some(v.to_string()),
            "admit" => {
                admitted = Some(match v {
                    "true" | "1" | "yes" => true,
                    "false" | "0" | "no" => false,
                    _ => return Err(format!("bad admit value {v:?}")),
                });
            }
            "lan" => {
                let list: Vec<String> = if v.is_empty() {
                    Vec::new()
                } else {
                    v.split(',').map(|s| s.trim().to_string()).collect()
                };
                lan_subnets = Some(list);
            }
            other => return Err(format!("unknown key {other:?}")),
        }
    }

    Ok(ServerAdminReq::DeviceSet {
        client_uuid,
        name,
        admitted,
        tap_ip,
        lan_subnets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_commands() {
        assert_eq!(
            parse("mknet hml").unwrap(),
            ReplCmd::Req(ServerAdminReq::MakeNet { name: "hml".into() })
        );
        assert_eq!(parse("list").unwrap(), ReplCmd::Req(ServerAdminReq::List));
        assert_eq!(
            parse("send hi there").unwrap(),
            ReplCmd::Req(ServerAdminReq::Send { msg: "hi there".into() })
        );
        assert_eq!(parse("quit").unwrap(), ReplCmd::Quit);
        assert_eq!(parse("exit").unwrap(), ReplCmd::Quit);
        assert_eq!(
            parse("shutdown").unwrap(),
            ReplCmd::Req(ServerAdminReq::Shutdown)
        );
    }

    #[test]
    fn parses_device_list() {
        assert_eq!(
            parse("device list").unwrap(),
            ReplCmd::Req(ServerAdminReq::DeviceList { net_uuid: None })
        );
        let nid = Uuid::new_v4();
        assert_eq!(
            parse(&format!("device list {nid}")).unwrap(),
            ReplCmd::Req(ServerAdminReq::DeviceList {
                net_uuid: Some(nid)
            })
        );
    }

    #[test]
    fn parses_device_push() {
        let nid = Uuid::new_v4();
        assert_eq!(
            parse(&format!("device push {nid}")).unwrap(),
            ReplCmd::Req(ServerAdminReq::DevicePush { net_uuid: nid })
        );
    }

    #[test]
    fn parses_device_set_full() {
        let cid = Uuid::new_v4();
        let line = format!(
            "device set {cid} name=router ip=10.0.0.5/24 admit=true lan=192.168.10.0/24,192.168.20.0/24"
        );
        let parsed = parse(&line).unwrap();
        assert_eq!(
            parsed,
            ReplCmd::Req(ServerAdminReq::DeviceSet {
                client_uuid: cid,
                name: Some("router".into()),
                admitted: Some(true),
                tap_ip: Some("10.0.0.5/24".into()),
                lan_subnets: Some(vec![
                    "192.168.10.0/24".into(),
                    "192.168.20.0/24".into()
                ]),
            })
        );
    }

    #[test]
    fn parses_device_set_partial_clears() {
        let cid = Uuid::new_v4();
        let parsed = parse(&format!("device set {cid} ip= lan=")).unwrap();
        assert_eq!(
            parsed,
            ReplCmd::Req(ServerAdminReq::DeviceSet {
                client_uuid: cid,
                name: None,
                admitted: None,
                tap_ip: Some(String::new()),
                lan_subnets: Some(Vec::new()),
            })
        );
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(parse("garbage"), Err("unknown command \"garbage\"".into()));
        assert!(parse("approve 7").is_err()); // command removed
        assert!(parse("device").is_err());
        assert!(parse("device push not-a-uuid").is_err());
        assert!(parse("device set not-a-uuid").is_err());
        assert!(parse("mknet").is_err());
    }
}
