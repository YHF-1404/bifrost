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
};
type ClientData = {
  client: Device;
  bridgeIp: string;
  bridgePrefix: Prefix | null;
  collisions: string[];
  onUpdateAdmitted: (body: DeviceUpdateBody) => void;
  onUpdatePending: (body: { name?: string; lan_subnets?: string[] }) => void;
};

const DEFAULT_FRAME = { x: 0, y: 0, width: 720, height: 480 };
const DEFAULT_HUB_OFFSET = { x: 24, y: 36 };
const CLIENT_W = 280;
const CLIENT_H = 150;
const HUB_W = 240;
const HUB_H = 110;
const FRAME_GAP_X = DEFAULT_FRAME.width + 80;
const FRAME_GAP_Y = DEFAULT_FRAME.height + 80;
const COLS = 2;

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
    <div className="rounded-lg border-2 border-primary bg-card text-sm shadow-md">
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
      <div className="space-y-1.5 px-3 py-2">
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
            className="ml-auto h-6 px-2 text-[10px]"
            title="Re-derive routes from LAN subnets and push"
          >
            push
          </Button>
        </div>
      </div>
    </div>
  );
}

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
        "flex flex-col rounded-lg border bg-card text-xs shadow-sm",
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
      <div className="flex flex-col gap-1.5 px-2.5 py-2">

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
      // Reset saved position so the moved client gets a fresh default
      // inside its new frame (or its free-node slot when detaching).
      layout.update((p) => {
        const positions = { ...p.graph.positions };
        delete positions[`client:${cid}`];
        return { ...p, graph: { ...p.graph, positions } };
      });
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
  const pushRoutesMut = useMutation({
    mutationFn: (nid: string) => api.pushRoutes(nid),
    onSuccess: (r) =>
      pushToast("success", `pushed ${r.routes.length} route(s) to ${r.count} client(s)`),
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

    networks.forEach((n, i) => {
      const f = frames[n.id] ?? {
        x: (i % COLS) * FRAME_GAP_X,
        y: Math.floor(i / COLS) * FRAME_GAP_Y,
        width: DEFAULT_FRAME.width,
        height: DEFAULT_FRAME.height,
      };
      ns.push({
        id: `frame:${n.id}`,
        type: "frame",
        data: { net: n },
        position: { x: f.x, y: f.y },
        style: { width: f.width, height: f.height, zIndex: -1 },
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
          onRename: (name: string) => renameNetMut.mutate({ nid: n.id, name }),
          onSetBridgeIp: (ip: string) =>
            setBridgeIpMut.mutate({ nid: n.id, ip }),
          onDelete: () => deleteNetMut.mutate(n.id),
          onPushRoutes: () => pushRoutesMut.mutate(n.id),
        },
        position: hubPos,
        parentId: `frame:${n.id}`,
        // Hub stays inside its frame; clients do NOT (so they can be
        // dragged across to other frames — see #2 in the bug list).
        extent: "parent",
        dragHandle: ".drag-handle",
        style: { width: HUB_W, height: HUB_H },
      });
    });

    const inFrameSeq = new Map<string, number>();
    let freeStackY = 20;
    for (const c of clients) {
      const ckey = `client:${c.client_uuid}`;
      const stored = positions[ckey];
      const net =
        c.net_uuid !== null ? networks.find((n) => n.id === c.net_uuid) : null;
      const bridgePrefix = prefixOf(net?.bridge_ip);
      const collisions: string[] = c.net_uuid
        ? (byNet.get(c.net_uuid) ?? [])
            .filter((d) => d.client_uuid !== c.client_uuid && d.tap_ip)
            .map((d) => d.tap_ip as string)
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
        const seq = inFrameSeq.get(c.net_uuid) ?? 0;
        inFrameSeq.set(c.net_uuid, seq + 1);
        const pos = stored ?? {
          x: HUB_W + 60,
          y: 36 + seq * (CLIENT_H + 16),
        };
        ns.push({
          id: ckey,
          type: "client",
          data: sharedData,
          position: pos,
          parentId: `frame:${c.net_uuid}`,
          // No `extent: "parent"` — clients must be draggable across
          // frame boundaries to trigger assign_client on drop.
          dragHandle: ".drag-handle",
          style: { width: CLIENT_W, height: CLIENT_H },
        });
        if (c.admitted) {
          es.push({
            id: `e:${c.client_uuid}->${c.net_uuid}`,
            source: ckey,
            target: `hub:${c.net_uuid}`,
            style: { strokeDasharray: c.online ? "" : "4 3" },
          });
        }
      } else {
        const rightEdge =
          (Math.min(networks.length, COLS) - 1) * FRAME_GAP_X +
          DEFAULT_FRAME.width +
          80;
        const pos = stored ?? { x: rightEdge, y: freeStackY };
        freeStackY += CLIENT_H + 16;
        ns.push({
          id: ckey,
          type: "client",
          data: sharedData,
          position: pos,
          dragHandle: ".drag-handle",
          style: { width: CLIENT_W, height: CLIENT_H },
        });
      }
    }
    return { nodes: ns, edges: es };
    // The mutations are stable (TanStack Query memoises them); we
    // intentionally exclude them from deps so the memo only recomputes
    // when actual data changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [networks, clients, layout.layout]);

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
        layout.update((prev) => ({
          ...prev,
          graph: {
            ...prev.graph,
            frames: {
              ...prev.graph.frames,
              [nid]: {
                x: node.position.x,
                y: node.position.y,
                width: Number(node.style?.width ?? DEFAULT_FRAME.width),
                height: Number(node.style?.height ?? DEFAULT_FRAME.height),
              },
            },
          },
        }));
        return;
      }

      if (node.type !== "client") {
        // Hub move — just remember its (relative-to-frame) position.
        layout.update((prev) => ({
          ...prev,
          graph: {
            ...prev.graph,
            positions: {
              ...prev.graph.positions,
              [node.id]: { x: node.position.x, y: node.position.y },
            },
          },
        }));
        return;
      }

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

      if (currentNid === targetNid) {
        // Same frame (or both null) — just save the new position.
        layout.update((prev) => ({
          ...prev,
          graph: {
            ...prev.graph,
            positions: {
              ...prev.graph.positions,
              [node.id]: { x: node.position.x, y: node.position.y },
            },
          },
        }));
        return;
      }
      // Cross-frame move (or detach to free) — fire assign_client; the
      // mutation's onMutate clears the saved position so the new frame
      // can lay it out fresh.
      assignMut.mutate({ cid: cuid, nid: targetNid });
    },
    [clients, flow, layout, assignMut],
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
