//! Hub-emitted events broadcast to local subscribers (WebUI today,
//! Prometheus / pcap dump later).
//!
//! All events serialize as JSON to match the WebUI wire shape:
//!
//! ```json
//! { "type": "metrics.tick", "samples": [ ... ] }
//! ```
//!
//! `serde(tag = "type")` produces this externally-tagged form. Variant
//! names use the same `domain.action` convention as in
//! `web/src/lib/types.ts::ServerEvent`.
//!
//! These are server→client only — `Serialize` is enough. (Embedding a
//! `Deserialize` impl would be strange: events come from the Hub, full
//! stop, and round-tripping JSON in tests is straightforward without
//! deriving it.)

use serde::Serialize;
use uuid::Uuid;

/// One per-session 1 Hz sample of the byte counters in `SessionTask`.
///
/// `bps_in` / `bps_out` are deltas from the previous tick (effectively
/// "bytes per second" since the sampler runs at 1 Hz). `total_in` /
/// `total_out` are the running counters since the session started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricsSample {
    pub network: Uuid,
    pub client_uuid: Uuid,
    pub bps_in: u64,
    pub bps_out: u64,
    pub total_in: u64,
    pub total_out: u64,
}

/// Broadcast over `tokio::sync::broadcast` from the Hub. Subscribers
/// (currently the `/ws` handler in `bifrost-web`) JSON-encode and
/// forward; lagging subscribers drop frames rather than block the Hub.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum HubEvent {
    /// Periodic snapshot of every joined session's byte counters.
    /// Empty `samples` arrays are not emitted.
    #[serde(rename = "metrics.tick")]
    MetricsTick { samples: Vec<MetricsSample> },
    // Reserved for Phase 1.3:
    //   #[serde(rename = "device.online")]
    //   DeviceOnline { network: Uuid, client_uuid: Uuid, sid: u64, tap_name: String },
    //   #[serde(rename = "device.offline")]
    //   DeviceOffline { network: Uuid, client_uuid: Uuid },
    //   #[serde(rename = "device.changed")]
    //   DeviceChanged { network: Uuid, device: DeviceEntry },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_tick_serializes_with_dotted_tag() {
        let nid = Uuid::nil();
        let cid = Uuid::nil();
        let evt = HubEvent::MetricsTick {
            samples: vec![MetricsSample {
                network: nid,
                client_uuid: cid,
                bps_in: 100,
                bps_out: 200,
                total_in: 1000,
                total_out: 2000,
            }],
        };
        let json = serde_json::to_value(&evt).unwrap();
        assert_eq!(json["type"], "metrics.tick");
        assert_eq!(json["samples"][0]["bps_in"], 100);
        assert_eq!(json["samples"][0]["client_uuid"], cid.to_string());
    }
}
