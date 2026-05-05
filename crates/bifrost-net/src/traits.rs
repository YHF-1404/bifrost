use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::IpNet;

use crate::types::RouteEntry;

/// A user-space TAP device, asynchronously readable and writable.
///
/// Methods take `&self` so concrete implementations can keep their
/// fd/state behind interior mutability and the trait can be wrapped in
/// `Arc<dyn Tap>` for shared ownership across the [`Hub`] and the
/// connection task without a write lock on the hot path.
///
/// [`Hub`]: https://docs.rs/bifrost-core
#[async_trait]
pub trait Tap: Send + Sync + 'static {
    /// Kernel name of the interface (e.g. `"tap1234abcd"`).
    fn name(&self) -> &str;

    /// Read one Ethernet frame into `buf`. Returns the byte count.
    async fn read(&self, buf: &mut [u8]) -> io::Result<usize>;

    /// Write one Ethernet frame.
    async fn write(&self, frame: &[u8]) -> io::Result<usize>;

    /// Replace the IP/CIDR assigned to this TAP. `None` clears the
    /// address. Implementations should be idempotent on repeat calls
    /// with the same value.
    async fn set_ip(&self, ip: Option<IpNet>) -> io::Result<()>;

    /// Replace the device-scoped routing table. Implementations should
    /// preserve kernel-installed direct-connect routes (Linux:
    /// `proto kernel`).
    async fn apply_routes(&self, routes: &[RouteEntry]) -> io::Result<()>;

    /// Best-effort destruction of the kernel device. Idempotent;
    /// subsequent reads/writes return errors.
    async fn destroy(&self) -> io::Result<()>;
}

/// Layer-2 bridge.
///
/// Server-side concept; clients don't create bridges. The trait is split
/// from [`Tap`] so client-only platforms (macOS utun, Windows wintun)
/// can omit it and still satisfy `Platform::create_tap`.
#[async_trait]
pub trait Bridge: Send + Sync + 'static {
    fn name(&self) -> &str;

    /// Make `tap_name` a port of this bridge.
    async fn add_tap(&self, tap_name: &str) -> io::Result<()>;

    /// Remove `tap_name` from this bridge. Idempotent.
    async fn remove_tap(&self, tap_name: &str) -> io::Result<()>;

    /// Replace the IP/CIDR assigned to this bridge. `None` clears the
    /// address. Idempotent on repeat calls with the same value.
    async fn set_ip(&self, ip: Option<IpNet>) -> io::Result<()>;

    /// Replace the kernel routes that flow through this bridge.
    /// Implementations install each entry with the bridge as the
    /// output device, so the host's routing table can reach the
    /// `via` gateway over its local TAP ports.
    ///
    /// Pre-existing user-installed routes through this device are
    /// flushed; kernel-installed direct-connect routes are preserved.
    async fn apply_routes(&self, routes: &[RouteEntry]) -> io::Result<()>;

    /// Best-effort destruction of the kernel bridge. Idempotent.
    async fn destroy(&self) -> io::Result<()>;
}

/// Factory for the running platform.
///
/// Returned trait objects use `Arc` so the same TAP can be observed by
/// the session task (read/write) and by future per-session pcap
/// recorders without an extra layer of indirection.
#[async_trait]
pub trait Platform: Send + Sync + 'static {
    /// Create a TAP device with the given kernel name and optional IP.
    async fn create_tap(&self, name: &str, ip: Option<IpNet>) -> io::Result<Arc<dyn Tap>>;

    /// Create a bridge. Client-only platforms return
    /// [`io::ErrorKind::Unsupported`].
    async fn create_bridge(&self, name: &str, ip: Option<IpNet>) -> io::Result<Arc<dyn Bridge>>;
}
