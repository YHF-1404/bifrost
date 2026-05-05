// TypeScript mirror of the backend's HTTP API types.
// Keep in sync with `crates/bifrost-web/src/api.rs` and
// `crates/bifrost-proto/src/admin.rs::DeviceEntry`.

export interface Network {
  id: string; // net_uuid as string
  name: string;
  bridge_name: string;
  bridge_ip: string;
  device_count: number;
  online_count: number;
  /** True when the network's currently-derived routes (the union of
   *  every admitted device's `lan_subnets`) don't match what was
   *  last broadcast via `device_push`. The WebUI pulses the hub
   *  card's "push routes" button amber while this is true. Updated
   *  in real time via the `routes.dirty` WS event. */
  routes_dirty: boolean;
}

export interface Throughput {
  bps_in: number;
  bps_out: number;
  total_in: number;
  total_out: number;
}

export interface Device {
  client_uuid: string;
  /** Phase 3 — `null` for unassigned (pending pool) clients. */
  net_uuid: string | null;
  display_name: string;
  admitted: boolean;
  tap_ip: string | null;
  lan_subnets: string[];
  online: boolean;
  sid: number | null;
  tap_name: string | null;
  // Phase 1.2 — populated once the metrics sampler ships.
  throughput?: Throughput | null;
}

/** WebSocket event payloads from the Hub. The `device.*` variants
 *  drive query invalidation — see `lib/eventInvalidator.ts`. */
export type ServerEvent =
  | { type: "device.online"; network: string; client_uuid: string; sid: number; tap_name: string }
  | { type: "device.offline"; network: string; client_uuid: string }
  | { type: "device.changed"; network: string; device: Device }
  | { type: "device.pending"; network: string; device: Device }
  | { type: "device.removed"; network: string; client_uuid: string }
  | {
      type: "metrics.tick";
      samples: Array<{ network: string; client_uuid: string } & Throughput>;
    }
  | {
      type: "routes.changed";
      network: string;
      routes: Array<{ dst: string; via: string }>;
      count: number;
    }
  | { type: "routes.dirty"; network: string; dirty: boolean }
  | { type: "network.created"; network: string; name: string }
  | { type: "network.changed"; network: string; name: string }
  | { type: "network.deleted"; network: string };
