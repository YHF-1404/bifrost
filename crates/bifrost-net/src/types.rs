use std::net::IpAddr;

use ipnet::IpNet;
use thiserror::Error;

/// Validated routing-table row.
///
/// Distinct from [`bifrost_proto::RouteEntry`], which uses strings on the
/// wire. The boundary between the two lives in `bifrost-core` so this
/// crate stays free of any wire-protocol concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    pub dst: IpNet,
    pub via: IpAddr,
}

impl RouteEntry {
    /// Parse a destination CIDR and gateway IP from their string forms.
    ///
    /// ```
    /// use bifrost_net::RouteEntry;
    /// let r = RouteEntry::parse("192.168.10.0/24", "10.0.0.1").unwrap();
    /// assert_eq!(r.via.to_string(), "10.0.0.1");
    /// ```
    pub fn parse(dst: &str, via: &str) -> Result<Self, ParseError> {
        let dst = dst
            .parse::<IpNet>()
            .map_err(|_| ParseError::BadCidr(dst.to_owned()))?;
        let via = via
            .parse::<IpAddr>()
            .map_err(|_| ParseError::BadIp(via.to_owned()))?;
        Ok(Self { dst, via })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("invalid CIDR: {0}")]
    BadCidr(String),
    #[error("invalid IP address: {0}")]
    BadIp(String),
}
