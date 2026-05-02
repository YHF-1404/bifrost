//! Linux backend: real `Tap` / `Bridge` / `Platform` implementations.
//!
//! * **`tap`** — `/dev/net/tun` + `TUNSETIFF` ioctl + `AsyncFd<OwnedFd>`
//!   for asynchronous reads/writes; address and route management is
//!   delegated to rtnetlink so we don't fork a `/sbin/ip` per call.
//! * **`bridge`** — pure rtnetlink: `link add … type bridge`,
//!   `link set master`, `link set up`.
//! * **`platform`** — owns one rtnetlink `Handle` and hands out boxed
//!   trait objects.
//!
//! All `unsafe` is contained in [`tap`]; the rest is safe code.

#![allow(unsafe_code)]

mod bridge;
mod platform;
mod tap;

pub use bridge::LinuxBridge;
pub use platform::LinuxPlatform;
pub use tap::LinuxTap;
