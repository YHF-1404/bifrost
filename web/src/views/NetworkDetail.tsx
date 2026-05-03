// One-network page. Owns the device-list query, all four write
// mutations, and the table/graph view-mode toggle. The body content
// is delegated to <DevicesAsTable /> or <DevicesAsGraph /> — the two
// share the props shape `DeviceViewProps`.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api, type DeviceUpdateBody } from "@/lib/api";
import { fmtErr } from "@/lib/format";
import { pushToast } from "@/lib/toast";
import type { Device, Network } from "@/lib/types";
import { Button } from "@/components/ui/Button";
import { DevicesAsGraph } from "./DevicesAsGraph";
import { DevicesAsTable } from "./DevicesAsTable";

export type ViewMode = "table" | "graph";

export interface DeviceViewProps {
  networkId: string;
  /** Display name for the network — shown on the Hub card in graph
   *  mode. Empty string if not yet loaded. */
  networkName: string;
  devices: Device[];
  onUpdate: (cid: string, body: DeviceUpdateBody) => void;
  /** Rename the current network. Optimistic; `Network.name` updates
   *  on the next `network.changed` event. */
  onRenameNetwork: (name: string) => void;
}

const VIEW_MODE_KEY = "bifrost.viewMode";

function loadViewMode(): ViewMode {
  try {
    const v = localStorage.getItem(VIEW_MODE_KEY);
    return v === "graph" ? "graph" : "table";
  } catch {
    return "table";
  }
}

