//! Linux L2 bridge via rtnetlink.
//!
//! `add().bridge(name)` creates `name` with `kind = bridge`; we then
//! `set(idx).up()` and optionally add an IP. Member ports are attached
//! through `link().set(member_idx).controller(br_idx)`.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::IpNet;
use rtnetlink::Handle;
use tracing::{debug, warn};

use super::tap::{
    add_route, del_link, flush_user_routes, io_other, lookup_if_index, set_link_mtu, TAP_MTU,
};
use crate::traits::Bridge;
use crate::types::RouteEntry;

pub struct LinuxBridge {
    name: String,
    if_index: u32,
    handle: Handle,
    gone: AtomicBool,
}

impl LinuxBridge {
    pub async fn create(
        name: &str,
        ip: Option<IpNet>,
        handle: Handle,
    ) -> io::Result<Arc<Self>> {
        // If a stale bridge of the same name is hanging around (e.g.
        // from a prior crashed run) reuse it; otherwise create fresh.
        let if_index = match lookup_if_index(&handle, name).await {
            Ok(idx) => {
                debug!(bridge = name, ifindex = idx, "reusing existing bridge");
                idx
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                handle
                    .link()
                    .add()
                    .bridge(name.to_owned())
                    .execute()
                    .await
                    .map_err(io_other)?;
                let idx = lookup_if_index(&handle, name).await?;
                debug!(bridge = name, ifindex = idx, "bridge created");
                idx
            }
            Err(e) => return Err(e),
        };

        // Match the bridge MTU to its member TAPs (TAP_MTU). Linux's
        // bridge MTU defaults to 1500; if a member TAP has a smaller
        // MTU the kernel auto-shrinks the bridge to match, but setting
        // it explicitly avoids depending on that timing and makes the
        // host-side address (br-bifrost) advertise the correct MSS to
        // local listeners (e.g. sshd on 10.0.0.1 negotiating with a
        // remote scp).
        if let Err(e) = set_link_mtu(&handle, if_index, TAP_MTU).await {
            warn!(error = %e, bridge = name, mtu = TAP_MTU, "set bridge MTU failed");
        }

        // Up + IP. The kernel may already have the link up if we
        // re-used an existing bridge; setting it up again is a no-op.
        handle
            .link()
            .set(if_index)
            .up()
            .execute()
            .await
            .map_err(io_other)?;

        if let Some(net) = ip {
            // Best-effort: ignore "address already exists" errors.
            if let Err(e) = handle
                .address()
                .add(if_index, net.addr(), net.prefix_len())
                .execute()
                .await
            {
                warn!(error = %e, bridge = name, "address add failed (may already exist)");
            }
        }

        Ok(Arc::new(Self {
            name: name.to_owned(),
            if_index,
            handle,
            gone: AtomicBool::new(false),
        }))
    }
}

#[async_trait]
impl Bridge for LinuxBridge {
    fn name(&self) -> &str {
        &self.name
    }

    async fn add_tap(&self, tap_name: &str) -> io::Result<()> {
        let tap_idx = lookup_if_index(&self.handle, tap_name).await?;
        self.handle
            .link()
            .set(tap_idx)
            .controller(self.if_index)
            .execute()
            .await
            .map_err(io_other)
    }

    async fn remove_tap(&self, tap_name: &str) -> io::Result<()> {
        let tap_idx = match lookup_if_index(&self.handle, tap_name).await {
            Ok(i) => i,
            // Already gone — idempotent.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        // controller(0) detaches the port.
        self.handle
            .link()
            .set(tap_idx)
            .controller(0)
            .execute()
            .await
            .map_err(io_other)
    }

    async fn apply_routes(&self, routes: &[RouteEntry]) -> io::Result<()> {
        // Routes that flow OUT of this host toward a client's TAP go
        // via the bridge as their `dev`. Per-route `via` is a
        // 10.0.0.x address that the bridge knows how to reach because
        // the client's TAP is one of its ports.
        flush_user_routes(&self.handle, self.if_index).await?;
        for r in routes {
            if let Err(e) = add_route(&self.handle, self.if_index, r).await {
                warn!(error = %e, dst = %r.dst, via = %r.via, "skip bridge route");
            }
        }
        Ok(())
    }

    async fn destroy(&self) -> io::Result<()> {
        if self.gone.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Err(e) = del_link(&self.handle, self.if_index).await {
            warn!(error = %e, bridge = self.name, "bridge link delete failed");
        }
        Ok(())
    }
}

