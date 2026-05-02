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
         mknet <name> | approve <sid> | deny <sid> | setip <prefix> <ip> |\n  \
         route add <dst> via <gw> | route del <dst> | route list | route push |\n  \
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
        "approve" => ServerAdminReq::Approve {
            sid: parse_u64(rest)?,
        },
        "deny" => ServerAdminReq::Deny {
            sid: parse_u64(rest)?,
        },
        "setip" => {
            let mut it = rest.split_whitespace();
            let prefix = it
                .next()
                .ok_or("usage: setip <prefix> <ip>")?
                .to_string();
            let ip = it.next().unwrap_or("").to_string();
            ServerAdminReq::SetIp { prefix, ip }
        }
        "route" => parse_route(rest)?,
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

fn parse_u64(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("expected integer, got {s:?}"))
}

fn parse_route(rest: &str) -> Result<ServerAdminReq, String> {
    let mut it = rest.splitn(2, ' ');
    let sub = it.next().unwrap_or("").trim();
    let tail = it.next().unwrap_or("").trim();
    match sub {
        "list" => Ok(ServerAdminReq::List),
        "push" => Ok(ServerAdminReq::RoutePush),
        "del" => {
            if tail.is_empty() {
                Err("usage: route del <dst>".into())
            } else {
                Ok(ServerAdminReq::RouteDel {
                    dst: tail.to_string(),
                })
            }
        }
        "add" => {
            let pieces: Vec<&str> = tail.split_whitespace().collect();
            if pieces.len() != 3 || pieces[1] != "via" {
                Err("usage: route add <dst/cidr> via <gw>".into())
            } else {
                Ok(ServerAdminReq::RouteAdd {
                    dst: pieces[0].to_string(),
                    via: pieces[2].to_string(),
                })
            }
        }
        _ => Err("usage: route add|del|list|push".into()),
    }
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
        assert_eq!(
            parse("approve 7").unwrap(),
            ReplCmd::Req(ServerAdminReq::Approve { sid: 7 })
        );
        assert_eq!(
            parse("deny 9").unwrap(),
            ReplCmd::Req(ServerAdminReq::Deny { sid: 9 })
        );
        assert_eq!(
            parse("setip abcd 10.0.0.5/24").unwrap(),
            ReplCmd::Req(ServerAdminReq::SetIp {
                prefix: "abcd".into(),
                ip: "10.0.0.5/24".into()
            })
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
    fn parses_route_subcommands() {
        assert_eq!(
            parse("route push").unwrap(),
            ReplCmd::Req(ServerAdminReq::RoutePush)
        );
        assert_eq!(
            parse("route add 192.168.1.0/24 via 10.0.0.1").unwrap(),
            ReplCmd::Req(ServerAdminReq::RouteAdd {
                dst: "192.168.1.0/24".into(),
                via: "10.0.0.1".into()
            })
        );
        assert_eq!(
            parse("route del 192.168.1.0/24").unwrap(),
            ReplCmd::Req(ServerAdminReq::RouteDel {
                dst: "192.168.1.0/24".into()
            })
        );
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("mknet").is_err());
        assert!(parse("approve abc").is_err());
        assert!(parse("route").is_err());
        assert!(parse("garbage").is_err());
    }

    #[test]
    fn setip_with_empty_ip_clears() {
        // Hub treats empty `ip` as "clear"; the REPL forwards verbatim.
        assert_eq!(
            parse("setip abcd").unwrap(),
            ReplCmd::Req(ServerAdminReq::SetIp {
                prefix: "abcd".into(),
                ip: String::new()
            })
        );
    }
}
