// Phase 3 — single-page unified Networks + Devices view.
//
// Layout:
//   ┌── toolbar ──────────────────────────────────┐
//   │ Bifrost  [Table | Graph]  [layout: saved]   │
//   ├──────────────┬──────────────────────────────┤
//   │ Pending      │ Network: alpha               │
//   │ • client A   │   ── client X ── client Y ── │
//   │ • client B   │                              │
//   │              │ Network: beta                │
//   │              │   ── client Z ──             │
//   │              │ + new network                │
//   │ [«]          │                              │
//   └──────────────┴──────────────────────────────┘
//
// The left pane lists *unassigned* clients (Phase 3 pending pool).
// The right pane is one card per virtual network. Drag a client from
// the left pane into a network card to assign it; drag a client from
// one network into another to switch networks. Drop on the left pane
// to detach.
//
// Per spec, every drag clears the dropped client's `admitted` and
// `tap_ip` (B3) — the user must re-admit and re-set IP. That's
// enforced server-side by `assign_client`; the UI just calls it.

import {
  DndContext,
  PointerSensor,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { Panel, PanelGroup, PanelResizeHandle } from "react-resizable-panels";
import { api, type DeviceUpdateBody } from "@/lib/api";
import { fmtErr, isCidr, shortUuid } from "@/lib/format";
import { pushToast } from "@/lib/toast";
import type { Device, Network } from "@/lib/types";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { InlineEdit } from "@/components/InlineEdit";
import { IpSegmentInput, type Prefix } from "@/components/IpSegmentInput";
import { SaveStatusChip } from "@/components/SaveStatusChip";
import { Switch } from "@/components/ui/Switch";
import { ThroughputCell } from "@/components/ThroughputCell";
import { useUiLayout } from "@/lib/useUiLayout";
import { cn } from "@/lib/cn";
import { UnifiedGraphView } from "./UnifiedGraphView";

const DEFAULT_LEFT_RATIO = 33; // 1/3, %
const COLLAPSE_KEY = "bifrost.left-collapsed";
const VIEW_MODE_KEY = "bifrost.viewMode";

type ViewMode = "table" | "graph";

// Helper: pull prefix length out of a "x.x.x.x/p" or empty.
function prefixOf(cidr: string | null | undefined): Prefix | null {
  if (!cidr) return null;
  const m = /\/(\d{1,2})$/.exec(cidr);
  if (!m) return null;
  const p = Number(m[1]);
  return p === 16 || p === 24 ? p : null;
}

export function UnifiedView() {
  const qc = useQueryClient();

  // ── Data ──────────────────────────────────────────────────────────────
  const networksQ = useQuery({
    queryKey: ["networks"] as const,
    queryFn: () => api.listNetworks(),
    refetchInterval: 30_000,
  });
  const clientsQ = useQuery({
    queryKey: ["clients"] as const,
    queryFn: () => api.listClients(),
    refetchInterval: 30_000,
  });

  const networks: Network[] = networksQ.data ?? [];
  const clients: Device[] = clientsQ.data ?? [];

  const pending = clients.filter((c) => c.net_uuid === null);
  const byNet = new Map<string, Device[]>();
  for (const c of clients) {
    if (c.net_uuid) {
      const list = byNet.get(c.net_uuid) ?? [];
      list.push(c);
      byNet.set(c.net_uuid, list);
    }
  }

  // ── Layout state ──────────────────────────────────────────────────────
  const layout = useUiLayout();
  const [leftCollapsed, setLeftCollapsed] = useState<boolean>(() => {
    try {
      return localStorage.getItem(COLLAPSE_KEY) === "1";
    } catch {
      return false;
    }
  });
  // Sync collapse state from persisted layout once it loads.
  useEffect(() => {
    if (layout.isLoading) return;
    if (typeof layout.layout.table.left_collapsed === "boolean") {
      setLeftCollapsed(layout.layout.table.left_collapsed);
    }
  }, [layout.isLoading, layout.layout.table.left_collapsed]);

  // ── View-mode toggle ──────────────────────────────────────────────────
  const [viewMode, setViewMode] = useState<ViewMode>(() => {
    try {
      return localStorage.getItem(VIEW_MODE_KEY) === "graph" ? "graph" : "table";
    } catch {
      return "table";
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(VIEW_MODE_KEY, viewMode);
    } catch {
      /* ignore */
    }
  }, [viewMode]);

  // ── Mutations ─────────────────────────────────────────────────────────
  const assignMut = useMutation({
    mutationFn: ({ cid, nid }: { cid: string; nid: string | null }) =>
      api.assignClient(cid, nid),
    onSuccess: (_d, vars) => {
      qc.invalidateQueries({ queryKey: ["clients"] });
      qc.invalidateQueries({ queryKey: ["networks"] });
      if (vars.nid === null) {
        pushToast("info", "client detached to pending pool");
      } else {
        pushToast(
          "info",
          "client assigned — admit + set TAP IP to bring it online",
        );
      }
    },
    onError: (e) => pushToast("error", `assign failed: ${fmtErr(e)}`),
  });

  const updateAdmittedDeviceMut = useMutation({
    mutationFn: ({
      nid,
      cid,
      body,
    }: {
      nid: string;
      cid: string;
      body: DeviceUpdateBody;
    }) => api.updateDevice(nid, cid, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["clients"] }),
    onError: (e) => pushToast("error", `update failed: ${fmtErr(e)}`),
  });

  const patchPendingMut = useMutation({
    mutationFn: ({
      cid,
      body,
    }: {
      cid: string;
      body: { name?: string; lan_subnets?: string[] };
    }) => api.patchPendingClient(cid, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["clients"] }),
    onError: (e) => pushToast("error", `update failed: ${fmtErr(e)}`),
  });

  const renameNetMut = useMutation({
    mutationFn: ({ nid, name }: { nid: string; name: string }) =>
      api.renameNetwork(nid, name),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["networks"] }),
    onError: (e) => pushToast("error", `rename failed: ${fmtErr(e)}`),
  });

  const setBridgeIpMut = useMutation({
    mutationFn: ({ nid, ip }: { nid: string; ip: string }) =>
      api.patchNetwork(nid, { bridge_ip: ip }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["networks"] });
      qc.invalidateQueries({ queryKey: ["clients"] });
    },
    onError: (e) => pushToast("error", `bridge IP update failed: ${fmtErr(e)}`),
  });

  const createNetMut = useMutation({
    mutationFn: (name: string) => api.createNetwork(name),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["networks"] }),
    onError: (e) => pushToast("error", `create network failed: ${fmtErr(e)}`),
  });

  const deleteNetMut = useMutation({
    mutationFn: (nid: string) => api.deleteNetwork(nid),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["networks"] });
      qc.invalidateQueries({ queryKey: ["clients"] });
      pushToast("info", "network deleted; clients moved to pending pool");
    },
    onError: (e) => pushToast("error", `delete failed: ${fmtErr(e)}`),
  });

  const pushRoutesMut = useMutation({
    mutationFn: (nid: string) => api.pushRoutes(nid),
    onSuccess: (r) =>
      pushToast("success", `pushed ${r.routes.length} route(s) to ${r.count} client(s)`),
    onError: (e) => pushToast("error", `push failed: ${fmtErr(e)}`),
  });

  // ── DnD ───────────────────────────────────────────────────────────────
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
  );
  const onDragEnd = (e: DragEndEvent) => {
    const dragged = e.active?.data?.current as
      | { kind: "client"; cuid: string; from: string | null }
      | undefined;
    const target = e.over?.data?.current as
      | { kind: "net"; nid: string }
      | { kind: "pending" }
      | undefined;
    if (!dragged || !target) return;
    if (target.kind === "net") {
      // Same-net drop: no-op (B3).
      if (dragged.from === target.nid) return;
      assignMut.mutate({ cid: dragged.cuid, nid: target.nid });
    } else {
      // Drop on pending pane.
      if (dragged.from === null) return;
      assignMut.mutate({ cid: dragged.cuid, nid: null });
    }
  };

  // ── Render ────────────────────────────────────────────────────────────
  return (
    <DndContext sensors={sensors} onDragEnd={onDragEnd}>
      <div className="flex min-h-0 flex-1 flex-col">
        <Toolbar
          viewMode={viewMode}
          onViewMode={setViewMode}
          isDirty={layout.isDirty}
          isSaving={layout.isSaving}
        />
        {viewMode === "graph" ? (
          <UnifiedGraphView />
        ) : (
          <PanelGroup
            direction="horizontal"
            className="min-h-0 flex-1"
            onLayout={(sizes) => {
              // Persist as fraction of total (0–1); use the FIRST pane.
              if (leftCollapsed) return;
              const ratio = (sizes[0] ?? 0) / 100;
              if (Number.isFinite(ratio) && ratio > 0) {
                layout.update((prev) => ({
                  ...prev,
                  table: { ...prev.table, left_ratio: ratio },
                }));
              }
            }}
          >
            {!leftCollapsed && (
              <>
                <Panel
                  defaultSize={
                    (layout.layout.table.left_ratio ?? DEFAULT_LEFT_RATIO / 100) * 100
                  }
                  minSize={15}
                  maxSize={60}
                  className="overflow-y-auto"
                >
                  <PendingPane
                    clients={pending}
                    onUpdate={(cid, body) =>
                      patchPendingMut.mutate({ cid, body })
                    }
                  />
                </Panel>
                <PanelResizeHandle className="w-px bg-border hover:bg-primary/50" />
              </>
            )}
            <Panel className="overflow-y-auto" minSize={40}>
              <NetworksPane
                networks={networks}
                byNet={byNet}
                onUpdateDevice={(nid, cid, body) =>
                  updateAdmittedDeviceMut.mutate({ nid, cid, body })
                }
                onRenameNet={(nid, name) => renameNetMut.mutate({ nid, name })}
                onSetBridgeIp={(nid, ip) => setBridgeIpMut.mutate({ nid, ip })}
                onCreateNet={(name) => createNetMut.mutate(name)}
                onDeleteNet={(nid) => deleteNetMut.mutate(nid)}
                onPushRoutes={(nid) => pushRoutesMut.mutate(nid)}
              />
            </Panel>
          </PanelGroup>
        )}
        <CollapseLeftFAB
          collapsed={leftCollapsed}
          onToggle={() => {
            const next = !leftCollapsed;
            setLeftCollapsed(next);
            try {
              localStorage.setItem(COLLAPSE_KEY, next ? "1" : "0");
            } catch {
              /* ignore */
            }
            layout.update((prev) => ({
              ...prev,
              table: { ...prev.table, left_collapsed: next },
            }));
          }}
        />
      </div>
    </DndContext>
  );
}

