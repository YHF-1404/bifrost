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
import type { Device } from "@/lib/types";
import { Button } from "@/components/ui/Button";
import { DevicesAsGraph } from "./DevicesAsGraph";
import { DevicesAsTable } from "./DevicesAsTable";

export type ViewMode = "table" | "graph";

export interface DeviceViewProps {
  networkId: string;
  devices: Device[];
  onUpdate: (cid: string, body: DeviceUpdateBody) => void;
  onApprove: (cid: string) => void;
  onDeny: (cid: string) => void;
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

  // ── Mutations ─────────────────────────────────────────────────────────

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
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const approveDevice = useMutation({
    mutationFn: (cid: string) => api.approveDevice(nid!, cid),
    onSuccess: () => pushToast("success", "device admitted"),
    onError: (e) => pushToast("error", `approve failed: ${fmtErr(e)}`),
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const denyDevice = useMutation({
    mutationFn: (cid: string) => api.denyDevice(nid!, cid),
    onSuccess: () => pushToast("info", "device denied"),
    onError: (e) => pushToast("error", `deny failed: ${fmtErr(e)}`),
    onSettled: () => qc.invalidateQueries({ queryKey }),
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
        devices: q.data ?? [],
        onUpdate: (cid, body) => updateDevice.mutate({ cid, body }),
        onApprove: (cid) => approveDevice.mutate(cid),
        onDeny: (cid) => denyDevice.mutate(cid),
      }
    : null;

  return (
    <div className="mx-auto max-w-6xl">
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
          >
            {pushing ? "pushing…" : "Push routes"}
          </Button>
        </div>
      </div>

      {q.isLoading ? (
        <div className="text-sm text-muted-foreground">loading…</div>
      ) : q.isError ? (
        <div className="text-sm text-destructive">
          failed to load: {(q.error as Error).message}
        </div>
      ) : !childProps ? null : viewMode === "graph" ? (
        <DevicesAsGraph {...childProps} />
      ) : (
        <DevicesAsTable {...childProps} />
      )}
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
