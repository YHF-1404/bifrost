//! `SessionTask` — owns a single client's TAP across reconnects.
//!
//! ```text
//!                  ┌────── BindConn ──────┐
//!                  │                      ▼
//!                Joined ◀── BindConn ── Disconnected
//!                  │                      │
//!         Kill /   │                      │  Timeout / Kill /
//!     Tap error    │                      │  cmd_rx closed
//!                  ▼                      ▼
//!                 Dead ◀──────────────────┘
//! ```
//!
//! The task is born **already in `Joined`** (the Hub creates it only after
//! approving a join), forwards Ethernet frames in both directions, and
//! winds itself down by sending exactly one [`SessionEvt::Died`] before
//! returning. The owned [`Tap`] is `destroy`d on death — including via
//! the `Drop` impl on the concrete platform type, as a belt-and-braces
//! guard against the task being aborted instead of returning normally.

use std::sync::Arc;
use std::time::Duration;

use bifrost_net::{RouteEntry, Tap};
use bifrost_proto::{Frame, RouteEntry as WireRoute};
use ipnet::IpNet;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, trace, warn};
use uuid::Uuid;

use crate::ids::SessionId;

/// Default for [`SessionTask`] when no config override is provided.
pub const DEFAULT_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Disconnect-timeout configuration.
///
/// * Server sessions pass `Some(d)` so a half-dead client eventually
///   releases its TAP.
/// * Client sessions pass `None` — the local TAP must survive arbitrary
///   reconnect storms; only the user's explicit `leave` (sent as
///   `SessionCmd::Kill`) tears it down.
pub type DisconnectTimeout = Option<Duration>;

/// Maximum TAP read size; large enough for jumbo Ethernet.
const READ_BUF: usize = 65_536;

// ── Public command / event types ───────────────────────────────────────────

/// Messages the Hub (or, for `EthIn`, a connection task) sends to a session.
#[derive(Debug)]
pub enum SessionCmd {
    /// Attach a new outbound channel — the session is (re-)entering `Joined`.
    BindConn(mpsc::Sender<Frame>),

    /// Detach the current outbound channel — connection went away.
    /// The session enters `Disconnected` and starts the death timer.
    UnbindConn,

    /// Forward this raw Ethernet frame to the TAP.
    EthIn(Vec<u8>),

    /// Replace the TAP's IP. `None` clears it.
    SetIp(Option<String>),

    /// Replace the device-scoped routing table.
    SetRoutes(Vec<WireRoute>),

    /// Hub-decided termination. The session destroys its TAP and exits.
    Kill,
}

