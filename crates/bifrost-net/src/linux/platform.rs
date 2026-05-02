//! `LinuxPlatform` — owns one rtnetlink connection and hands out
//! [`Tap`](crate::traits::Tap) / [`Bridge`](crate::traits::Bridge)
//! trait objects backed by [`super::LinuxTap`] / [`super::LinuxBridge`].

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::IpNet;
use rtnetlink::Handle;

use crate::traits::{Bridge, Platform, Tap};

use super::{tap::io_other, LinuxBridge, LinuxTap};

pub struct LinuxPlatform {
    handle: Handle,
}

impl LinuxPlatform {
    /// Spawn an rtnetlink connection task on the current tokio runtime
    /// and return a platform handle. Must be called inside a runtime.
    pub fn new() -> io::Result<Self> {
        let (connection, handle, _messages) =
            rtnetlink::new_connection().map_err(io_other)?;
        tokio::spawn(connection);
        Ok(Self { handle })
    }
}

#[async_trait]
impl Platform for LinuxPlatform {
    async fn create_tap(&self, name: &str, ip: Option<IpNet>) -> io::Result<Arc<dyn Tap>> {
        let tap = LinuxTap::create(name, ip, self.handle.clone()).await?;
        Ok(tap as Arc<dyn Tap>)
    }

    async fn create_bridge(&self, name: &str, ip: Option<IpNet>) -> io::Result<Arc<dyn Bridge>> {
        let br = LinuxBridge::create(name, ip, self.handle.clone()).await?;
        Ok(br as Arc<dyn Bridge>)
    }
}
