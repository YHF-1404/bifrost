// Phase 3 — single-canvas graph view.
//
// All networks live on one React Flow canvas. Each network is a
// solid-bordered group/frame node containing a Hub card and all of
// its admitted clients; pending (unassigned) clients are free
// floating nodes outside any frame.
//
// Interactions:
// * Drag a client across frames ⇒ assign_client(cid, target_nid).
// * Drag a client out of every frame ⇒ assign_client(cid, null).
// * Right-click a Hub card ⇒ "Delete network" (clients fall out as
//   free nodes via the Phase-3 detach behavior).
// * Right-click the canvas blank ⇒ "Create new network".
// * Click any field on a card to edit (admit switch, name, IP, LAN
//   subnets, bridge IP). Cards have a small drag handle (⠿) on the
//   left so editing inputs doesn't grab the node.
//
// Layout (frame x/y/w/h, node x/y) persists to `/api/ui-layout` via
// `useUiLayout`.

import {
  Background,
  Controls,
  type EdgeTypes,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useEdgesState,
  useNodesState,
  useReactFlow,
  type Edge,
  type Node,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
import { FloatingEdge } from "@/components/graph/FloatingEdge";
import "@xyflow/react/dist/style.css";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type DeviceUpdateBody } from "@/lib/api";
import { fmtErr, isCidr, shortUuid } from "@/lib/format";
import { pushToast } from "@/lib/toast";
import type { Device, Network } from "@/lib/types";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import { Switch } from "@/components/ui/Switch";
import { InlineEdit } from "@/components/InlineEdit";
import { IpSegmentInput, type Prefix } from "@/components/IpSegmentInput";
import { ThroughputCell } from "@/components/ThroughputCell";
import { useUiLayout } from "@/lib/useUiLayout";
import { cn } from "@/lib/cn";

// ── Node-data types ─────────────────────────────────────────────────────

type FrameData = { net: Network };
type HubData = {
  net: Network;
  onRename: (name: string) => void;
  onSetBridgeIp: (ip: string) => void;
  onDelete: () => void;
  onPushRoutes: () => void;
  deviceCount: number;
  routesPending: boolean;
};
type ClientData = {
  client: Device;
  bridgeIp: string;
  bridgePrefix: Prefix | null;
  collisions: string[];
  onUpdateAdmitted: (body: DeviceUpdateBody) => void;
  onUpdatePending: (body: { name?: string; lan_subnets?: string[] }) => void;
};

const DEFAULT_FRAME = { x: 0, y: 0, width: 720, height: 520 };
const DEFAULT_HUB_OFFSET = { x: 24, y: 36 };
const CLIENT_W = 280;
const HUB_W = 240;
const HUB_H = 132;
const FRAME_PADDING = 40;
const FRAME_GAP = 24; // gap kept between non-overlapping frames
const FRAME_GAP_X = DEFAULT_FRAME.width + 80;
const FRAME_GAP_Y = DEFAULT_FRAME.height + 80;
const COLS = 2;

// Estimated rendered height of a ClientNode card, given its content.
// LAN badges wrap roughly two-per-row at CLIENT_W=280 / 9px font, so
// each additional row of subnets adds ~22 px. Pending clients lack
// the IP row and the throughput chart, so they get a smaller base.
function clientHeight(c: Device): number {
  const isPending = c.net_uuid === null;
  const lanCount = c.lan_subnets?.length ?? 0;
  const lanRows = lanCount === 0 ? 1 : Math.ceil(lanCount / 2);
  const base = isPending ? 132 : 200;
  return base + Math.max(0, lanRows - 1) * 22;
}

interface FrameBox {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  /** Whether the user has saved a custom position (= less likely to
   *  be moved during overlap resolution). */
  pinned: boolean;
}

/** Push frames apart in-place along the axis of smaller overlap.
 *  Pinned frames are pushed last (i.e. unpinned ones get moved first
 *  to resolve a collision). */
