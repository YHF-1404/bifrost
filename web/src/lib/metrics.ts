// Per-device throughput store driven by `metrics.tick` WS events.
//
// Lives outside React; consumed via `useSyncExternalStore`. Why?
//   * 1 Hz × N devices shouldn't propagate through a Provider tree.
//   * The data is conceptually global — every Sparkline reads from
//     the same source.
//
// Storage: a Map keyed by `${networkId}:${clientUuid}`. Each entry
// keeps the last `MAX_HISTORY` samples (60 = 1 min at 1 Hz). Samples
// are appended in order; consumers read snapshots, never the live
// buffer.
//
// Garbage collection: on each tick we age out devices not mentioned
// in the latest tick AND not seen for >GC_GRACE_MS — when a device
// disconnects, the Hub stops including it, so its history goes
// stale and we drop it.

import { useSyncExternalStore } from "react";
import { sharedWS } from "./wsClient";
import type { ServerEvent, Throughput } from "./types";

export interface MetricSample extends Throughput {
  /** Wall-clock ms when this sample was received. */
  ts: number;
}

export interface DeviceMetrics {
  samples: MetricSample[];
  lastSeen: number;
}

const MAX_HISTORY = 60;
const GC_GRACE_MS = 30_000;

function key(network: string, clientUuid: string): string {
  return `${network}:${clientUuid}`;
}

class MetricsStore {
  private byKey = new Map<string, DeviceMetrics>();
  private listeners = new Set<() => void>();
  /** Cache of stable references handed to React. Cleared on every tick. */
  private snapshot = new Map<string, DeviceMetrics>();

  constructor() {
    sharedWS().onEvent((evt: ServerEvent) => {
      if (evt.type !== "metrics.tick") return;
      const now = Date.now();
      const seen = new Set<string>();
      for (const s of evt.samples) {
        const k = key(s.network, s.client_uuid);
        seen.add(k);
        const entry = this.byKey.get(k) ?? { samples: [], lastSeen: now };
        entry.lastSeen = now;
        entry.samples.push({
          ts: now,
          bps_in: s.bps_in,
          bps_out: s.bps_out,
          total_in: s.total_in,
          total_out: s.total_out,
        });
        if (entry.samples.length > MAX_HISTORY) {
          entry.samples.splice(0, entry.samples.length - MAX_HISTORY);
        }
        this.byKey.set(k, entry);
      }
      for (const [k, entry] of this.byKey) {
        if (!seen.has(k) && now - entry.lastSeen > GC_GRACE_MS) {
          this.byKey.delete(k);
        }
      }
      this.snapshot = new Map();
      for (const fn of this.listeners) fn();
    });
  }

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  get(network: string, clientUuid: string): DeviceMetrics | undefined {
    const k = key(network, clientUuid);
    const cached = this.snapshot.get(k);
    if (cached) return cached;
    const live = this.byKey.get(k);
    if (!live) return undefined;
    // Freeze a shallow copy so React's identity check is stable
    // until the next tick rebuilds the snapshot.
    const frozen: DeviceMetrics = {
      samples: live.samples.slice(),
      lastSeen: live.lastSeen,
    };
    this.snapshot.set(k, frozen);
    return frozen;
  }
}

let store: MetricsStore | null = null;
function getStore(): MetricsStore {
  if (!store) store = new MetricsStore();
  return store;
}

/** Subscribe a component to one device's metrics. Re-renders on every
 *  new sample for that device. Returns `undefined` until the first
 *  tick arrives. */
export function useDeviceMetrics(
  network: string,
  clientUuid: string,
): DeviceMetrics | undefined {
  const s = getStore();
  return useSyncExternalStore(
    s.subscribe,
    () => s.get(network, clientUuid),
    () => undefined,
  );
}
