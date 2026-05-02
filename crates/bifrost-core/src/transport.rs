//! Transport abstraction.
//!
//! This is intentionally a **marker trait** — any
//! `AsyncRead + AsyncWrite + Send + Unpin + 'static` type satisfies it
//! via the blanket impl. Today that's `tokio::net::TcpStream` and
//! `tokio_socks::tcp::Socks5Stream<TcpStream>`. Tomorrow it will also be
//! `tokio_rustls::client::TlsStream<…>` or a Noise wrapper, with **no
//! changes** required to the session / conn code.
//!
//! Frame-level encoding is kept separate (see [`bifrost_proto`]); the
//! transport's only job is to carry bytes.

use tokio::io::{AsyncRead, AsyncWrite};

/// Anything that can carry the Bifrost wire protocol bytes.
pub trait Transport: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Transport for T {}
