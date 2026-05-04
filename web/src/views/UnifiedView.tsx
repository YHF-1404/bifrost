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
  DragOverlay,
  PointerSensor,
  pointerWithin,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type ImperativePanelHandle,
  Panel,
  PanelGroup,
  PanelResizeHandle,
} from "react-resizable-panels";
import { api, type DeviceUpdateBody } from "@/lib/api";
import { fmtErr, isCidr, shortUuid } from "@/lib/format";
import { pushToast } from "@/lib/toast";
import { useWSStatus } from "@/lib/useWS";
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

const DEFAULT_LEFT_RATIO = 0.33; // 1/3
const VIEW_MODE_KEY = "bifrost.viewMode";

// Shared grid template for the per-network device list. Same template
// is used by both the column-header strip and every row, so columns
// line up across rows even when content widths vary (e.g. one row has
// 5 LAN subnets and another has none). Min widths keep the cells
// stable; the name and LAN columns are flexible.
//
// TAP IP column is 232 px so the editing-mode picker (4 × w-12 octet
// inputs + dot separators + "/24") fits comfortably without spilling
// into the LAN column. THROUGHPUT column is 192 px so the value
// (w-20 = 80 px) + 80-px sparkline + triangle/gaps stay on one line
// at any byte rate up to "99.9 GB/s".
const ROW_COLS =
  "grid grid-cols-[20px_40px_minmax(120px,1fr)_232px_minmax(160px,1.5fr)_192px_64px] items-center gap-2";

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
  // Phase 3 — use react-resizable-panels' Imperative collapse handle.
  // The Panel stays mounted across collapse/expand so its dragged
  // size survives a round-trip (fixes the "expand width drifts every
  // click" issue from Phase 3.0e).
  const leftPanelRef = useRef<ImperativePanelHandle | null>(null);
  const [leftCollapsed, setLeftCollapsed] = useState<boolean>(false);
  // Sync collapse state from persisted layout the first time it loads.
  const persistedCollapseApplied = useRef(false);
  useEffect(() => {
    if (layout.isLoading || persistedCollapseApplied.current) return;
    persistedCollapseApplied.current = true;
    const persisted = layout.layout.table.left_collapsed === true;
    setLeftCollapsed(persisted);
    if (persisted) {
      leftPanelRef.current?.collapse();
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
  // Phase 3 — assign is the high-frequency drag-end mutation, so it
  // gets full optimistic-update treatment: we rewrite the cached
  // `clients` array immediately in `onMutate` so the dragged card
  // appears in its new home the instant the user drops it (instead
  // of bouncing back to the source pane until the round-trip lands).
  const assignMut = useMutation({
    mutationFn: ({ cid, nid }: { cid: string; nid: string | null }) =>
      api.assignClient(cid, nid),
    onMutate: async ({ cid, nid }) => {
      await qc.cancelQueries({ queryKey: ["clients"] });
      const prev = qc.getQueryData<Device[]>(["clients"]);
      qc.setQueryData<Device[]>(["clients"], (old) =>
        (old ?? []).map((d) =>
          d.client_uuid === cid
            ? {
                ...d,
                net_uuid: nid,
                admitted: false,
                tap_ip: null,
              }
            : d,
        ),
      );
      return { prev };
    },
    onError: (e, _vars, ctx) => {
      if (ctx?.prev) qc.setQueryData(["clients"], ctx.prev);
      pushToast("error", `assign failed: ${fmtErr(e)}`);
    },
    onSettled: () => {
      qc.invalidateQueries({ queryKey: ["clients"] });
      qc.invalidateQueries({ queryKey: ["networks"] });
    },
    onSuccess: (_d, vars) => {
      if (vars.nid === null) {
        pushToast("info", "client detached to pending pool");
      } else {
        pushToast(
          "info",
          "client assigned — admit + set TAP IP to bring it online",
        );
      }
    },
  });

  // Networks whose lan_subnets have been edited since the last push.
  // The card's push button pulses amber while a net is here.
  const [pendingPush, setPendingPush] = useState<Set<string>>(new Set());
  const markPending = useCallback((nid: string) => {
    setPendingPush((prev) => {
      if (prev.has(nid)) return prev;
      const next = new Set(prev);
      next.add(nid);
      return next;
    });
    pushToast(
      "info",
      "LAN subnets updated — click 'push routes' on the network card.",
    );
  }, []);

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
    onSuccess: (_d, vars) => {
      qc.invalidateQueries({ queryKey: ["clients"] });
      if (vars.body.lan_subnets !== undefined) markPending(vars.nid);
    },
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
    onSuccess: (r, nid) => {
      pushToast(
        "success",
        `pushed ${r.routes.length} route(s) to ${r.count} client(s)`,
      );
      setPendingPush((prev) => {
        if (!prev.has(nid)) return prev;
        const next = new Set(prev);
        next.delete(nid);
        return next;
      });
    },
    onError: (e) => pushToast("error", `push failed: ${fmtErr(e)}`),
  });

  // ── DnD ───────────────────────────────────────────────────────────────
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
  );
  // Phase 3 — track the current drag victim so <DragOverlay> can
  // render a portal-mounted preview that survives stacking-context
  // boundaries (the resize panes used to clip it at the divider).
  const [activeDrag, setActiveDrag] = useState<Device | null>(null);
  const onDragStart = (e: DragStartEvent) => {
    const d = e.active?.data?.current as
      | { kind: "client"; cuid: string }
      | undefined;
    if (!d) return;
    const c = clients.find((c) => c.client_uuid === d.cuid) ?? null;
    setActiveDrag(c);
  };
  const onDragEnd = (e: DragEndEvent) => {
    setActiveDrag(null);
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

  // Persist the divider position only when the user is *actively
  // dragging* it — the first onLayout fires on mount with the same
  // value we just supplied as defaultSize, which would otherwise
  // round-trip and accumulate drift across sessions.
  const isFirstLayoutCall = useRef(true);

  // ── Render ────────────────────────────────────────────────────────────
  const wsStatus = useWSStatus();
  return (
    <DndContext
      sensors={sensors}
      // Use pointer-position (not rect-intersection) so the drop is
      // decided purely by where the cursor is. The DragOverlay preview
      // is ~200 px wide; with rectIntersection a drop near the top of
      // the narrow PENDING pane mostly overlapped the wider Networks
      // pane and got mis-routed there. pointerWithin makes "release
      // here" do exactly what the user expects.
      collisionDetection={pointerWithin}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragCancel={() => setActiveDrag(null)}
    >
      <div className="flex min-h-0 flex-1 flex-col">
        <Toolbar
          viewMode={viewMode}
          onViewMode={setViewMode}
          isDirty={layout.isDirty}
          isSaving={layout.isSaving}
          wsStatus={wsStatus}
        />
        {viewMode === "graph" ? (
          <UnifiedGraphView />
        ) : (
          <PanelGroup
            direction="horizontal"
            className="min-h-0 flex-1"
            onLayout={(sizes) => {
              if (isFirstLayoutCall.current) {
                isFirstLayoutCall.current = false;
                return;
              }
              if (leftCollapsed) return;
              const ratio = (sizes[0] ?? 0) / 100;
              if (Number.isFinite(ratio) && ratio > 0.001) {
                layout.update((prev) => ({
                  ...prev,
                  table: { ...prev.table, left_ratio: ratio },
                }));
              }
            }}
          >
            <Panel
              ref={leftPanelRef}
              id="pending"
              order={1}
              collapsible
              collapsedSize={0}
              defaultSize={
                (layout.layout.table.left_ratio ?? DEFAULT_LEFT_RATIO) * 100
              }
              minSize={15}
              maxSize={60}
              onCollapse={() => setLeftCollapsed(true)}
              onExpand={() => setLeftCollapsed(false)}
              className="overflow-hidden"
            >
              <PendingPane
                clients={pending}
                onUpdate={(cid, body) =>
                  patchPendingMut.mutate({ cid, body })
                }
              />
            </Panel>
            <PanelResizeHandle className="w-1.5 bg-border transition-colors hover:bg-primary/50 data-[resize-handle-active]:bg-primary" />
            <Panel id="networks" order={2} className="overflow-y-auto" minSize={40}>
              <NetworksPane
                networks={networks}
                byNet={byNet}
                pendingPush={pendingPush}
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
            // Drive the panel through its imperative handle so the
            // dragged size survives across collapse/expand.
            const ref = leftPanelRef.current;
            if (next) ref?.collapse();
            else ref?.expand();
            setLeftCollapsed(next);
            layout.update((prev) => ({
              ...prev,
              table: { ...prev.table, left_collapsed: next },
            }));
          }}
        />
      </div>
      {/* dropAnimation={null} kills the @dnd-kit default fly-back-to-
          source 250ms animation. With the optimistic-update on assign,
          the card lands in its new home in the same frame the user
          releases — the fly-back was visually telling the user "drop
          rejected" even though it succeeded. */}
      <DragOverlay dropAnimation={null}>
        {activeDrag ? <DragPreview client={activeDrag} /> : null}
      </DragOverlay>
    </DndContext>
  );
}

