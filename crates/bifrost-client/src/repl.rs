//! Blocking-thread REPL.
//!
//! `rustyline`'s editor is sync; we run it on a `spawn_blocking` thread
//! and bridge to the async controller via `mpsc::Sender::blocking_send`.

use std::path::PathBuf;

use rustyline::error::ReadlineError;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Snapshot of client state returned by [`UserCmd::Status`].
#[derive(Debug, Clone)]
pub struct ClientStatusSnapshot {
    pub client_uuid: Uuid,
    pub connected: bool,
    pub joined_network: Option<Uuid>,
    pub tap_name: Option<String>,
    pub tap_ip: Option<String>,
}

/// User-issued commands forwarded from the REPL or admin socket to
/// [`crate::app::App`].
///
/// Not `PartialEq` because `Status` carries a `oneshot::Sender`.
#[derive(Debug)]
pub enum UserCmd {
    Join(Uuid),
    Leave,
    SendText(String),
    /// File data already in memory. The REPL reads from disk before
    /// sending; the admin socket receives bytes over the wire.
    SendFile {
        name: String,
        data: Vec<u8>,
    },
    /// Reply with current state via the supplied oneshot.
    Status(oneshot::Sender<ClientStatusSnapshot>),
    /// Quit the program.
    Quit,
}

/// Run the REPL on the calling thread until the user types `quit` or
/// the controller drops the receiver. Always sends a final
/// [`UserCmd::Quit`] before returning so the controller wakes and
/// shuts down cleanly.
pub fn run(tx: mpsc::Sender<UserCmd>) {
    let mut rl: rustyline::Editor<(), _> = match rustyline::Editor::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[!] readline init failed: {e}");
            return;
        }
    };

    println!("commands: join <uuid> | leave | send <msg> | sendfile <path> | status | quit");

    loop {
        match rl.readline("> ") {
            Ok(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&line);

                // Special-case `status` so the REPL can synchronously
                // print the snapshot before returning to the prompt.
                if line == "status" {
                    let (s_tx, s_rx) = oneshot::channel();
                    if tx.blocking_send(UserCmd::Status(s_tx)).is_err() {
                        return;
                    }
                    match s_rx.blocking_recv() {
                        Ok(snap) => println!("{}", format_status(&snap)),
                        Err(_) => return,
                    }
                    continue;
                }

                match parse(&line) {
                    Ok(Some(cmd)) => {
                        let is_quit = matches!(cmd, UserCmd::Quit);
                        if tx.blocking_send(cmd).is_err() {
                            return;
                        }
                        if is_quit {
                            return;
                        }
                    }
                    Ok(None) => println!(
                        "[!] unknown command. Try: join | leave | send | sendfile | status | quit"
                    ),
                    Err(e) => println!("[!] {e}"),
                }
            }
            Err(ReadlineError::Eof | ReadlineError::Interrupted) => {
                let _ = tx.blocking_send(UserCmd::Quit);
                return;
            }
            Err(e) => {
                eprintln!("[!] readline error: {e}");
                let _ = tx.blocking_send(UserCmd::Quit);
                return;
            }
        }
    }
}

pub fn format_status(s: &ClientStatusSnapshot) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "client_uuid:    {}", s.client_uuid);
    let _ = writeln!(out, "connected:      {}", s.connected);
    let _ = writeln!(
        out,
        "joined_network: {}",
        s.joined_network
            .map(|u| u.to_string())
            .unwrap_or_else(|| "(none)".into())
    );
    let _ = writeln!(out, "tap_name:       {}", s.tap_name.as_deref().unwrap_or("-"));
    let _ = write!(out, "tap_ip:         {}", s.tap_ip.as_deref().unwrap_or("-"));
    out
}

/// Parse one command line, reading file contents off disk for `sendfile`.
///
/// Returns `Ok(None)` for unknown verbs or empty input; `Err` for
/// recognised verbs whose arguments are malformed (bad UUID, missing
/// path, etc.).
pub fn parse(line: &str) -> Result<Option<UserCmd>, String> {
    let mut parts = line.splitn(2, ' ');
    let head = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match head {
        "join" if !arg.is_empty() => {
            let net = arg.parse::<Uuid>().map_err(|_| format!("invalid uuid: {arg:?}"))?;
            Ok(Some(UserCmd::Join(net)))
        }
        "join" => Err("usage: join <uuid>".into()),
        "leave" => Ok(Some(UserCmd::Leave)),
        "send" if !arg.is_empty() => Ok(Some(UserCmd::SendText(arg.to_string()))),
        "send" => Err("usage: send <msg>".into()),
        "sendfile" if !arg.is_empty() => {
            let path = PathBuf::from(arg);
            let data = std::fs::read(&path).map_err(|e| format!("read {path:?}: {e}"))?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            Ok(Some(UserCmd::SendFile { name, data }))
        }
        "sendfile" => Err("usage: sendfile <path>".into()),
        "quit" | "exit" => Ok(Some(UserCmd::Quit)),
        "" => Ok(None),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_join_leave_send_quit() {
        let net = Uuid::new_v4();
        assert!(matches!(
            parse(&format!("join {net}")).unwrap(),
            Some(UserCmd::Join(u)) if u == net
        ));
        assert!(matches!(parse("leave").unwrap(), Some(UserCmd::Leave)));
        assert!(matches!(
            parse("send hello world").unwrap(),
            Some(UserCmd::SendText(s)) if s == "hello world"
        ));
        assert!(matches!(parse("quit").unwrap(), Some(UserCmd::Quit)));
        assert!(matches!(parse("exit").unwrap(), Some(UserCmd::Quit)));
    }

    #[test]
    fn parse_sendfile_reads_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, b"hello-disk").unwrap();
        let cmd = parse(&format!("sendfile {}", path.display()))
            .unwrap()
            .unwrap();
        match cmd {
            UserCmd::SendFile { name, data } => {
                assert_eq!(name, "payload.bin");
                assert_eq!(data, b"hello-disk");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("join not-a-uuid").is_err());
        assert!(parse("join").is_err());
        assert!(parse("send").is_err());
        assert!(parse("sendfile").is_err());
        assert!(parse("sendfile /no/such/file/here_xyz").is_err());
    }

    #[test]
    fn unknown_verb_yields_none() {
        assert!(parse("garbage").unwrap().is_none());
        assert!(parse("").unwrap().is_none());
    }
}
