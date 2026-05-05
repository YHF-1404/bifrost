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

use bifrost_proto::admin::DeviceEntry;
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

/// One row of a derived route table, in the shape the WebUI consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteRow {
    pub dst: String,
    pub via: String,
}

/// Broadcast over `tokio::sync::broadcast` from the Hub. Subscribers
/// (currently the `/ws` handler in `bifrost-web`) JSON-encode and
/// forward; lagging subscribers drop frames rather than block the Hub.
///
/// All variants carry the `network` UUID so a multi-tab WebUI can
/// scope its query invalidations.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum HubEvent {
    /// Periodic snapshot of every joined session's byte counters.
    /// Empty `samples` arrays are not emitted.
    #[serde(rename = "metrics.tick")]
    MetricsTick { samples: Vec<MetricsSample> },

    /// A SessionTask just started, or a returning client re-bound an
    /// existing session.
    #[serde(rename = "device.online")]
    DeviceOnline {
        network: Uuid,
        client_uuid: Uuid,
        sid: u64,
        tap_name: String,
    },

    /// A SessionTask finished — disconnect-timeout, kill, or TAP error.
    /// Note: the merely-unbound state (conn dropped but session alive)
    /// does NOT fire this; the device is still "online" by the WebUI's
    /// definition until its TAP is destroyed.
    #[serde(rename = "device.offline")]
    DeviceOffline {
        network: Uuid,
        client_uuid: Uuid,
    },

    /// A new join request landed in `pending` — admin must approve.
    #[serde(rename = "device.pending")]
    DevicePending {
        network: Uuid,
        device: DeviceEntry,
    },

    /// One device's persistent fields (name, tap_ip, lan_subnets) or
    /// runtime state (online, sid) changed. The full record is
    /// included so subscribers can replace their cached row with no
    /// extra round-trip.
    #[serde(rename = "device.changed")]
    DeviceChanged {
        network: Uuid,
        device: DeviceEntry,
    },

    /// A device's `approved_clients` row was removed — either via
    /// `device_set { admitted: false }` (kick) or `deny` of a pending
    /// session.
    #[serde(rename = "device.removed")]
    DeviceRemoved {
        network: Uuid,
        client_uuid: Uuid,
    },

    /// The route table for a network was just (re-)derived and pushed.
    /// `count` is how many bound peers received the push.
    #[serde(rename = "routes.changed")]
    RoutesChanged {
        network: Uuid,
        routes: Vec<RouteRow>,
        count: u64,
    },

    /// The set of routes derived from `lan_subnets` changed and no
    /// longer matches what was last pushed to peers (`dirty=true`),
    /// or it just got back into sync (`dirty=false`). Subscribers use
    /// this to nudge admins to click "push routes" — typically by
    /// pulsing the button amber. The hub emits an event only on
    /// state TRANSITIONS (false → true or true → false).
    #[serde(rename = "routes.dirty")]
    RoutesDirty {
        network: Uuid,
        dirty: bool,
    },

    /// A new virtual network was created.
    #[serde(rename = "network.created")]
    NetworkCreated {
        network: Uuid,
        name: String,
    },

    /// A network's metadata changed (name only, today).
    #[serde(rename = "network.changed")]
    NetworkChanged {
        network: Uuid,
        name: String,
    },

    /// A network was deleted, cascading to all its devices.
    #[serde(rename = "network.deleted")]
    NetworkDeleted {
        network: Uuid,
    },
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

    #[test]
    fn device_online_serializes() {
        let evt = HubEvent::DeviceOnline {
            network: Uuid::nil(),
            client_uuid: Uuid::nil(),
            sid: 42,
            tap_name: "tap0a1b2c3d".into(),
        };
        let json = serde_json::to_value(&evt).unwrap();
        assert_eq!(json["type"], "device.online");
        assert_eq!(json["sid"], 42);
        assert_eq!(json["tap_name"], "tap0a1b2c3d");
    }

    #[test]
    fn routes_changed_serializes() {
        let evt = HubEvent::RoutesChanged {
            network: Uuid::nil(),
            routes: vec![RouteRow {
                dst: "192.168.10.0/24".into(),
                via: "10.0.0.5".into(),
            }],
            count: 3,
        };
        let json = serde_json::to_value(&evt).unwrap();
        assert_eq!(json["type"], "routes.changed");
        assert_eq!(json["routes"][0]["dst"], "192.168.10.0/24");
        assert_eq!(json["count"], 3);
    }
}