/** Phase 3 — portal-rendered preview that follows the cursor while
 *  a client card is being dragged. Lives outside the resize-pane
 *  stacking contexts so it stays visible across the divider. */
function DragPreview({ client }: { client: Device }) {
  const isPending = client.net_uuid === null;
  return (
    <div
      className={cn(
        "rounded-md border bg-card px-3 py-2 text-sm shadow-xl",
        isPending ? "border-amber-400" : "border-primary",
      )}
      style={{ minWidth: 200 }}
    >
      <div className="flex items-center gap-2">
        <span className="font-mono text-xs text-muted-foreground">⠿</span>
        <span className="font-medium">
          {client.display_name || `client ${shortUuid(client.client_uuid)}…`}
        </span>
      </div>
      {client.tap_ip && (
        <div className="mt-1 font-mono text-[10px] text-muted-foreground">
          {client.tap_ip}
        </div>
      )}
    </div>
  );
}

// ── Toolbar ─────────────────────────────────────────────────────────────

function Toolbar({
  viewMode,
  onViewMode,
  isDirty,
  isSaving,
  wsStatus,
}: {
  viewMode: ViewMode;
  onViewMode: (m: ViewMode) => void;
  isDirty: boolean;
  isSaving: boolean;
  wsStatus: ReturnType<typeof useWSStatus>;
}) {
  return (
    <div className="flex items-center gap-3 border-b border-border bg-background px-3 py-2 text-sm">
      <span className="font-semibold">Bifrost</span>
      <div className="ml-auto flex items-center gap-3">
        <SaveStatusChip isDirty={isDirty} isSaving={isSaving} />
        <ViewModeToggle value={viewMode} onChange={onViewMode} />
        <WsStatusBadge status={wsStatus} />
      </div>
    </div>
  );
}

