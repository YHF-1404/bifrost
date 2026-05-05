//! Bifrost — platform abstraction for TAP devices and L2 bridges.
//!
//! This crate is **interface-only**: real backends (Linux netlink + ioctl,
//! macOS utun, Windows wintun, …) are added later behind `#[cfg(target_os
//! = …)]`. P0 ships only the traits plus an in-memory mock used by the
//! downstream `bifrost-core` test suite.
//!
//! Consumers depend on this crate through the **trait objects**:
//!
//! ```ignore
//! use std::sync::Arc;
//! use bifrost_net::{Platform, Tap};
//!
//! async fn make_tap(p: &dyn Platform) -> std::io::Result<Arc<dyn Tap>> {
//!     p.create_tap("tap-test", None).await
//! }
//! ```

// `linux` backend uses ioctl for TUNSETIFF, which requires `unsafe`;
// every other module is forbidden from using it.
#![deny(unsafe_code)]

pub mod null;
pub mod traits;
pub mod types;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

#[cfg(target_os = "linux")]
pub mod linux;

pub use ipnet::IpNet;
pub use null::NullPlatform;
pub use traits::{Bridge, Platform, Tap};
pub use types::{ParseError, RouteEntry};

#[cfg(target_os = "linux")]
pub use linux::{set_send_buffer_size, LinuxPlatform};
