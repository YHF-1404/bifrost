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
  method: "PATCH" | "POST" | "PUT" | "DELETE",
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

/** Phase 3 — single-file UI layout. Replaces the old per-network
 *  `<nid>.json` files; the WebUI does one GET on load and one
 *  debounced PUT after each interaction.
 *
 *  Schema:
 *  - `table.left_ratio`: width of the LEFT pane in [0, 1].
 *  - `table.left_collapsed`: whether the left pane is collapsed.
 *  - `graph.positions`: keyed by `hub:<nid>` / `client:<cuid>`.
 *  - `graph.frames`: keyed by net_uuid string. */
export interface UiLayout {
  table: {
    left_ratio?: number | null;
    left_collapsed?: boolean;
  };
  graph: {
    positions: Record<string, { x: number; y: number }>;
    frames: Record<string, { x: number; y: number; width: number; height: number }>;
  };
}

export interface PatchNetworkBody {
  name?: string;
  bridge_ip?: string;
}

export interface PatchPendingClientBody {
  name?: string;
  lan_subnets?: string[];
}

export const api = {
  listNetworks(): Promise<Network[]> {
    return getJson<Network[]>("/api/networks");
  },
  createNetwork(name: string): Promise<{ id: string; name: string }> {
    return sendJson<{ id: string; name: string }>("POST", "/api/networks", {
      name,
    }) as Promise<{ id: string; name: string }>;
  },
  renameNetwork(networkId: string, name: string): Promise<Network> {
    return sendJson<Network>(
      "PATCH",
      `/api/networks/${encodeURIComponent(networkId)}`,
      { name },
    ) as Promise<Network>;
  },
  patchNetwork(networkId: string, body: PatchNetworkBody): Promise<Network> {
    return sendJson<Network>(
      "PATCH",
      `/api/networks/${encodeURIComponent(networkId)}`,
      body,
    ) as Promise<Network>;
  },
  deleteNetwork(networkId: string): Promise<null> {
    return sendJson<null>(
      "DELETE",
      `/api/networks/${encodeURIComponent(networkId)}`,
    ) as Promise<null>;
  },
  listDevices(networkId: string): Promise<Device[]> {
    return getJson<Device[]>(`/api/networks/${encodeURIComponent(networkId)}/devices`);
  },
  /** Phase 3 — every known client (admitted + pending), one shot. */
  listClients(): Promise<Device[]> {
    return getJson<Device[]>("/api/clients");
  },
  /** Phase 3 — edit name / lan_subnets of a pending (unassigned) client. */
  patchPendingClient(
    clientUuid: string,
    body: PatchPendingClientBody,
  ): Promise<Device> {
    return sendJson<Device>(
      "PATCH",
      `/api/clients/${encodeURIComponent(clientUuid)}`,
      body,
    ) as Promise<Device>;
  },
  /** Phase 3 — drag-to-assign. `netUuid = null` detaches to pending pool. */
  assignClient(clientUuid: string, netUuid: string | null): Promise<Device> {
    return sendJson<Device>(
      "POST",
      `/api/clients/${encodeURIComponent(clientUuid)}/assign`,
      { net_uuid: netUuid },
    ) as Promise<Device>;
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
  /** Phase 3 — single-file UI layout (replaces per-network layouts). */
  getUiLayout(): Promise<UiLayout> {
    return getJson<UiLayout>("/api/ui-layout");
  },
  putUiLayout(layout: UiLayout): Promise<null> {
    return sendJson<null>("PUT", "/api/ui-layout", layout) as Promise<null>;
  },
};

export { ApiError };
