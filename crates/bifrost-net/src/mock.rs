//! In-memory test doubles for [`Tap`], [`Bridge`], [`Platform`].
//!
//! `MockTap` exposes the kernel side as two channels:
//!
//! * **`from_kernel`** — bytes the test pushes here become the next
//!   [`Tap::read`] result. Use [`MockTap::inject_frame`].
//! * **`to_kernel`** — bytes the user-space [`Tap::write`]s appear here.
//!   Inspect via [`MockTap::pop_written`].
//!
//! Combined with `MockPlatform`, this lets `bifrost-core` exercise the
//! session state machine end-to-end without any kernel interaction.

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::IpNet;
use tokio::sync::{mpsc, Mutex};

use crate::traits::{Bridge, Platform, Tap};
use crate::types::RouteEntry;

// ─── MockTap ───────────────────────────────────────────────────────────────

/// In-memory TAP for tests.
pub struct MockTap {
    name: String,
    from_kernel_rx: Mutex<mpsc::Receiver<Vec<u8>>>,
    from_kernel_tx: mpsc::Sender<Vec<u8>>,
    to_kernel_tx: mpsc::UnboundedSender<Vec<u8>>,
    to_kernel_rx: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    state: Mutex<MockTapState>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct MockTapState {
    pub ip: Option<IpNet>,
    pub routes: Vec<RouteEntry>,
    pub destroyed: bool,
}

impl MockTap {
    pub fn new(name: &str) -> Arc<Self> {
        let (tx_in, rx_in) = mpsc::channel(64);
        let (tx_out, rx_out) = mpsc::unbounded_channel();
        Arc::new(Self {
            name: name.to_owned(),
            from_kernel_rx: Mutex::new(rx_in),
            from_kernel_tx: tx_in,
            to_kernel_tx: tx_out,
            to_kernel_rx: Mutex::new(rx_out),
            state: Mutex::new(MockTapState::default()),
        })
    }

    /// Push a frame from the "kernel" side; the next [`Tap::read`] returns it.
    pub async fn inject_frame(&self, frame: Vec<u8>) {
        let _ = self.from_kernel_tx.send(frame).await;
    }

    /// Pop one frame the user-space wrote, if any. Returns immediately.
    pub async fn pop_written(&self) -> Option<Vec<u8>> {
        self.to_kernel_rx.lock().await.try_recv().ok()
    }

    /// Pop one frame, waiting up to `timeout` for it.
    pub async fn pop_written_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Option<Vec<u8>> {
        let mut rx = self.to_kernel_rx.lock().await;
        tokio::time::timeout(timeout, rx.recv()).await.ok().flatten()
    }

    /// Snapshot the current `set_ip` / `apply_routes` / `destroy` state.
    pub async fn snapshot(&self) -> MockTapState {
        self.state.lock().await.clone()
    }
}

#[async_trait]
impl Tap for MockTap {
    fn name(&self) -> &str {
        &self.name
    }

    async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut rx = self.from_kernel_rx.lock().await;
        match rx.recv().await {
            Some(frame) => {
                let n = frame.len().min(buf.len());
                buf[..n].copy_from_slice(&frame[..n]);
                Ok(n)
            }
            None => Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "mock tap closed",
            )),
        }
    }

    async fn write(&self, frame: &[u8]) -> io::Result<usize> {
        if self.state.lock().await.destroyed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "tap destroyed"));
        }
        self.to_kernel_tx
            .send(frame.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::ConnectionAborted, "mock rx dropped"))?;
        Ok(frame.len())
    }

    async fn set_ip(&self, ip: Option<IpNet>) -> io::Result<()> {
        self.state.lock().await.ip = ip;
        Ok(())
    }

    async fn apply_routes(&self, routes: &[RouteEntry]) -> io::Result<()> {
        self.state.lock().await.routes = routes.to_vec();
        Ok(())
    }

    async fn destroy(&self) -> io::Result<()> {
        self.state.lock().await.destroyed = true;
        Ok(())
    }
}

// ─── MockBridge ────────────────────────────────────────────────────────────

pub struct MockBridge {
    name: String,
    state: Mutex<MockBridgeState>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct MockBridgeState {
    pub ports: Vec<String>,
    pub routes: Vec<RouteEntry>,
    pub destroyed: bool,
}

impl MockBridge {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            state: Mutex::new(MockBridgeState::default()),
        })
    }

    pub async fn snapshot(&self) -> MockBridgeState {
        self.state.lock().await.clone()
    }
}

#[async_trait]
impl Bridge for MockBridge {
    fn name(&self) -> &str {
        &self.name
    }

    async fn add_tap(&self, tap_name: &str) -> io::Result<()> {
        let mut s = self.state.lock().await;
        if !s.ports.iter().any(|p| p == tap_name) {
            s.ports.push(tap_name.to_owned());
        }
        Ok(())
    }

    async fn remove_tap(&self, tap_name: &str) -> io::Result<()> {
        self.state.lock().await.ports.retain(|p| p != tap_name);
        Ok(())
    }

    async fn apply_routes(&self, routes: &[RouteEntry]) -> io::Result<()> {
        self.state.lock().await.routes = routes.to_vec();
        Ok(())
    }

    async fn destroy(&self) -> io::Result<()> {
        self.state.lock().await.destroyed = true;
        Ok(())
    }
}

// ─── MockPlatform ──────────────────────────────────────────────────────────

#[derive(Default)]
pub struct MockPlatform {
    taps: Mutex<Vec<Arc<MockTap>>>,
    bridges: Mutex<Vec<Arc<MockBridge>>>,
}

impl MockPlatform {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Most recently created TAP, if any.
    pub async fn last_tap(&self) -> Option<Arc<MockTap>> {
        self.taps.lock().await.last().cloned()
    }

    /// Most recently created bridge, if any.
    pub async fn last_bridge(&self) -> Option<Arc<MockBridge>> {
        self.bridges.lock().await.last().cloned()
    }

    pub async fn taps_count(&self) -> usize {
        self.taps.lock().await.len()
    }
}

#[async_trait]
impl Platform for MockPlatform {
    async fn create_tap(&self, name: &str, ip: Option<IpNet>) -> io::Result<Arc<dyn Tap>> {
        let tap = MockTap::new(name);
        if let Some(addr) = ip {
            tap.state.lock().await.ip = Some(addr);
        }
        self.taps.lock().await.push(tap.clone());
        Ok(tap as Arc<dyn Tap>)
    }

    async fn create_bridge(&self, name: &str, _ip: Option<IpNet>) -> io::Result<Arc<dyn Bridge>> {
        let br = MockBridge::new(name);
        self.bridges.lock().await.push(br.clone());
        Ok(br as Arc<dyn Bridge>)
    }
}