export function NetworkDetail() {
  const { nid } = useParams<{ nid: string }>();
  const qc = useQueryClient();
  const queryKey = ["devices", nid] as const;

  const q = useQuery({
    queryKey,
    queryFn: () => api.listDevices(nid!),
    refetchInterval: 30_000,
    enabled: !!nid,
  });

  // We also need the network's display name for the Hub card. The
  // networks list is already cached if the user navigated here from
  // the index page; if they deep-linked, we fetch it the same way the
  // index page does.
  const networksQ = useQuery({
    queryKey: ["networks"] as const,
    queryFn: () => api.listNetworks(),
    refetchInterval: 30_000,
  });
  const networkName: string =
    networksQ.data?.find((n: Network) => n.id === nid)?.name ?? "";

  // ── Mutations ─────────────────────────────────────────────────────────

  // True when a `lan_subnets` PATCH has succeeded but the user hasn't
  // hit "Push routes" yet. The server stores the new subnets in its
  // approved_clients table immediately, but peers don't see the
  // derived routes until push. Reset on a successful push. Dropped
  // on full page reload — there's no server-side notion of
  // "pushed vs unpushed", so a per-tab flag is the most we can know.
  const [routesUnpushed, setRoutesUnpushed] = useState(false);

  const updateDevice = useMutation({
    mutationFn: ({ cid, body }: { cid: string; body: DeviceUpdateBody }) =>
      api.updateDevice(nid!, cid, body),
    onMutate: async ({ cid, body }) => {
      await qc.cancelQueries({ queryKey });
      const prev = qc.getQueryData<Device[]>(queryKey);
      qc.setQueryData<Device[]>(queryKey, (old) =>
        old?.map((d) =>
          d.client_uuid === cid
            ? {
                ...d,
                ...(body.name !== undefined ? { display_name: body.name } : {}),
                ...(body.tap_ip !== undefined
                  ? { tap_ip: body.tap_ip === "" ? null : body.tap_ip }
                  : {}),
                ...(body.lan_subnets !== undefined
                  ? { lan_subnets: body.lan_subnets }
                  : {}),
                ...(body.admitted !== undefined ? { admitted: body.admitted } : {}),
              }
            : d,
        ) ?? [],
      );
      return { prev };
    },
    onError: (err, _vars, ctx) => {
      if (ctx?.prev) qc.setQueryData(queryKey, ctx.prev);
      pushToast("error", `update failed: ${fmtErr(err)}`);
    },
    onSuccess: (_data, vars) => {
      // Subnet edits don't reach peers until pushed. Surface that
      // explicitly so the user doesn't wonder why nothing changed.
      if (vars.body.lan_subnets !== undefined) {
        setRoutesUnpushed(true);
        pushToast(
          "info",
          "LAN subnets updated — click 'Push routes' to apply on every peer.",
        );
      }
    },
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const renameNet = useMutation({
    mutationFn: (name: string) => api.renameNetwork(nid!, name),
    onMutate: async (name) => {
      await qc.cancelQueries({ queryKey: ["networks"] });
      const prev = qc.getQueryData<Network[]>(["networks"]);
      qc.setQueryData<Network[]>(["networks"], (old) =>
        old?.map((n) => (n.id === nid ? { ...n, name } : n)) ?? [],
      );
      return { prev };
    },
    onError: (err, _vars, ctx) => {
      if (ctx?.prev) qc.setQueryData(["networks"], ctx.prev);
      pushToast("error", `rename failed: ${fmtErr(err)}`);
    },
    onSettled: () => qc.invalidateQueries({ queryKey: ["networks"] }),
  });

  const [pushing, setPushing] = useState(false);
  const pushRoutes = async () => {
    if (!nid) return;
    setPushing(true);
    try {
      const r = await api.pushRoutes(nid);
      pushToast(
        "success",
        `pushed ${r.routes.length} route(s) to ${r.count} client(s)`,
      );
      setRoutesUnpushed(false);
    } catch (e) {
      pushToast("error", `push failed: ${fmtErr(e)}`);
    } finally {
      setPushing(false);
    }
  };

  // ── View-mode toggle ──────────────────────────────────────────────────

  const [viewMode, setViewMode] = useState<ViewMode>(loadViewMode);
  useEffect(() => {
    try {
      localStorage.setItem(VIEW_MODE_KEY, viewMode);
    } catch {
      // private mode / quota — silently ignore
    }
  }, [viewMode]);

  const childProps: DeviceViewProps | null = nid
    ? {
        networkId: nid,
        networkName,
        devices: q.data ?? [],
        onUpdate: (cid, body) => updateDevice.mutate({ cid, body }),
        onRenameNetwork: (name) => renameNet.mutate(name),
      }
    : null;

  // The graph view wants to fill the viewport. The table view wants
  // a bounded width. The toolbar is always centered. We achieve this
  // with two different containers depending on viewMode.
  const toolbar = (
    <div className="mb-4 flex items-center gap-3 text-sm">
      <Link to="/networks" className="text-muted-foreground hover:underline">
        ← Networks
      </Link>
      <span className="font-mono text-xs text-muted-foreground">{nid}</span>

      <div className="ml-auto flex items-center gap-2">
        <ViewModeToggle value={viewMode} onChange={setViewMode} />
        <Button
          size="sm"
          onClick={pushRoutes}
          disabled={pushing || !q.data?.length}
          // When there are unpushed changes, switch to an amber color
          // and add a soft pulse so the user's eye is drawn here right
          // after they edit a LAN subnet. The toast already told them
          // what to do — this keeps the affordance visible after the
          // toast fades.
          className={
            routesUnpushed
              ? "animate-pulse bg-amber-500 text-white ring-2 ring-amber-400 hover:bg-amber-600"
              : undefined
          }
          title={
            routesUnpushed
              ? "LAN subnets changed — click to push to all peers"
              : "Re-derive routes from LAN subnets and push to all peers"
          }
        >
          {pushing
            ? "pushing…"
            : routesUnpushed
              ? "Push routes •"
              : "Push routes"}
        </Button>
      </div>
    </div>
  );

  const body = q.isLoading ? (
    <div className="text-sm text-muted-foreground">loading…</div>
  ) : q.isError ? (
    <div className="text-sm text-destructive">
      failed to load: {(q.error as Error).message}
    </div>
  ) : !childProps ? null : viewMode === "graph" ? (
    <DevicesAsGraph {...childProps} />
  ) : (
    <DevicesAsTable {...childProps} />
  );

  if (viewMode === "graph") {
    // Full-width column that flex-grows so DevicesAsGraph can fill
    // the remaining viewport.
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <div className="mx-auto w-full max-w-6xl">{toolbar}</div>
        <div className="flex min-h-0 flex-1 flex-col">{body}</div>
      </div>
    );
  }

  return (
    <div className="mx-auto w-full max-w-6xl">
      {toolbar}
      {body}
    </div>
  );
}

function ViewModeToggle(props: {
  value: ViewMode;
  onChange: (v: ViewMode) => void;
}) {
  return (
    <div className="inline-flex rounded-md border border-border p-0.5 text-sm">
      {(["table", "graph"] as const).map((m) => (
        <button
          key={m}
          type="button"
          onClick={() => props.onChange(m)}
          aria-pressed={props.value === m}
          className={
            props.value === m
              ? "rounded bg-primary px-2.5 py-1 text-xs font-medium text-primary-foreground"
              : "rounded px-2.5 py-1 text-xs font-medium text-muted-foreground hover:text-foreground"
          }
        >
          {m === "table" ? "Table" : "Graph"}
        </button>
      ))}
    </div>
  );
}
