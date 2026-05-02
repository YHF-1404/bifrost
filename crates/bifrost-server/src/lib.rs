//! Bifrost server driver library.
//!
//! Decomposed into:
//!
//! * [`cli`] — CLI parsing.
//! * [`conn`] — per-TCP-connection task. Owns one `Framed<S, FrameCodec>`
//!   and a `bind_rx` receiver via which the [`bifrost_core::Hub`] tells
//!   it which session to forward Ethernet frames to.
//! * [`accept`] — accept loop that spawns a [`conn::run`] per inbound
//!   socket.
//! * [`repl`] — blocking-thread `rustyline` REPL with the full server
//!   command set.
//!
//! The `main.rs` binary glues everything together; integration tests
//! exercise [`conn::run`] over `tokio::io::duplex` pipes so the
//! end-to-end flow can be verified without real sockets.

#![forbid(unsafe_code)]

pub mod accept;
pub mod admin;
pub mod cli;
pub mod conn;
pub mod dispatch;
pub mod repl;
