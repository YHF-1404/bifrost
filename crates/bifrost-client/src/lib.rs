//! Bifrost client driver library.
//!
//! Decomposed into four modules:
//!
//! * [`cli`] — argument parsing (`clap` derive).
//! * [`conn`] — TCP / SOCKS5 connect plus reconnect supervisor.
//! * [`app`] — controller that wires connection events, REPL commands,
//!   and a `bifrost-core` `SessionTask` together. The state machine
//!   for "joining / disconnecting / rejoining" lives here.
//! * [`repl`] — blocking-thread `rustyline` REPL.
//!
//! `main.rs` glues these together; `tests/app_lifecycle.rs` exercises
//! the [`app::App`] state machine against an in-memory event stream.

#![forbid(unsafe_code)]

pub mod admin;
pub mod app;
pub mod cli;
pub mod conn;
pub mod dispatch;
pub mod repl;
