//! Linux TAP device: ioctl-bound user-space network interface that
//! reads and writes raw Ethernet frames over a file descriptor.
//!
//! ```text
//!     open(/dev/net/tun)            → fd
//!     ioctl(fd, TUNSETIFF, &ifr)    → kernel creates "tap-XYZ"
//!     fcntl(fd, O_NONBLOCK)         → non-blocking I/O
//!     AsyncFd<OwnedFd>              → tokio-friendly readiness
//! ```
//!
//! `set_ip` / `apply_routes` / `destroy` go via rtnetlink so we never
//! shell out to `/sbin/ip`.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::IpNet;
use rtnetlink::Handle;
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tracing::{debug, warn};

use crate::traits::Tap;
use crate::types::RouteEntry;

// `struct ifreq` — only the fields we touch (name + flags). Linux's
// real ifreq is 40 bytes; we pad to match so the kernel doesn't read
// past our buffer.
#[repr(C)]
#[derive(Default)]
struct IfReq {
    ifr_name: [u8; 16],
    ifr_flags: u16,
    _padding: [u8; 22],
}

const IFF_TAP: u16 = 0x0002;
const IFF_NO_PI: u16 = 0x1000;

// Direction = WRITE, type = 'T' (0x54), nr = 202, size = c_int.
// `request_code_write!('T', 202, libc::c_int)` → 0x400454ca on every
// Linux ABI we care about, but we use the macro for portability.
nix::ioctl_write_int!(tunsetiff, b'T', 202);

pub struct LinuxTap {
    name: String,
    if_index: u32,
    fd: AsyncFd<OwnedFd>,
    handle: Handle,
    /// Set true after a successful `destroy` so subsequent calls are no-ops.
    gone: AtomicBool,
}

