// Thin wrappers around the bifrost-server HTTP API. URLs are
// relative — Vite's dev proxy forwards them to the backend; in
// production the SPA is served from the same origin as /api.

import type { Device, Network } from "./types";

class ApiError extends Error {
  constructor(public readonly status: number, message: string) {
    super(message);
    this.name = "ApiError";
  }
}

async function getJson<T>(url: string): Promise<T> {
  const r = await fetch(url, { headers: { Accept: "application/json" } });
  if (!r.ok) {
    let body = "";
    try {
      body = (await r.text()).slice(0, 500);
    } catch {
      // ignore
    }
    throw new ApiError(r.status, `${r.status} ${r.statusText}: ${body}`);
  }
  return (await r.json()) as T;
}

export const api = {
  listNetworks(): Promise<Network[]> {
    return getJson<Network[]>("/api/networks");
  },
  listDevices(networkId: string): Promise<Device[]> {
    return getJson<Device[]>(`/api/networks/${encodeURIComponent(networkId)}/devices`);
  },
};

export { ApiError };