/// Events the session sends back to the Hub. Currently a single shape.
#[derive(Debug)]
pub enum SessionEvt {
    Died { sid: SessionId, reason: DeathReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeathReason {
    /// `disconnect_timeout` elapsed without a reconnect.
    Timeout,
    /// Hub explicitly asked us to die.
    HubKill,
    /// Underlying TAP returned an error.
    TapError(String),
    /// `cmd_rx` was closed (Hub dropped) — implicit shutdown.
    HubGone,
}

// ── Task ───────────────────────────────────────────────────────────────────

/// A long-lived task that owns a TAP and routes frames between it and the
/// currently-bound connection.
///
/// Construct via [`SessionTask::new`], then `tokio::spawn(task.run(...))`.
pub struct SessionTask {
    sid: SessionId,
    client_uuid: Uuid,
    net_uuid: Uuid,
    tap: Arc<dyn Tap>,
    cmd_rx: mpsc::Receiver<SessionCmd>,
    evt_tx: mpsc::Sender<SessionEvt>,
    disconnect_timeout: DisconnectTimeout,
}

impl SessionTask {
    pub fn new(
        sid: SessionId,
        client_uuid: Uuid,
        net_uuid: Uuid,
        tap: Arc<dyn Tap>,
        cmd_rx: mpsc::Receiver<SessionCmd>,
        evt_tx: mpsc::Sender<SessionEvt>,
        disconnect_timeout: DisconnectTimeout,
    ) -> Self {
        Self {
            sid,
            client_uuid,
            net_uuid,
            tap,
            cmd_rx,
            evt_tx,
            disconnect_timeout,
        }
    }

    /// Drive the task until terminal. Always emits a single
    /// `SessionEvt::Died` before returning.
    pub async fn run(mut self, initial_conn: mpsc::Sender<Frame>) {
        debug!(
            sid = %self.sid,
            client = %short(&self.client_uuid),
            net    = %short(&self.net_uuid),
            tap    = self.tap.name(),
            "session start"
        );

        let mut state = LoopState::Joined {
            conn_tx: initial_conn,
        };
        let mut buf = vec![0u8; READ_BUF];

        let reason = loop {
            let outcome = match &mut state {
                LoopState::Joined { conn_tx } => {
                    Self::tick_joined(
                        &mut self.cmd_rx,
                        &*self.tap,
                        conn_tx,
                        &mut buf,
                        self.disconnect_timeout,
                    )
                    .await
                }
                LoopState::Disconnected { deadline } => {
                    Self::tick_disconnected(&mut self.cmd_rx, *deadline).await
                }
            };

            match outcome {
                LoopOutcome::Stay(s) => state = s,
                LoopOutcome::Die(r) => break r,
            }
        };

        // Best-effort TAP teardown. Any failure is logged but doesn't
        // suppress the death event — Hub bookkeeping must not stall.
        if let Err(e) = self.tap.destroy().await {
            warn!(sid = %self.sid, error = %e, "tap destroy failed");
        }

        debug!(sid = %self.sid, ?reason, "session end");
        let _ = self
            .evt_tx
            .send(SessionEvt::Died {
                sid: self.sid,
                reason,
            })
            .await;
    }

    // ── Tick helpers ────────────────────────────────────────────────────

    async fn tick_joined(
        cmd_rx: &mut mpsc::Receiver<SessionCmd>,
        tap: &dyn Tap,
        conn_tx: &mut mpsc::Sender<Frame>,
        buf: &mut [u8],
        disconnect_timeout: DisconnectTimeout,
    ) -> LoopOutcome {
        tokio::select! {
            biased;

            cmd = cmd_rx.recv() => match cmd {
                None => LoopOutcome::Die(DeathReason::HubGone),
                Some(SessionCmd::Kill) => LoopOutcome::Die(DeathReason::HubKill),
                Some(SessionCmd::UnbindConn) => LoopOutcome::Stay(LoopState::Disconnected {
                    deadline: disconnect_timeout.map(|t| Instant::now() + t),
                }),
                Some(SessionCmd::BindConn(new_tx)) => {
                    *conn_tx = new_tx;
                    LoopOutcome::Stay(LoopState::Joined { conn_tx: conn_tx.clone() })
                }
                Some(SessionCmd::EthIn(frame)) => {
                    if let Err(e) = tap.write(&frame).await {
                        return LoopOutcome::Die(DeathReason::TapError(e.to_string()));
                    }
                    LoopOutcome::Stay(LoopState::Joined { conn_tx: conn_tx.clone() })
                }
                Some(SessionCmd::SetIp(ip_str)) => {
                    let parsed = parse_ip_cidr(ip_str.as_deref());
                    if let Err(e) = tap.set_ip(parsed).await {
                        warn!(error = %e, "set_ip failed");
                    }
                    LoopOutcome::Stay(LoopState::Joined { conn_tx: conn_tx.clone() })
                }
                Some(SessionCmd::SetRoutes(wire_routes)) => {
                    let routes = parse_routes(&wire_routes);
                    if let Err(e) = tap.apply_routes(&routes).await {
                        warn!(error = %e, "apply_routes failed");
                    }
                    LoopOutcome::Stay(LoopState::Joined { conn_tx: conn_tx.clone() })
                }
            },

            read = tap.read(buf) => match read {
                Ok(n) => {
                    let frame = Frame::Eth(buf[..n].to_vec());
                    match conn_tx.send(frame).await {
                        Ok(()) => {
                            trace!(bytes = n, "tap → conn");
                            LoopOutcome::Stay(LoopState::Joined { conn_tx: conn_tx.clone() })
                        }
                        Err(_) => {
                            // Outbound channel dropped — the conn task
                            // is gone. Treat this as an implicit unbind
                            // so the Hub's UnbindConn arrives idempotently.
                            LoopOutcome::Stay(LoopState::Disconnected {
                                deadline: disconnect_timeout.map(|t| Instant::now() + t),
                            })
                        }
                    }
                }
                Err(e) => LoopOutcome::Die(DeathReason::TapError(e.to_string())),
            },
        }
    }

    async fn tick_disconnected(
        cmd_rx: &mut mpsc::Receiver<SessionCmd>,
        deadline: Option<Instant>,
    ) -> LoopOutcome {
        tokio::select! {
            biased;

            cmd = cmd_rx.recv() => match cmd {
                None => LoopOutcome::Die(DeathReason::HubGone),
                Some(SessionCmd::Kill) => LoopOutcome::Die(DeathReason::HubKill),
                Some(SessionCmd::BindConn(new_tx)) => {
                    LoopOutcome::Stay(LoopState::Joined { conn_tx: new_tx })
                }
                // While disconnected, ignore everything else; the next
                // BindConn re-applies state via Hub-driven SetIp/SetRoutes
                // pushes anyway.
                Some(_) => LoopOutcome::Stay(LoopState::Disconnected { deadline }),
            },

            _ = wait_for_deadline(deadline) => LoopOutcome::Die(DeathReason::Timeout),
        }
    }
}

/// Awaits `deadline` if `Some`, otherwise blocks forever — letting the
/// surrounding `select!` ignore the timeout arm when the session has no
/// disconnect cap (the client uses this).
async fn wait_for_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
}

// ── Internal state plumbing ────────────────────────────────────────────────

enum LoopState {
    Joined { conn_tx: mpsc::Sender<Frame> },
    Disconnected { deadline: Option<Instant> },
}

enum LoopOutcome {
    Stay(LoopState),
    Die(DeathReason),
}

// ── Parsing helpers ────────────────────────────────────────────────────────

fn parse_ip_cidr(s: Option<&str>) -> Option<IpNet> {
    let raw = s?.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<IpNet>().ok().or_else(|| {
        // Tolerate a bare IP like "10.0.0.5" by promoting it to /32 (or /128).
        let ip: std::net::IpAddr = raw.parse().ok()?;
        Some(IpNet::new(ip, if ip.is_ipv4() { 32 } else { 128 }).unwrap())
    })
}

fn parse_routes(wire: &[WireRoute]) -> Vec<RouteEntry> {
    wire.iter()
        .filter_map(|r| match RouteEntry::parse(&r.dst, &r.via) {
            Ok(rt) => Some(rt),
            Err(e) => {
                warn!(error = %e, dst = r.dst, via = r.via, "drop malformed route");
                None
            }
        })
        .collect()
}

fn short(u: &Uuid) -> String {
    u.simple().to_string()[..8].to_owned()
}
