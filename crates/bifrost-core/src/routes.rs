//! Route table derivation.
//!
//! The server stops persisting a manually-edited `routes` table. Instead,
//! it derives the route table for each network from the per-client
//! `lan_subnets` field on every `ApprovedClient`:
//!
//! ```text
//!   for each admitted client in network N with a non-empty tap_ip:
//!     for each lan_subnet in client.lan_subnets:
//!       route { dst: lan_subnet, via: tap_ip.addr() }
//! ```
//!
//! Duplicates (same `dst`) are deduped, first-write-wins. Subnets that
//! contain the `via` address itself are dropped (would form a loop).
//!
//! The result is consumed by:
//!
//! * `Bridge::apply_routes` — installs these into the host's kernel
//!   routing table so traffic from the server toward LANs behind a
//!   client actually finds its way out.
//! * `Frame::SetRoutes` — pushed to each joined client so they install
//!   the same table on their TAP (minus self-loops).

use std::collections::HashSet;
use std::str::FromStr;

use bifrost_proto::RouteEntry as WireRoute;
use ipnet::IpNet;
use tracing::warn;
use uuid::Uuid;

use crate::config::ServerConfig;

/// Derive the route table for a single network.
///
/// Returns wire-format `{dst, via}` strings — the same shape used both
/// for `Frame::SetRoutes` and for parsing into `bifrost_net::RouteEntry`.
pub fn derive_routes_for_network(cfg: &ServerConfig, nid: Uuid) -> Vec<WireRoute> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for ac in cfg.approved_clients.iter().filter(|c| c.net_uuid == nid) {
        // Strip the prefix off `tap_ip` to get just the bare address —
        // that's what goes in `via`. Skip clients without an IP.
        let Ok(tap_net) = IpNet::from_str(&ac.tap_ip) else {
            continue;
        };
        let via_addr = tap_net.addr();
        let via_str = via_addr.to_string();

        for subnet in &ac.lan_subnets {
            // Validate; warn and skip malformed entries.
            let Ok(dst_net) = IpNet::from_str(subnet) else {
                warn!(
                    client = %ac.client_uuid,
                    %subnet,
                    "skip invalid lan_subnet"
                );
                continue;
            };
            // Drop self-loops: subnet contains its own `via`.
            if dst_net.contains(&via_addr) {
                continue;
            }
            // First-write-wins on dst.
            if !seen.insert(subnet.clone()) {
                continue;
            }
            out.push(WireRoute {
                dst: subnet.clone(),
                via: via_str.clone(),
            });
        }
    }
    out
}

/// Filter a derived route table for a specific peer: drop routes whose
/// `via` equals the peer's own TAP IP (would loop back through itself).
pub fn filter_for_peer(routes: &[WireRoute], peer_tap_ip: Option<&str>) -> Vec<WireRoute> {
    let host = peer_tap_ip
        .and_then(|s| s.split('/').next())
        .filter(|s| !s.is_empty());
    routes
        .iter()
        .filter(|r| host.is_none_or(|h| r.via != h))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApprovedClient;

    fn cfg_with(clients: Vec<ApprovedClient>) -> ServerConfig {
        ServerConfig {
            approved_clients: clients,
            ..ServerConfig::default()
        }
    }

    #[test]
    fn derives_one_route_per_subnet() {
        let net = Uuid::new_v4();
        let client = Uuid::new_v4();
        let cfg = cfg_with(vec![ApprovedClient {
            client_uuid: client,
            net_uuid: net,
            tap_ip: "10.0.0.2/24".into(),
            display_name: String::new(),
            lan_subnets: vec!["192.168.10.0/24".into(), "192.168.20.0/24".into()],
        }]);
        let routes = derive_routes_for_network(&cfg, net);
        assert_eq!(routes.len(), 2);
        assert!(routes.iter().all(|r| r.via == "10.0.0.2"));
    }

    #[test]
    fn skips_clients_without_ip() {
        let net = Uuid::new_v4();
        let cfg = cfg_with(vec![ApprovedClient {
            client_uuid: Uuid::new_v4(),
            net_uuid: net,
            tap_ip: String::new(),
            display_name: String::new(),
            lan_subnets: vec!["192.168.10.0/24".into()],
        }]);
        assert!(derive_routes_for_network(&cfg, net).is_empty());
    }

    #[test]
    fn skips_other_networks() {
        let net_a = Uuid::new_v4();
        let net_b = Uuid::new_v4();
        let cfg = cfg_with(vec![ApprovedClient {
            client_uuid: Uuid::new_v4(),
            net_uuid: net_b,
            tap_ip: "10.0.0.2/24".into(),
            display_name: String::new(),
            lan_subnets: vec!["192.168.10.0/24".into()],
        }]);
        assert!(derive_routes_for_network(&cfg, net_a).is_empty());
    }

    #[test]
    fn dedupes_dst_first_wins() {
        let net = Uuid::new_v4();
        let cfg = cfg_with(vec![
            ApprovedClient {
                client_uuid: Uuid::new_v4(),
                net_uuid: net,
                tap_ip: "10.0.0.2/24".into(),
                display_name: String::new(),
                lan_subnets: vec!["192.168.10.0/24".into()],
            },
            ApprovedClient {
                client_uuid: Uuid::new_v4(),
                net_uuid: net,
                tap_ip: "10.0.0.3/24".into(),
                display_name: String::new(),
                lan_subnets: vec!["192.168.10.0/24".into()],
            },
        ]);
        let routes = derive_routes_for_network(&cfg, net);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].via, "10.0.0.2");
    }

    #[test]
    fn drops_self_loop() {
        let net = Uuid::new_v4();
        let cfg = cfg_with(vec![ApprovedClient {
            client_uuid: Uuid::new_v4(),
            net_uuid: net,
            tap_ip: "10.0.0.2/24".into(),
            display_name: String::new(),
            // 10.0.0.0/24 contains 10.0.0.2 — the via is inside dst.
            lan_subnets: vec!["10.0.0.0/24".into()],
        }]);
        assert!(derive_routes_for_network(&cfg, net).is_empty());
    }

    #[test]
    fn warn_on_invalid_subnet_but_continue() {
        let net = Uuid::new_v4();
        let cfg = cfg_with(vec![ApprovedClient {
            client_uuid: Uuid::new_v4(),
            net_uuid: net,
            tap_ip: "10.0.0.2/24".into(),
            display_name: String::new(),
            lan_subnets: vec!["not-a-cidr".into(), "192.168.10.0/24".into()],
        }]);
        let routes = derive_routes_for_network(&cfg, net);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].dst, "192.168.10.0/24");
    }

    #[test]
    fn filter_drops_routes_via_self() {
        let routes = vec![
            WireRoute {
                dst: "192.168.10.0/24".into(),
                via: "10.0.0.2".into(),
            },
            WireRoute {
                dst: "192.168.20.0/24".into(),
                via: "10.0.0.3".into(),
            },
        ];
        let filtered = filter_for_peer(&routes, Some("10.0.0.2/24"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].via, "10.0.0.3");
    }
}
