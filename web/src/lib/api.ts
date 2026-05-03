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

async function readErrorBody(r: Response): Promise<string> {
  try {
    const ct = r.headers.get("content-type") ?? "";
    if (ct.includes("application/json")) {
      const j = (await r.json()) as { error?: string };
      return j.error ?? `${r.status} ${r.statusText}`;
    }
    return (await r.text()).slice(0, 500) || `${r.status} ${r.statusText}`;
  } catch {
    return `${r.status} ${r.statusText}`;
  }
}

async function getJson<T>(url: string): Promise<T> {
  const r = await fetch(url, { headers: { Accept: "application/json" } });
  if (!r.ok) throw new ApiError(r.status, await readErrorBody(r));
  return (await r.json()) as T;
}

async function sendJson<T>(
  method: "PATCH" | "POST" | "DELETE",
  url: string,
  body?: unknown,
): Promise<T | null> {
  const init: RequestInit = {
    method,
    headers: { Accept: "application/json" },
  };
  if (body !== undefined) {
    init.headers = {
      ...init.headers,
      "Content-Type": "application/json",
    };
    init.body = JSON.stringify(body);
  }
  const r = await fetch(url, init);
  if (!r.ok) throw new ApiError(r.status, await readErrorBody(r));
  // 204 No Content: nothing to parse.
  if (r.status === 204) return null;
  return (await r.json()) as T;
}

export interface DeviceUpdateBody {
  name?: string;
  admitted?: boolean;
  tap_ip?: string;
  lan_subnets?: string[];
}

export interface PushRoutesResp {
  count: number;
  routes: Array<{ dst: string; via: string }>;
}

export const api = {
  listNetworks(): Promise<Network[]> {
    return getJson<Network[]>("/api/networks");
  },
  listDevices(networkId: string): Promise<Device[]> {
    return getJson<Device[]>(`/api/networks/${encodeURIComponent(networkId)}/devices`);
  },
  updateDevice(
    networkId: string,
    clientUuid: string,
    body: DeviceUpdateBody,
  ): Promise<Device> {
    return sendJson<Device>(
      "PATCH",
      `/api/networks/${encodeURIComponent(networkId)}/devices/${encodeURIComponent(clientUuid)}`,
      body,
    ) as Promise<Device>;
  },
  pushRoutes(networkId: string): Promise<PushRoutesResp> {
    return sendJson<PushRoutesResp>(
      "POST",
      `/api/networks/${encodeURIComponent(networkId)}/routes/push`,
    ) as Promise<PushRoutesResp>;
  },
};

export { ApiError };