impl LinuxTap {
    /// Open `/dev/net/tun`, hand the kernel an `ifreq` configured for
    /// TAP+NO_PI, set the resulting fd to non-blocking, and bring the
    /// link up via rtnetlink.
    pub async fn create(name: &str, ip: Option<IpNet>, handle: Handle) -> io::Result<Arc<Self>> {
        if name.len() >= 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("tap name too long: {name:?} (max 15 bytes)"),
            ));
        }

        // O_NONBLOCK from the start keeps us out of a race where the
        // kernel fills the queue before we mark it non-blocking.
        let raw_fd = unsafe {
            libc::open(
                c"/dev/net/tun".as_ptr(),
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        // Build ifreq.
        let mut ifr = IfReq::default();
        let bytes = name.as_bytes();
        ifr.ifr_name[..bytes.len()].copy_from_slice(bytes);
        ifr.ifr_flags = IFF_TAP | IFF_NO_PI;

        // ioctl(fd, TUNSETIFF, &ifr) — pass the struct by pointer.
        let rc = unsafe {
            libc::ioctl(
                owned.as_raw_fd(),
                tunsetiff_request(),
                &ifr as *const IfReq as *const libc::c_void,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }

        let async_fd =
            AsyncFd::with_interest(owned, Interest::READABLE | Interest::WRITABLE)?;

        // Look up the kernel-assigned index, bring the link up.
        let if_index = lookup_if_index(&handle, name).await?;
        bring_link_up(&handle, if_index).await?;

        if let Some(net) = ip {
            add_addr(&handle, if_index, net).await?;
        }

        debug!(tap = name, ifindex = if_index, "tap created");
        Ok(Arc::new(Self {
            name: name.to_owned(),
            if_index,
            fd: async_fd,
            handle,
            gone: AtomicBool::new(false),
        }))
    }
}

/// Return the precomputed TUNSETIFF request number. We compute it once
/// via the `ioctl_write_int!` macro definition above.
fn tunsetiff_request() -> libc::c_ulong {
    // `nix::ioctl_write_int!` defines `tunsetiff` as a function. Its
    // request number isn't exposed directly, so we recompute it: write
    // direction + 'T' type + 202 nr + sizeof(c_int).
    nix::request_code_write!(b'T', 202, std::mem::size_of::<libc::c_int>()) as libc::c_ulong
}

#[async_trait]
impl Tap for LinuxTap {
    fn name(&self) -> &str {
        &self.name
    }

    async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|fd| {
                let raw = fd.get_ref().as_raw_fd();
                let n = unsafe {
                    libc::read(raw, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    async fn write(&self, frame: &[u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|fd| {
                let raw = fd.get_ref().as_raw_fd();
                let n = unsafe {
                    libc::write(raw, frame.as_ptr() as *const libc::c_void, frame.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    async fn set_ip(&self, ip: Option<IpNet>) -> io::Result<()> {
        // Replace strategy: drop every IPv4/IPv6 address currently
        // bound to the device, then add the new one.
        flush_addrs(&self.handle, self.if_index).await?;
        if let Some(net) = ip {
            add_addr(&self.handle, self.if_index, net).await?;
        }
        Ok(())
    }

    async fn apply_routes(&self, routes: &[RouteEntry]) -> io::Result<()> {
        flush_user_routes(&self.handle, self.if_index).await?;
        for r in routes {
            if let Err(e) = add_route(&self.handle, self.if_index, r).await {
                warn!(error = %e, dst = %r.dst, "skip route");
            }
        }
        Ok(())
    }

    async fn destroy(&self) -> io::Result<()> {
        if self.gone.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Deleting the link makes the fd unusable; the kernel will
        // EOF subsequent reads.
        if let Err(e) = del_link(&self.handle, self.if_index).await {
            warn!(error = %e, tap = self.name, "tap link delete failed");
        }
        Ok(())
    }
}

impl Drop for LinuxTap {
    fn drop(&mut self) {
        // Best-effort: only synchronous knob we have at drop is closing
        // the fd, which the OwnedFd does for us. Link teardown happens
        // via `destroy`; when that wasn't called (e.g. test panic) the
        // tap leaks until next reboot or manual `ip link delete`.
    }
}

// ─── rtnetlink helpers ────────────────────────────────────────────────────

/// Resolve `name` to its kernel `ifindex`. ENODEV (link missing) is
/// translated to `io::ErrorKind::NotFound` so callers can differentiate
/// "doesn't exist yet" from "netlink itself broke".
pub(super) async fn lookup_if_index(handle: &Handle, name: &str) -> io::Result<u32> {
    use futures::TryStreamExt;
    let mut stream = handle.link().get().match_name(name.to_owned()).execute();
    match stream.try_next().await {
        Ok(Some(link)) => Ok(link.header.index),
        Ok(None) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("link {name:?} not found"),
        )),
        Err(rtnetlink::Error::NetlinkError(em))
            if em.code.map(|c| c.get()) == Some(-libc::ENODEV) =>
        {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("link {name:?} not found"),
            ))
        }
        Err(e) => Err(io_other(e)),
    }
}

async fn bring_link_up(handle: &Handle, idx: u32) -> io::Result<()> {
    handle.link().set(idx).up().execute().await.map_err(io_other)
}

async fn add_addr(handle: &Handle, idx: u32, net: IpNet) -> io::Result<()> {
    handle
        .address()
        .add(idx, net.addr(), net.prefix_len())
        .execute()
        .await
        .map_err(io_other)
}

async fn flush_addrs(handle: &Handle, idx: u32) -> io::Result<()> {
    use futures::TryStreamExt;
    let mut stream = handle.address().get().set_link_index_filter(idx).execute();
    while let Some(addr_msg) = stream.try_next().await.map_err(io_other)? {
        // Best effort — del returns Err if the kernel already removed
        // the addr (race with a concurrent flush); ignore those.
        let _ = handle.address().del(addr_msg).execute().await;
    }
    Ok(())
}

pub(super) async fn flush_user_routes(handle: &Handle, idx: u32) -> io::Result<()> {
    use futures::TryStreamExt;
    use netlink_packet_route::route::{RouteAttribute, RouteProtocol};

    let mut stream = handle.route().get(rtnetlink::IpVersion::V4).execute();
    while let Some(msg) = stream.try_next().await.map_err(io_other)? {
        // Match: route on this oif (output interface) and not kernel-
        // installed (proto kernel — direct connected route).
        let mut on_us = false;
        for attr in &msg.attributes {
            if let RouteAttribute::Oif(oif) = attr {
                if *oif == idx {
                    on_us = true;
                    break;
                }
            }
        }
        if on_us && msg.header.protocol != RouteProtocol::Kernel {
            let _ = handle.route().del(msg).execute().await;
        }
    }
    let mut stream = handle.route().get(rtnetlink::IpVersion::V6).execute();
    while let Some(msg) = stream.try_next().await.map_err(io_other)? {
        let mut on_us = false;
        for attr in &msg.attributes {
            if let RouteAttribute::Oif(oif) = attr {
                if *oif == idx {
                    on_us = true;
                    break;
                }
            }
        }
        if on_us && msg.header.protocol != RouteProtocol::Kernel {
            let _ = handle.route().del(msg).execute().await;
        }
    }
    Ok(())
}

pub(super) async fn add_route(handle: &Handle, idx: u32, r: &RouteEntry) -> io::Result<()> {
    match (r.dst, r.via) {
        (IpNet::V4(dst), std::net::IpAddr::V4(gw)) => {
            handle
                .route()
                .add()
                .v4()
                .destination_prefix(dst.network(), dst.prefix_len())
                .gateway(gw)
                .output_interface(idx)
                .execute()
                .await
                .map_err(io_other)
        }
        (IpNet::V6(dst), std::net::IpAddr::V6(gw)) => {
            handle
                .route()
                .add()
                .v6()
                .destination_prefix(dst.network(), dst.prefix_len())
                .gateway(gw)
                .output_interface(idx)
                .execute()
                .await
                .map_err(io_other)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("route family mismatch: dst={} via={}", r.dst, r.via),
        )),
    }
}

pub(super) async fn del_link(handle: &Handle, idx: u32) -> io::Result<()> {
    handle.link().del(idx).execute().await.map_err(io_other)
}

pub(super) fn io_other<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(format!("{e}"))
}
