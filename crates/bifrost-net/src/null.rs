//! "No-op" platform that compiles on every target.
//!
//! Useful for two cases:
//!
//! * **Cross-OS dev builds** of the client/server binaries — the
//!   binaries link, but a real run on a non-supported host will see
//!   `Unsupported` errors as soon as a TAP is requested.
//! * **Smoke tests** that exercise the wiring without spawning any
//!   real kernel objects.
//!
//! `NullPlatform::create_bridge` *succeeds* and returns a [`NullBridge`]
//! whose `add_tap` errors with `Unsupported`. This is what makes the
//! server binary start up cleanly on macOS for REPL testing — the
//! bridge handle is held but never actually does anything.

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::IpNet;

use crate::traits::{Bridge, Platform, Tap};
use crate::types::RouteEntry;

/// Platform whose `create_tap` returns [`io::ErrorKind::Unsupported`]
/// and whose `create_bridge` returns a [`NullBridge`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NullPlatform;

#[async_trait]
impl Platform for NullPlatform {
    async fn create_tap(&self, _name: &str, _ip: Option<IpNet>) -> io::Result<Arc<dyn Tap>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no TAP backend compiled in for this platform",
        ))
    }

    async fn create_bridge(&self, name: &str, _ip: Option<IpNet>) -> io::Result<Arc<dyn Bridge>> {
        Ok(Arc::new(NullBridge {
            name: name.to_owned(),
        }) as Arc<dyn Bridge>)
    }
}

/// Non-functional bridge used by [`NullPlatform`]. `add_tap` errors;
/// everything else is a successful no-op so shutdown stays clean.
#[derive(Debug)]
pub struct NullBridge {
    name: String,
}

#[async_trait]
impl Bridge for NullBridge {
    fn name(&self) -> &str {
        &self.name
    }
    async fn add_tap(&self, _tap_name: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no bridge backend compiled in for this platform",
        ))
    }
    async fn remove_tap(&self, _tap_name: &str) -> io::Result<()> {
        Ok(())
    }
    async fn set_ip(&self, _ip: Option<IpNet>) -> io::Result<()> {
        Ok(())
    }
    async fn apply_routes(&self, _routes: &[RouteEntry]) -> io::Result<()> {
        Ok(())
    }
    async fn destroy(&self) -> io::Result<()> {
        Ok(())
    }
}
