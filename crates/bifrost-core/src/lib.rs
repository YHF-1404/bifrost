//! Bifrost — core control logic.
//!
//! This crate is **runtime-bound** (depends on `tokio`) but transport-
//! agnostic. It owns:
//!
//! * The on-disk **config** schemas for both server and client roles.
//! * Strongly-typed **IDs** for sessions and connections.
//! * A marker **transport** trait so future Noise / TLS wrappers can plug
//!   in without touching the session code.
//! * The **`SessionTask`** that owns a TAP across reconnects and runs
//!   the full `Joined → Disconnected → Dead` state machine.
//!
//! The `Hub` actor — which routes between connections, sessions, and the
//! REPL — lives next door (P0+1) and is built on top of the same
//! command/event types defined here.

#![forbid(unsafe_code)]

pub mod atomic_write;
pub mod config;
pub mod error;
pub mod events;
pub mod hub;
pub mod ids;
pub mod routes;
pub mod session;
pub mod transport;

pub use error::CoreError;
pub use events::{HubEvent, MetricsSample};
pub use hub::{
    ConnLink, DevicePushResult, DeviceSetResult, DeviceUpdate, Hub, HubCmd, HubHandle, HubSnapshot,
    PendingInfo, SessionInfo,
};
pub use ids::{ConnId, SessionId};
pub use session::{
    DeathReason, DisconnectTimeout, SessionCmd, SessionEvt, SessionTask, DEFAULT_DISCONNECT_TIMEOUT,
};