function resolveFrameOverlaps(input: FrameBox[]): FrameBox[] {
  const out = input.map((f) => ({ ...f }));
  for (let iter = 0; iter < 80; iter++) {
    let moved = false;
    for (let i = 0; i < out.length; i++) {
      for (let j = i + 1; j < out.length; j++) {
        const a = out[i]!;
        const b = out[j]!;
        const overlapX =
          Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x);
        const overlapY =
          Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y);
        if (overlapX <= 0 || overlapY <= 0) continue;
        // Decide which one to move: prefer the unpinned one. If both
        // have the same pinned state, move the later one.
        const moveB = !b.pinned || (a.pinned && b.pinned);
        const movee = moveB ? b : a;
        const fixed = moveB ? a : b;
        if (overlapX < overlapY) {
          if (movee.x < fixed.x) movee.x = fixed.x - movee.width - FRAME_GAP;
          else movee.x = fixed.x + fixed.width + FRAME_GAP;
        } else {
          if (movee.y < fixed.y) movee.y = fixed.y - movee.height - FRAME_GAP;
          else movee.y = fixed.y + fixed.height + FRAME_GAP;
        }
        moved = true;
      }
    }
    if (!moved) break;
  }
  return out;
}

// Pull /16 ↔ /24 prefix length from a CIDR or empty.
function prefixOf(cidr: string | null | undefined): Prefix | null {
  if (!cidr) return null;
  const m = /\/(\d{1,2})$/.exec(cidr);
  if (!m) return null;
  const p = Number(m[1]);
  return p === 16 || p === 24 ? p : null;
}

// ── Node renderers ──────────────────────────────────────────────────────

function FrameNode({ data, selected }: NodeProps<Node<FrameData>>) {
  // The frame itself can be dragged by clicking on its border / title
  // strip. The whole inner area is NOT a drag handle so children can
  // be interacted with.
  return (
    <div
      className={cn(
        "h-full w-full rounded-xl border-2 border-solid border-border bg-card/30",
        selected && "border-primary",
      )}
    >
      <div className="drag-handle cursor-grab px-3 py-1 text-xs font-semibold text-muted-foreground">
        {data.net.name || "(unnamed)"}
      </div>
    </div>
  );
}

function HubNode({ data }: NodeProps<Node<HubData>>) {
  const { net } = data;
  return (
    // h-full + flex-col: the card stretches to fill the React Flow
    // wrapper exactly, so FloatingEdge's wrapper-bbox midpoints
    // coincide with the visible card border (no gap below).
    <div className="flex h-full w-full flex-col rounded-lg border-2 border-primary bg-card text-sm shadow-md">
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />
      {/* The whole header strip is the drag handle: a wide easy-to-
          hit grab area, marked with a ⠿ icon for affordance. The
          inputs below intentionally LACK the .drag-handle class so
          clicks on them edit instead of starting a drag. */}
      <div className="drag-handle flex cursor-grab items-center gap-2 border-b border-border bg-primary/5 px-3 py-2 active:cursor-grabbing">
        <span className="text-muted-foreground">⠿</span>
        <span className="font-semibold">{net.name || "(unnamed)"}</span>
        <Badge variant="muted" className="ml-auto">
          {data.deviceCount} dev
        </Badge>
      </div>
      <div className="flex flex-1 flex-col justify-between gap-1 px-3 py-2">
        <div className="flex items-center gap-2 text-xs">
          <span className="text-muted-foreground">name</span>
          <InlineEdit
            value={net.name}
            placeholder="(unnamed)"
            onCommit={(v) => data.onRename(v)}
          />
        </div>
        <div className="flex items-center gap-2 text-xs">
          <span className="text-muted-foreground">br IP</span>
          <IpSegmentInput
            value={net.bridge_ip}
            onCommit={(ip) => data.onSetBridgeIp(ip)}
            bridgePrefix={null}
            allowPrefixToggle
            placeholder="click to set"
          />
        </div>
        <div className="flex items-center gap-1.5 text-[10px]">
          <span className="font-mono text-muted-foreground">{net.bridge_name}</span>
          <Button
            size="sm"
            variant="outline"
            onClick={() => data.onPushRoutes()}
            className={cn(
              "ml-auto h-6 px-2 text-[10px]",
              data.routesPending &&
                "animate-pulse bg-amber-500 text-white ring-2 ring-amber-400 hover:bg-amber-600",
            )}
            title={
              data.routesPending
                ? "LAN subnets changed — click to push to all peers"
                : "Re-derive routes from LAN subnets and push"
            }
          >
            {data.routesPending ? "push •" : "push"}
          </Button>
        </div>
      </div>
    </div>
  );
}

