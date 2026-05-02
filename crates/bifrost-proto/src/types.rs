use serde::{Deserialize, Serialize};

/// Capability bit flags exchanged in [`crate::Frame::Hello`] and
/// [`crate::Frame::HelloAck`].
///
/// All bits are **reserved** in v1; senders MUST emit `0`, receivers MUST
/// ignore unknown bits. Listed here so future versions can negotiate
/// transport-layer upgrades without another protocol revision.
pub mod caps {
    /// Future Noise-XX wrapped transport.
    pub const NOISE: u32 = 1 << 0;
    /// Future TLS (rustls) wrapped transport.
    pub const TLS: u32 = 1 << 1;
    /// Live pcap streaming over the control channel (future).
    pub const PCAP_STREAM: u32 = 1 << 2;
}

/// One row of a routing table.
///
/// Strings are kept as-is on the wire so this crate stays free of
/// `ipnet`/`std::net` dependencies; consumers parse and validate them at
/// the use site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteEntry {
    /// Destination network in CIDR form, e.g. `"192.168.10.0/24"`.
    pub dst: String,
    /// Gateway IP address, e.g. `"10.0.0.1"`.
    pub via: String,
}
