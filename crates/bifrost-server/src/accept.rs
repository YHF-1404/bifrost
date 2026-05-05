//! Accept loop. Spawns a [`crate::conn::run`] task per inbound socket.

use std::path::PathBuf;

use bifrost_core::HubHandle;
use tokio::net::TcpListener;
use tracing::{info, warn};
use uuid::Uuid;

/// Accept connections until the listener errors out (typically because
/// the runtime is shutting down).
pub async fn run(listener: TcpListener, hub: HubHandle, server_id: Uuid, save_dir: PathBuf) {
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "accept failed; loop continuing");
                continue;
            }
        };
        // Disable Nagle. The data plane is one TAP frame per length-
        // prefixed TCP write; with Nagle on, every write waits for the
        // previous segment's ACK or a 200 ms timer, which throttles bulk
        // transfers (e.g. SCP) to ~10 % of line rate over a non-trivial
        // RTT. Bifrost frames are already large enough that batching
        // doesn't help.
        if let Err(e) = stream.set_nodelay(true) {
            warn!(%addr, error = %e, "set_nodelay failed (continuing)");
        }
        // Bound the kernel SNDBUF so writes block once we have ~256 KB
        // outstanding. Without this, Linux auto-tunes to a few MB and
        // the inner-TCP traffic riding on top has nowhere to drop —
        // CUBIC keeps growing cwnd through nested tunnels (xray, etc.)
        // until the bufferbloat collapses bulk throughput. Capping the
        // outer buffer pushes loss back into the TAP queue, where the
        // inner TCP can actually see it and back off.
        #[cfg(target_os = "linux")]
        if let Err(e) = bifrost_net::set_send_buffer_size(&stream, 256 * 1024) {
            warn!(%addr, error = %e, "set_send_buffer_size failed (continuing)");
        }
        info!(%addr, "accepted");
        let hub = hub.clone();
        let save_dir = save_dir.clone();
        tokio::spawn(crate::conn::run(
            stream,
            addr.to_string(),
            hub,
            server_id,
            save_dir,
        ));
    }
}