const EDGE_TYPES: EdgeTypes = { floating: FloatingEdge };

function ClientNode({ data }: NodeProps<Node<ClientData>>) {
  const c = data.client;
  const isPending = c.net_uuid === null;
  const status: "online" | "pending" | "offline" =
    c.online && c.admitted ? "online" : !c.admitted ? "pending" : "offline";
  const statusVariant =
    status === "online" ? "success" : status === "pending" ? "default" : "muted";

  const setName = (name: string) => {
    if (isPending) data.onUpdatePending({ name });
    else data.onUpdateAdmitted({ name });
  };
  const setLan = (list: string[]) => {
    if (isPending) data.onUpdatePending({ lan_subnets: list });
    else data.onUpdateAdmitted({ lan_subnets: list });
  };
  const setIp = (ip: string) => data.onUpdateAdmitted({ tap_ip: ip });
  const setAdmit = (next: boolean) => data.onUpdateAdmitted({ admitted: next });

  return (
    <div
      className={cn(
        "flex h-full w-full flex-col rounded-lg border bg-card text-xs shadow-sm",
        isPending ? "border-dashed border-amber-400" : "border-border",
      )}
    >
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />

      {/* Top header strip = the drag handle. Wide and tall enough to
          hit reliably. Inputs below intentionally lack `.drag-handle`
          so they receive clicks for editing. */}
      <div
        className={cn(
          "drag-handle flex cursor-grab items-center gap-1.5 rounded-t-lg border-b border-border px-2.5 py-1.5 active:cursor-grabbing",
          isPending ? "bg-amber-50/60" : "bg-muted/30",
        )}
        title="drag to another network or out to detach"
      >
        <span className="text-muted-foreground">⠿</span>
        <Badge variant={statusVariant}>{status}</Badge>
        {!isPending && (
          <Switch
            checked={c.admitted}
            onChange={setAdmit}
            label={c.admitted ? "Kick this device" : "Admit this device"}
          />
        )}
        <span
          className="ml-auto font-mono text-[10px] text-muted-foreground"
          title={c.client_uuid}
        >
          {shortUuid(c.client_uuid)}…
        </span>
      </div>
      <div className="flex flex-1 flex-col gap-1.5 px-2.5 py-2">

      {/* Name + IP row */}
      <div className="grid grid-cols-[auto_1fr] items-center gap-x-2 gap-y-0.5">
        <span className="text-[10px] text-muted-foreground">name</span>
        <InlineEdit
          value={c.display_name}
          placeholder="click to name"
          onCommit={setName}
        />
        {!isPending && (
          <>
            <span className="text-[10px] text-muted-foreground">IP</span>
            <IpSegmentInput
              value={c.tap_ip ?? ""}
              bridgePrefix={data.bridgePrefix}
              pinFromBridge={data.bridgeIp}
              collisions={data.collisions}
              onCommit={setIp}
              placeholder="click to set"
            />
          </>
        )}
        <span className="text-[10px] text-muted-foreground">LAN</span>
        <InlineEdit
          value={c.lan_subnets.join(", ")}
          placeholder="comma-separated CIDRs"
          examplePlaceholder="e.g. 192.168.1.0/24"
          inputClassName="w-full font-mono"
          display={(v) =>
            v === "" ? (
              <span className="italic text-muted-foreground">click to set</span>
            ) : (
              <div className="flex flex-wrap gap-0.5">
                {v.split(/\s*,\s*/).map((s) => (
                  <Badge key={s} variant="outline" className="font-mono text-[9px]">
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
            setLan(list);
          }}
        />
      </div>

      {/* Throughput (only useful when admitted + online) */}
      {!isPending && c.net_uuid && (
        <div className="border-t border-border pt-1">
          <ThroughputCell
            network={c.net_uuid}
            clientUuid={c.client_uuid}
            online={c.online && c.admitted}
          />
        </div>
      )}
      </div>
    </div>
  );
}

const NODE_TYPES: NodeTypes = {
  frame: FrameNode,
  hub: HubNode,
  client: ClientNode,
};

// ── Outer view (provides ReactFlow context) ─────────────────────────────

export function UnifiedGraphView() {
  return (
    <ReactFlowProvider>
      <UnifiedGraphInner />
    </ReactFlowProvider>
  );
}

interface CtxMenu {
  x: number;
  y: number;
  kind: "hub" | "blank";
  netId?: string;
  flowX?: number;
  flowY?: number;
}

function UnifiedGraphInner() {
  const qc = useQueryClient();
  const flow = useReactFlow();

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

  const layout = useUiLayout();

  // Networks whose `lan_subnets` have been edited since the last push.
  // The Hub card's push button pulses amber while a net is here.
  const [pendingPush, setPendingPush] = useState<Set<string>>(new Set());
  // Per-network shift applied at derive time (so left/up content
  // pushes the frame anchor). We save raw positions in the layout,
  // then add the shift when handing off to React Flow; on dragStop
  // we have to SUBTRACT the shift before persisting so the next
  // derive doesn't double-shift. Exposed via ref so onNodeDragStop
  // (which is a stable callback) can read the latest values.
  const shiftsRef = useRef<Map<string, { shiftX: number; shiftY: number }>>(
    new Map(),
  );
  const markPending = useCallback((nid: string) => {
    setPendingPush((prev) => {
      if (prev.has(nid)) return prev;
      const next = new Set(prev);
      next.add(nid);
      return next;
    });
    pushToast(
      "info",
      "LAN subnets updated — click 'push' on the Hub card to apply.",
    );
  }, []);

  // ── Mutations ─────────────────────────────────────────────────────────
  // Optimistic update on assign: write the client's new net_uuid into
  // the cache the instant the user releases the drag, so the card
  // doesn't snap back to the source frame while the round-trip
  // settles. Also clear the saved position for that client so the
  // re-derived node lands at the new frame's default slot rather than
  // re-using a coordinate that meant something inside the old frame.
  const assignMut = useMutation({
    mutationFn: ({ cid, nid }: { cid: string; nid: string | null }) =>
      api.assignClient(cid, nid),
    onMutate: async ({ cid, nid }) => {
      await qc.cancelQueries({ queryKey: ["clients"] });
      const prev = qc.getQueryData<Device[]>(["clients"]);
      qc.setQueryData<Device[]>(["clients"], (old) =>
        (old ?? []).map((d) =>
          d.client_uuid === cid
            ? { ...d, net_uuid: nid, admitted: false, tap_ip: null }
            : d,
        ),
      );
      // NB: position is set by `onNodeDragStop` BEFORE this mutation
      // fires, in the coordinate system of the destination frame, so
      // the dragged card stays where the user dropped it.
      return { prev };
    },
    onError: (e, _v, ctx) => {
      if (ctx?.prev) qc.setQueryData(["clients"], ctx.prev);
      pushToast("error", `assign failed: ${fmtErr(e)}`);
    },
    onSettled: () => {
      qc.invalidateQueries({ queryKey: ["clients"] });
      qc.invalidateQueries({ queryKey: ["networks"] });
    },
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
  const createNetMut = useMutation({
    mutationFn: (name: string) => api.createNetwork(name),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["networks"] }),
    onError: (e) => pushToast("error", `create failed: ${fmtErr(e)}`),
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
    onError: (e) => pushToast("error", `bridge IP failed: ${fmtErr(e)}`),
  });
  const updateAdmittedMut = useMutation({
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

  // ── Derive nodes/edges from data + persisted positions ────────────────
  const derived = useMemo(() => {
    const ns: Node[] = [];
    const es: Edge[] = [];
    const positions = layout.layout.graph.positions;
    const frames = layout.layout.graph.frames;

    // Devices grouped by net_uuid for quick lookup.
    const byNet = new Map<string, Device[]>();
    for (const c of clients) {
      if (c.net_uuid) {
        const list = byNet.get(c.net_uuid) ?? [];
        list.push(c);
        byNet.set(c.net_uuid, list);
      }
    }

    // Pass 1 — compute each frame's content bounding box across ALL
    // four sides (not just right/bottom). Children with negative
    // relative positions force the frame to grow left/up; we record
    // the necessary shift so we can re-anchor the frame and offset
    // every child by the same delta to keep visual positions stable.
    const frameLayout = (
      nid: string,
    ): {
      width: number;
      height: number;
      shiftX: number;
      shiftY: number;
    } => {
      type Item = { x: number; y: number; w: number; h: number };
      const items: Item[] = [];
      const hubPos = positions[`hub:${nid}`] ?? DEFAULT_HUB_OFFSET;
      items.push({ x: hubPos.x, y: hubPos.y, w: HUB_W, h: HUB_H });
      let stackY = 36;
      for (const c of byNet.get(nid) ?? []) {
        const stored = positions[`client:${c.client_uuid}`];
        const h = clientHeight(c);
        const p = stored ?? { x: HUB_W + 60, y: stackY };
        stackY += h + 16;
        items.push({ x: p.x, y: p.y, w: CLIENT_W, h });
      }
      let minX = Infinity;
      let minY = Infinity;
      let maxX = -Infinity;
      let maxY = -Infinity;
      for (const it of items) {
        minX = Math.min(minX, it.x);
        minY = Math.min(minY, it.y);
        maxX = Math.max(maxX, it.x + it.w);
        maxY = Math.max(maxY, it.y + it.h);
      }
      // shiftX/Y > 0 ⇒ the frame's saved left/top edge lands FRAME_PADDING
      // px to the right/below the leftmost/topmost child after we shift
      // the frame anchor. Negative children become non-negative inside
      // the new frame coord system.
      const shiftX = Math.max(0, FRAME_PADDING - minX);
      const shiftY = Math.max(0, FRAME_PADDING - minY);
      const width = Math.max(
        DEFAULT_FRAME.width,
        maxX + shiftX + FRAME_PADDING,
      );
      const height = Math.max(
        DEFAULT_FRAME.height,
        maxY + shiftY + FRAME_PADDING,
      );
      return { width, height, shiftX, shiftY };
    };

    // Pass 2 — initial frame boxes (pre-collision-resolution). Apply
    // the shifts so frame.x/y move left/up to encompass any
    // negatively-positioned children.
    const shifts = shiftsRef.current;
    shifts.clear();
    let initialFrames: FrameBox[] = networks.map((n, i) => {
      const saved = frames[n.id];
      const { width: w, height: h, shiftX, shiftY } = frameLayout(n.id);
      shifts.set(n.id, { shiftX, shiftY });
      const baseX = (i % COLS) * FRAME_GAP_X;
      const baseY = Math.floor(i / COLS) * FRAME_GAP_Y;
      const x = (saved?.x ?? baseX) - shiftX;
      const y = (saved?.y ?? baseY) - shiftY;
      const finalW = Math.max(saved?.width ?? DEFAULT_FRAME.width, w);
      const finalH = Math.max(saved?.height ?? DEFAULT_FRAME.height, h);
      return {
        id: n.id,
        x,
        y,
        width: finalW,
        height: finalH,
        pinned: !!saved,
      };
    });
    // Pass 3 — push apart any overlapping frames (a recently-grown
    // frame can otherwise cover its neighbours). User-dragged frames
    // are pinned and stay put when possible.
    initialFrames = resolveFrameOverlaps(initialFrames);
    const frameById = new Map(initialFrames.map((f) => [f.id, f]));

    networks.forEach((n) => {
      const fb = frameById.get(n.id)!;
      const shift = shifts.get(n.id) ?? { shiftX: 0, shiftY: 0 };
      ns.push({
        id: `frame:${n.id}`,
        type: "frame",
        data: { net: n },
        position: { x: fb.x, y: fb.y },
        style: { width: fb.width, height: fb.height, zIndex: -1 },
        // Frame uses its title strip as the drag handle so clients
        // can be clicked / dragged independently.
        dragHandle: ".drag-handle",
      });
      const hubKey = `hub:${n.id}`;
      const hubPos = positions[hubKey] ?? DEFAULT_HUB_OFFSET;
      const inThis = byNet.get(n.id) ?? [];
      ns.push({
        id: hubKey,
        type: "hub",
        data: {
          net: n,
          deviceCount: inThis.length,
          routesPending: pendingPush.has(n.id),
          onRename: (name: string) => renameNetMut.mutate({ nid: n.id, name }),
          onSetBridgeIp: (ip: string) =>
            setBridgeIpMut.mutate({ nid: n.id, ip }),
          onDelete: () => deleteNetMut.mutate(n.id),
          onPushRoutes: () => pushRoutesMut.mutate(n.id),
        },
        // Apply the same shift to the hub so growing the frame left
        // or up doesn't visually move the hub card.
        position: { x: hubPos.x + shift.shiftX, y: hubPos.y + shift.shiftY },
        parentId: `frame:${n.id}`,
        // Hub stays inside its frame; clients do NOT (so they can be
        // dragged across to other frames — see #2 in the bug list).
        extent: "parent",
        dragHandle: ".drag-handle",
        style: { width: HUB_W, height: HUB_H },
      });
    });

    const inFrameStackY = new Map<string, number>();
    let freeStackY = 20;
    for (const c of clients) {
      const ckey = `client:${c.client_uuid}`;
      const stored = positions[ckey];
      const net =
        c.net_uuid !== null ? networks.find((n) => n.id === c.net_uuid) : null;
      const bridgePrefix = prefixOf(net?.bridge_ip);
      // Bridge IP is reserved (it's the gateway address). Including it
      // in `collisions` makes the IpSegmentInput reject attempts to
      // set a client TAP IP equal to the bridge IP (issue #8).
      const collisions: string[] = c.net_uuid
        ? [
            ...(byNet.get(c.net_uuid) ?? [])
              .filter((d) => d.client_uuid !== c.client_uuid && d.tap_ip)
              .map((d) => d.tap_ip as string),
            ...(net?.bridge_ip ? [net.bridge_ip] : []),
          ]
        : [];

      const sharedData: ClientData = {
        client: c,
        bridgeIp: net?.bridge_ip ?? "",
        bridgePrefix,
        collisions,
        onUpdateAdmitted: (body) => {
          if (c.net_uuid) {
            updateAdmittedMut.mutate({
              nid: c.net_uuid,
              cid: c.client_uuid,
              body,
            });
          }
        },
        onUpdatePending: (body) =>
          patchPendingMut.mutate({ cid: c.client_uuid, body }),
      };

      if (c.net_uuid) {
        const ch = clientHeight(c);
        const stackY = inFrameStackY.get(c.net_uuid) ?? 36;
        inFrameStackY.set(c.net_uuid, stackY + ch + 16);
        const pos = stored ?? { x: HUB_W + 60, y: stackY };
        // Compensate for the frame's left/up shift so the client
        // appears at the same visual location even when the frame
        // grew leftward to encompass it.
        const shift = shifts.get(c.net_uuid) ?? { shiftX: 0, shiftY: 0 };
        ns.push({
          id: ckey,
          type: "client",
          data: sharedData,
          position: { x: pos.x + shift.shiftX, y: pos.y + shift.shiftY },
          parentId: `frame:${c.net_uuid}`,
          // No `extent: "parent"` — clients must be draggable across
          // frame boundaries to trigger assign_client on drop.
          dragHandle: ".drag-handle",
          style: { width: CLIENT_W, height: ch },
        });
        if (c.admitted) {
          // Edge target is the Hub card so the floating line lands
          // at the hub's closest border midpoint — the source of
          // truth visually for "this client connects to that
          // network". The hub card is sized to fill its React-Flow
          // wrapper exactly (CLIENT_H/HUB_H below + h-full inside),
          // so the endpoint coincides with the visible card border
          // rather than an empty padding zone.
          es.push({
            id: `e:${c.client_uuid}->${c.net_uuid}`,
            type: "floating",
            source: ckey,
            target: `hub:${c.net_uuid}`,
            style: { strokeDasharray: c.online ? "" : "4 3" },
          });
        }
      } else {
        // Free node — place it past the rightmost frame (in absolute
        // flow space), wrapping vertically.
        const ch = clientHeight(c);
        const rightEdge =
          Math.max(...initialFrames.map((f) => f.x + f.width), 0) + 80;
        const pos = stored ?? { x: rightEdge, y: freeStackY };
        freeStackY += ch + 16;
        ns.push({
          id: ckey,
          type: "client",
          data: sharedData,
          position: pos,
          dragHandle: ".drag-handle",
          style: { width: CLIENT_W, height: ch },
        });
      }
    }
    return { nodes: ns, edges: es };
    // The mutations are stable (TanStack Query memoises them); we
    // intentionally exclude them from deps so the memo only recomputes
    // when actual data changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [networks, clients, layout.layout, pendingPush]);

  // React Flow's controlled state. Without these, position changes
  // during a drag don't propagate to the DOM — the user only sees
  // the final position on release (issue #1 in the bug list).
  const [rfNodes, setRfNodes, onNodesChange] = useNodesState<Node>([]);
  const [rfEdges, setRfEdges, onEdgesChange] = useEdgesState<Edge>([]);

  // Re-sync when underlying data or the persisted layout changes.
  // Drags don't trigger this (they don't change either input until
  // onNodeDragStop runs), so the dragged node tracks the cursor.
  useEffect(() => {
    setRfNodes(derived.nodes);
    setRfEdges(derived.edges);
  }, [derived, setRfNodes, setRfEdges]);

  // ── Drag end → persist position + cross-frame assignment ──────────────
  const onNodeDragStop = useCallback(
    (_e: unknown, node: Node) => {
      if (node.type === "frame") {
        const nid = node.id.replace(/^frame:/, "");
        // node.position is the *visual* (post-shift) position. The
        // saved value is the RAW anchor — add the current shift back
        // so the next derive doesn't subtract it a second time and
        // the frame stays where the user released it.
        const shift = shiftsRef.current.get(nid) ?? { shiftX: 0, shiftY: 0 };
        layout.update((prev) => ({
          ...prev,
          graph: {
            ...prev.graph,
            frames: {
              ...prev.graph.frames,
              [nid]: {
                x: node.position.x + shift.shiftX,
                y: node.position.y + shift.shiftY,
                width: Number(node.style?.width ?? DEFAULT_FRAME.width),
                height: Number(node.style?.height ?? DEFAULT_FRAME.height),
              },
            },
          },
        }));
        return;
      }

      if (node.type === "hub") {
        // Hub move — subtract shift before saving so the next render
        // doesn't double-shift it.
        const nid = node.id.replace(/^hub:/, "");
        const shift = shiftsRef.current.get(nid) ?? { shiftX: 0, shiftY: 0 };
        layout.update((prev) => ({
          ...prev,
          graph: {
            ...prev.graph,
            positions: {
              ...prev.graph.positions,
              [node.id]: {
                x: node.position.x - shift.shiftX,
                y: node.position.y - shift.shiftY,
              },
            },
          },
        }));
        return;
      }

      if (node.type !== "client") return;

      // Client move — figure out which frame (if any) we ended up in.
      const cuid = node.id.replace(/^client:/, "");
      const client = clients.find((c) => c.client_uuid === cuid);
      if (!client) return;

      const targetFrame = flow
        .getIntersectingNodes(node)
        .find((n) => n.type === "frame");
      const targetNid = targetFrame
        ? targetFrame.id.replace(/^frame:/, "")
        : null;
      const currentNid = client.net_uuid;

      // node.position is the visual relative position to the OLD
      // frame (which is already at its post-shift x/y in rfNodes).
      // Compute absolute visual position then re-anchor to the target
      // frame, finally subtract the target frame's shift so the saved
      // value is the RAW (pre-shift) coordinate that's stable across
      // re-derives.
      const oldFrame = currentNid
        ? rfNodes.find((n) => n.id === `frame:${currentNid}`)
        : null;
      const absX = (oldFrame?.position.x ?? 0) + node.position.x;
      const absY = (oldFrame?.position.y ?? 0) + node.position.y;
      const targetShift =
        targetNid !== null
          ? shiftsRef.current.get(targetNid) ?? { shiftX: 0, shiftY: 0 }
          : { shiftX: 0, shiftY: 0 };
      const newPos = targetFrame
        ? {
            x: absX - targetFrame.position.x - targetShift.shiftX,
            y: absY - targetFrame.position.y - targetShift.shiftY,
          }
        : { x: absX, y: absY };

      // Save the drop position FIRST so the post-assign re-render
      // keeps the card under the cursor (issue #4).
      layout.update((prev) => ({
        ...prev,
        graph: {
          ...prev.graph,
          positions: {
            ...prev.graph.positions,
            [node.id]: newPos,
          },
        },
      }));

      if (currentNid === targetNid) return; // same frame (or both null)
      assignMut.mutate({ cid: cuid, nid: targetNid });
    },
    [clients, flow, layout, assignMut, rfNodes],
  );

  // ── Right-click menus ─────────────────────────────────────────────────
  const [menu, setMenu] = useState<CtxMenu | null>(null);
  const closeMenu = () => setMenu(null);
  useEffect(() => {
    if (!menu) return;
    const fn = () => closeMenu();
    window.addEventListener("click", fn);
    return () => window.removeEventListener("click", fn);
  }, [menu]);

  const onNodeContextMenu = useCallback(
    (e: React.MouseEvent, node: Node) => {
      if (node.type !== "hub") return;
      e.preventDefault();
      const nid = node.id.replace(/^hub:/, "");
      setMenu({ x: e.clientX, y: e.clientY, kind: "hub", netId: nid });
    },
    [],
  );

  const wrapperRef = useRef<HTMLDivElement | null>(null);
  const onPaneContextMenu = useCallback(
    (e: React.MouseEvent | MouseEvent) => {
      const me = e as React.MouseEvent;
      me.preventDefault();
      const bounds = wrapperRef.current?.getBoundingClientRect();
      const projected = flow.screenToFlowPosition({
        x: me.clientX - (bounds?.left ?? 0),
        y: me.clientY - (bounds?.top ?? 0),
      });
      setMenu({
        x: me.clientX,
        y: me.clientY,
        kind: "blank",
        flowX: projected.x,
        flowY: projected.y,
      });
    },
    [flow],
  );

  // ── Render ────────────────────────────────────────────────────────────
  return (
    <div ref={wrapperRef} className="relative min-h-0 w-full flex-1">
      <div className="absolute inset-0">
        <ReactFlow
          nodes={rfNodes}
          edges={rfEdges}
          nodeTypes={NODE_TYPES}
          edgeTypes={EDGE_TYPES}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          onNodeDragStop={onNodeDragStop}
          onNodeContextMenu={onNodeContextMenu}
          onPaneContextMenu={onPaneContextMenu}
          nodesConnectable={false}
          fitView={networks.length > 0}
          proOptions={{ hideAttribution: true }}
        >
          <Background />
          <Controls position="bottom-right" />
        </ReactFlow>
      </div>
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={closeMenu}
          items={
            menu.kind === "hub"
              ? [
                  {
                    label: "Delete network",
                    danger: true,
                    onClick: () => {
                      if (
                        window.confirm(
                          "Delete this network? Clients will fall back to the pending pool.",
                        )
                      ) {
                        deleteNetMut.mutate(menu.netId!);
                      }
                    },
                  },
                ]
              : [
                  {
                    label: "Create new network…",
                    onClick: () => {
                      const name = window.prompt("Network name?");
                      if (!name?.trim()) return;
                      createNetMut.mutate(name.trim(), {
                        onSuccess: (resp) => {
                          if (
                            menu.flowX !== undefined &&
                            menu.flowY !== undefined
                          ) {
                            layout.update((prev) => ({
                              ...prev,
                              graph: {
                                ...prev.graph,
                                frames: {
                                  ...prev.graph.frames,
                                  [resp.id]: {
                                    x: menu.flowX!,
                                    y: menu.flowY!,
                                    width: DEFAULT_FRAME.width,
                                    height: DEFAULT_FRAME.height,
                                  },
                                },
                              },
                            }));
                          }
                        },
                      });
                    },
                  },
                ]
          }
        />
      )}
    </div>
  );
}

// ── Generic right-click menu ────────────────────────────────────────────

function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: Array<{ label: string; onClick: () => void; danger?: boolean }>;
  onClose: () => void;
}) {
  return (
    <div
      style={{ left: x, top: y }}
      className="fixed z-50 min-w-[10rem] rounded-md border border-border bg-popover py-1 text-sm shadow-lg"
      onClick={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) => (
        <button
          key={i}
          type="button"
          onClick={() => {
            it.onClick();
            onClose();
          }}
          className={cn(
            "block w-full px-3 py-1.5 text-left hover:bg-accent",
            it.danger && "text-destructive",
          )}
        >
          {it.label}
        </button>
      ))}
    </div>
  );
}