function WsStatusBadge({ status }: { status: ReturnType<typeof useWSStatus> }) {
  return (
    <Badge
      variant={status === "open" ? "success" : status === "connecting" ? "muted" : "destructive"}
    >
      <span
        className={cn(
          "inline-block h-1.5 w-1.5 rounded-full",
          status === "open"
            ? "bg-emerald-500"
            : status === "connecting"
              ? "bg-muted-foreground"
              : "bg-destructive",
        )}
      />
      {status === "open" ? "live" : status === "connecting" ? "connecting" : "offline"}
    </Badge>
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
  // Two-layer wrapper: the OUTER one is the droppable + scroll
  // viewport (full panel height + always overflows-y), the INNER
  // one stretches to at least the viewport height so an empty pane
  // is still 100% drop-zone (fixes "you have to drop on a specific
  // strip" — the whole pane now accepts the drop).
  return (
    <div
      ref={setNodeRef}
      className={cn(
        "h-full overflow-y-auto transition-colors",
        isOver && "bg-amber-50/60",
      )}
    >
      <div className="flex min-h-full flex-col gap-2 p-3">
        <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          Pending ({clients.length})
        </h2>
        {clients.length === 0 ? (
          <div className="flex flex-1 items-center justify-center rounded-md border border-dashed border-border p-6 text-center text-xs text-muted-foreground">
            No unassigned clients. Drag here from a network to detach.
          </div>
        ) : (
          <>
            {clients.map((c) => (
              <PendingClientCard
                key={c.client_uuid}
                client={c}
                onUpdate={onUpdate}
              />
            ))}
            {/* Spacer so the empty area below the cards still
                participates in the drop zone. */}
            <div className="flex-1" aria-hidden />
          </>
        )}
      </div>
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
  // Phase 3 — no inline transform: the dragged image is rendered by
  // <DragOverlay> in a portal so it isn't clipped at the resize-pane
  // divider. The original card just dims while dragging.
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({
    id: `client:${client.client_uuid}`,
    data: { kind: "client", cuid: client.client_uuid, from: null },
  });
  return (
    <div
      ref={setNodeRef}
      className={cn(
        "rounded-md border bg-card p-2 text-sm shadow-sm",
        isDragging && "opacity-40",
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
  pendingPush,
  onUpdateDevice,
  onRenameNet,
  onSetBridgeIp,
  onCreateNet,
  onDeleteNet,
  onPushRoutes,
}: {
  networks: Network[];
  byNet: Map<string, Device[]>;
  pendingPush: Set<string>;
  onUpdateDevice: (nid: string, cid: string, body: DeviceUpdateBody) => void;
  onRenameNet: (nid: string, name: string) => void;
  onSetBridgeIp: (nid: string, ip: string) => void;
  onCreateNet: (name: string) => void;
  onDeleteNet: (nid: string) => void;
  onPushRoutes: (nid: string) => void;
}) {
  const [newName, setNewName] = useState("");
  return (
    <div className="flex flex-col gap-3 p-3">
      {networks.map((n) => (
        <NetworkCard
          key={n.id}
          network={n}
          devices={byNet.get(n.id) ?? []}
          routesPending={pendingPush.has(n.id)}
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
  routesPending,
  onUpdateDevice,
  onRename,
  onSetBridgeIp,
  onDelete,
  onPushRoutes,
}: {
  network: Network;
  devices: Device[];
  routesPending: boolean;
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
    () => [
      ...devices.map((d) => d.tap_ip ?? "").filter(Boolean),
      ...(network.bridge_ip ? [network.bridge_ip] : []),
    ],
    [devices, network.bridge_ip],
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
            className={
              routesPending
                ? "animate-pulse bg-amber-500 text-white ring-2 ring-amber-400 hover:bg-amber-600"
                : undefined
            }
            title={
              routesPending
                ? "LAN subnets changed — click to push to all peers"
                : "Re-derive routes from LAN subnets and push to all peers"
            }
          >
            {routesPending ? "push routes •" : "push routes"}
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
          <div className="space-y-1.5">
            {/* Column header — same grid template as each row so the
                visible labels align with the columns below. */}
            <div
              className={cn(
                ROW_COLS,
                "px-3 pb-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground",
              )}
            >
              <span aria-hidden />
              <span aria-hidden />
              <span>name</span>
              <span>tap IP</span>
              <span>LAN subnets</span>
              <span>throughput</span>
              <span>uuid</span>
            </div>
            <ul className="space-y-1.5">
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
          </div>
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
  // Phase 3 — DragOverlay handles the dragged preview at the document
  // level; the in-place row just dims while a drag is active.
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({
    id: `client:${client.client_uuid}`,
    data: { kind: "client", cuid: client.client_uuid, from: netUuid },
  });
  return (
    <li
      ref={setNodeRef}
      className={cn(
        // Each admitted client is its own bordered card with a muted
        // background + soft shadow so adjacent rows have a clear,
        // glanceable boundary against the network card behind. Same
        // grid template as the header strip above so columns line up.
        ROW_COLS,
        "rounded-md border border-border bg-muted/50 px-3 py-2 text-sm shadow-sm transition-colors hover:bg-muted/70",
        isDragging && "opacity-40",
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
          onUpdate({ lan_subnets: list });
        }}
      />
      <ThroughputCell
        network={netUuid}
        clientUuid={client.client_uuid}
        online={client.online && client.admitted}
      />
      <span
        className="truncate font-mono text-xs text-muted-foreground"
        title={client.client_uuid}
      >
        {shortUuid(client.client_uuid)}…
      </span>
    </li>
  );
}