// ── Toolbar ─────────────────────────────────────────────────────────────

function Toolbar({
  viewMode,
  onViewMode,
  isDirty,
  isSaving,
}: {
  viewMode: ViewMode;
  onViewMode: (m: ViewMode) => void;
  isDirty: boolean;
  isSaving: boolean;
}) {
  return (
    <div className="flex items-center gap-3 border-b border-border bg-background px-3 py-2 text-sm">
      <span className="font-semibold">Bifrost</span>
      <div className="ml-auto flex items-center gap-3">
        <SaveStatusChip isDirty={isDirty} isSaving={isSaving} />
        <ViewModeToggle value={viewMode} onChange={onViewMode} />
      </div>
    </div>
  );
}

function ViewModeToggle({
  value,
  onChange,
}: {
  value: ViewMode;
  onChange: (v: ViewMode) => void;
}) {
  return (
    <div className="inline-flex rounded-md border border-border p-0.5">
      {(["table", "graph"] as const).map((m) => (
        <button
          key={m}
          type="button"
          onClick={() => onChange(m)}
          aria-pressed={value === m}
          className={
            value === m
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

function CollapseLeftFAB({
  collapsed,
  onToggle,
}: {
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      className="fixed bottom-3 left-3 z-20 rounded-full border border-border bg-background px-2 py-1 text-xs shadow-md hover:bg-accent"
      title={collapsed ? "Show pending pane" : "Hide pending pane"}
    >
      {collapsed ? "» pending" : "« hide"}
    </button>
  );
}

// ── Pending pane (left) ─────────────────────────────────────────────────

function PendingPane({
  clients,
  onUpdate,
}: {
  clients: Device[];
  onUpdate: (cid: string, body: { name?: string; lan_subnets?: string[] }) => void;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: "pending",
    data: { kind: "pending" },
  });
  return (
    <div
      ref={setNodeRef}
      className={cn(
        "flex h-full flex-col gap-2 p-3",
        isOver && "bg-amber-50/60",
      )}
    >
      <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        Pending ({clients.length})
      </h2>
      {clients.length === 0 ? (
        <div className="rounded-md border border-dashed border-border p-6 text-center text-xs text-muted-foreground">
          No unassigned clients. Drag here from a network to detach.
        </div>
      ) : (
        clients.map((c) => (
          <PendingClientCard key={c.client_uuid} client={c} onUpdate={onUpdate} />
        ))
      )}
    </div>
  );
}

function PendingClientCard({
  client,
  onUpdate,
}: {
  client: Device;
  onUpdate: (cid: string, body: { name?: string; lan_subnets?: string[] }) => void;
}) {
  const { attributes, listeners, setNodeRef, transform, isDragging } =
    useDraggable({
      id: `client:${client.client_uuid}`,
      data: { kind: "client", cuid: client.client_uuid, from: null },
    });
  const style: React.CSSProperties | undefined = transform
    ? { transform: `translate3d(${transform.x}px, ${transform.y}px, 0)`, zIndex: 50 }
    : undefined;
  return (
    <div
      ref={setNodeRef}
      style={style}
      className={cn(
        "rounded-md border bg-card p-2 text-sm shadow-sm",
        isDragging && "opacity-60",
      )}
    >
      <div className="flex items-center gap-2">
        <button
          type="button"
          {...attributes}
          {...listeners}
          className="cursor-grab text-muted-foreground hover:text-foreground active:cursor-grabbing"
          title="drag to a network to assign"
        >
          ⠿
        </button>
        {client.online ? (
          <Badge variant="success">online</Badge>
        ) : (
          <Badge variant="muted">offline</Badge>
        )}
        <span
          className="ml-auto font-mono text-xs text-muted-foreground"
          title={client.client_uuid}
        >
          {shortUuid(client.client_uuid)}…
        </span>
      </div>
      <div className="mt-2 grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
        <span className="text-xs text-muted-foreground">name</span>
        <InlineEdit
          value={client.display_name}
          placeholder="click to name"
          onCommit={(v) => onUpdate(client.client_uuid, { name: v })}
        />
        <span className="text-xs text-muted-foreground">LAN</span>
        <InlineEdit
          value={client.lan_subnets.join(", ")}
          placeholder="comma-separated CIDRs"
          examplePlaceholder="e.g. 192.168.1.0/24"
          inputClassName="w-full font-mono"
          display={(v) =>
            v === "" ? (
              <span className="text-muted-foreground italic">click to set</span>
            ) : (
              <div className="flex flex-wrap gap-1">
                {v.split(/\s*,\s*/).map((s) => (
                  <Badge key={s} variant="outline" className="font-mono">
                    {s}
                  </Badge>
                ))}
              </div>
            )
          }
          validate={(v) => {
            if (v === "") return null;
            for (const p of v.split(/\s*,\s*/)) {
              if (!isCidr(p)) return `bad CIDR: ${p}`;
            }
            return null;
          }}
          onCommit={(v) => {
            const list = v === "" ? [] : v.split(/\s*,\s*/).filter(Boolean);
            onUpdate(client.client_uuid, { lan_subnets: list });
          }}
        />
      </div>
    </div>
  );
}

// ── Networks pane (right) ───────────────────────────────────────────────

function NetworksPane({
  networks,
  byNet,
  onUpdateDevice,
  onRenameNet,
  onSetBridgeIp,
  onCreateNet,
  onDeleteNet,
  onPushRoutes,
}: {
  networks: Network[];
  byNet: Map<string, Device[]>;
  onUpdateDevice: (nid: string, cid: string, body: DeviceUpdateBody) => void;
  onRenameNet: (nid: string, name: string) => void;
  onSetBridgeIp: (nid: string, ip: string) => void;
  onCreateNet: (name: string) => void;
  onDeleteNet: (nid: string) => void;
  onPushRoutes: (nid: string) => void;
}) {
  const [newName, setNewName] = useState("");
  return (
    <div className="grid auto-rows-min grid-cols-1 gap-3 p-3 xl:grid-cols-2">
      {networks.map((n) => (
        <NetworkCard
          key={n.id}
          network={n}
          devices={byNet.get(n.id) ?? []}
          onUpdateDevice={(cid, body) => onUpdateDevice(n.id, cid, body)}
          onRename={(name) => onRenameNet(n.id, name)}
          onSetBridgeIp={(ip) => onSetBridgeIp(n.id, ip)}
          onDelete={() => onDeleteNet(n.id)}
          onPushRoutes={() => onPushRoutes(n.id)}
        />
      ))}
      <Card className="border-dashed">
        <CardContent className="flex items-center gap-2 py-3">
          <input
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            placeholder="new network name"
            className="flex-1 rounded border border-border bg-background px-2 py-1 text-sm outline-none focus:ring-1 focus:ring-primary"
          />
          <Button
            size="sm"
            onClick={() => {
              const t = newName.trim();
              if (!t) return;
              onCreateNet(t);
              setNewName("");
            }}
          >
            + create
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}

function NetworkCard({
  network,
  devices,
  onUpdateDevice,
  onRename,
  onSetBridgeIp,
  onDelete,
  onPushRoutes,
}: {
  network: Network;
  devices: Device[];
  onUpdateDevice: (cid: string, body: DeviceUpdateBody) => void;
  onRename: (name: string) => void;
  onSetBridgeIp: (ip: string) => void;
  onDelete: () => void;
  onPushRoutes: () => void;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: `net:${network.id}`,
    data: { kind: "net", nid: network.id },
  });
  const bridgePrefix = prefixOf(network.bridge_ip);
  const collisions = useMemo(
    () => devices.map((d) => d.tap_ip ?? "").filter(Boolean),
    [devices],
  );
  return (
    <Card
      ref={setNodeRef}
      className={cn(
        "transition-colors",
        isOver && "ring-2 ring-primary ring-offset-1",
      )}
    >
      <CardHeader className="flex flex-row items-center gap-2">
        <CardTitle className="flex items-center gap-2">
          <InlineEdit
            value={network.name}
            placeholder="(unnamed)"
            onCommit={onRename}
            inputClassName="text-base"
          />
          <Badge variant="muted">{devices.length} dev</Badge>
        </CardTitle>
        <div className="ml-auto flex items-center gap-1.5 text-xs">
          <Button
            size="sm"
            variant="outline"
            onClick={onPushRoutes}
            disabled={devices.length === 0}
            title="Re-derive routes from LAN subnets and push to all peers"
          >
            push routes
          </Button>
          <button
            type="button"
            onClick={() => {
              if (window.confirm(`Delete network "${network.name}"? Clients will be moved to the pending pool.`)) {
                onDelete();
              }
            }}
            className="rounded px-2 py-1 text-destructive hover:bg-destructive/10"
            title="Delete network (clients become pending)"
          >
            ✕
          </button>
        </div>
      </CardHeader>
      <CardContent className="space-y-2">
        <div className="flex items-center gap-2 text-xs">
          <span className="text-muted-foreground">bridge IP</span>
          <IpSegmentInput
            value={network.bridge_ip}
            onCommit={onSetBridgeIp}
            bridgePrefix={null}
            allowPrefixToggle
            placeholder="click to set (e.g. 10.0.0.1/24)"
          />
          <span className="ml-auto font-mono text-muted-foreground">
            br: {network.bridge_name || "-"}
          </span>
        </div>
        {devices.length === 0 ? (
          <div className="rounded-md border border-dashed border-border p-4 text-center text-xs text-muted-foreground">
            Drag a client here to assign.
          </div>
        ) : (
          <ul className="divide-y divide-border">
            {devices.map((d) => (
              <AdmittedClientRow
                key={d.client_uuid}
                client={d}
                bridgePrefix={bridgePrefix}
                bridgeIp={network.bridge_ip}
                netUuid={network.id}
                collisions={collisions.filter((c) => c !== (d.tap_ip ?? ""))}
                onUpdate={(body) => onUpdateDevice(d.client_uuid, body)}
              />
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}

function AdmittedClientRow({
  client,
  bridgePrefix,
  bridgeIp,
  netUuid,
  collisions,
  onUpdate,
}: {
  client: Device;
  bridgePrefix: Prefix | null;
  bridgeIp: string;
  netUuid: string;
  collisions: string[];
  onUpdate: (body: DeviceUpdateBody) => void;
}) {
  const { attributes, listeners, setNodeRef, transform, isDragging } =
    useDraggable({
      id: `client:${client.client_uuid}`,
      data: { kind: "client", cuid: client.client_uuid, from: netUuid },
    });
  const style: React.CSSProperties | undefined = transform
    ? { transform: `translate3d(${transform.x}px, ${transform.y}px, 0)`, zIndex: 50 }
    : undefined;
  return (
    <li
      ref={setNodeRef}
      style={style}
      className={cn(
        "grid grid-cols-[auto_auto_1fr_auto_auto_auto_auto] items-center gap-2 py-2 text-sm",
        isDragging && "opacity-60",
      )}
    >
      <button
        type="button"
        {...attributes}
        {...listeners}
        className="cursor-grab text-muted-foreground hover:text-foreground active:cursor-grabbing"
        title="drag to another network or to pending"
      >
        ⠿
      </button>
      <Switch
        checked={client.admitted}
        onChange={(next) => onUpdate({ admitted: next })}
        label={client.admitted ? "Kick this device" : "Admit this device"}
      />
      <InlineEdit
        value={client.display_name}
        placeholder="click to name"
        onCommit={(v) => onUpdate({ name: v })}
      />
      <IpSegmentInput
        value={client.tap_ip ?? ""}
        bridgePrefix={bridgePrefix}
        pinFromBridge={bridgeIp}
        collisions={collisions}
        onCommit={(v) => onUpdate({ tap_ip: v })}
        placeholder="click to set"
      />
      <InlineEdit
        value={client.lan_subnets.join(", ")}
        placeholder="LAN subnets"
        examplePlaceholder="e.g. 192.168.1.0/24"
        inputClassName="w-48 font-mono"
        display={(v) =>
          v === "" ? (
            <span className="text-muted-foreground italic">LAN</span>
          ) : (
            <div className="flex flex-wrap gap-1">
              {v.split(/\s*,\s*/).map((s) => (
                <Badge key={s} variant="outline" className="font-mono">
                  {s}
                </Badge>
              ))}
            </div>
          )
        }
        validate={(v) => {
          if (v === "") return null;
          for (const p of v.split(/\s*,\s*/)) {
            if (!isCidr(p)) return `bad CIDR: ${p}`;
          }
          return null;
        }}
        onCommit={(v) => {
          const list = v === "" ? [] : v.split(/\s*,\s*/).filter(Boolean);
          onUpdate({ lan_subnets: list });
        }}
      />
      <ThroughputCell
        network={netUuid}
        clientUuid={client.client_uuid}
        online={client.online && client.admitted}
      />
      <span
        className="font-mono text-xs text-muted-foreground"
        title={client.client_uuid}
      >
        {shortUuid(client.client_uuid)}…
      </span>
    </li>
  );
}
