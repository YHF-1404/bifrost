// Phase 3 — single-canvas graph view.
//
// All networks live on one React Flow canvas. Each network is a
// solid-bordered group/frame node containing a Hub card and all of
// its admitted clients; pending (unassigned) clients are free
// floating nodes outside any frame.
//
// Interactions:
// * Drag a client into a frame ⇒ assign_client(cid, frame_nid).
// * Drag a client out of every frame ⇒ assign_client(cid, null).
// * Right-click a Hub card ⇒ "Delete network" (clients fall out as
//   free nodes via the Phase-3 detach behavior).
// * Right-click the canvas blank ⇒ "Create new network" (immediately
//   inline-edit its name).
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
  useReactFlow,
  type Edge,
  type Node,
  type NodeChange,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "@/lib/api";
import { fmtErr, shortUuid } from "@/lib/format";
import { pushToast } from "@/lib/toast";
import type { Device, Network } from "@/lib/types";
import { Badge } from "@/components/ui/Badge";
import { useUiLayout } from "@/lib/useUiLayout";
import { cn } from "@/lib/cn";

// ── Node-data types ─────────────────────────────────────────────────────

type FrameData = { net: Network };
type HubData = { net: Network; onDelete: () => void };
type ClientData = { client: Device };

const DEFAULT_FRAME = { x: 0, y: 0, width: 520, height: 360 };
const DEFAULT_HUB_OFFSET = { x: 20, y: 30 };

// ── Node renderers ──────────────────────────────────────────────────────

function FrameNode({ data, selected }: NodeProps<Node<FrameData>>) {
  return (
    <div
      className={cn(
        "h-full w-full rounded-xl border-2 border-solid border-border bg-card/30 backdrop-blur-sm",
        selected && "border-primary",
      )}
    >
      <div className="px-3 py-1 text-xs font-semibold text-muted-foreground">
        {data.net.name || "(unnamed)"}
      </div>
    </div>
  );
}

function HubNode({ data }: NodeProps<Node<HubData>>) {
  return (
    <div className="rounded-lg border-2 border-primary bg-card px-3 py-2 text-sm shadow-md">
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />
      <div className="flex items-center gap-2">
        <span className="font-mono text-xs">⬢</span>
        <span className="font-semibold">{data.net.name || "(unnamed)"}</span>
      </div>
      <div className="mt-1 font-mono text-[10px] text-muted-foreground">
        {data.net.bridge_ip || "no IP"} · {data.net.bridge_name}
      </div>
    </div>
  );
}

function ClientNode({ data }: NodeProps<Node<ClientData>>) {
  const c = data.client;
  const status: "online" | "pending" | "offline" =
    c.online && c.admitted ? "online" : !c.admitted ? "pending" : "offline";
  const variant =
    status === "online" ? "success" : status === "pending" ? "default" : "muted";
  return (
    <div
      className={cn(
        "rounded-lg border bg-card px-3 py-2 text-xs shadow-sm",
        c.net_uuid ? "border-border" : "border-dashed border-amber-400",
      )}
    >
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />
      <div className="flex items-center gap-1.5">
        <Badge variant={variant}>{status}</Badge>
        <span className="truncate font-medium">
          {c.display_name || `client ${shortUuid(c.client_uuid)}…`}
        </span>
      </div>
      {c.tap_ip && (
        <div className="mt-1 font-mono text-[10px] text-muted-foreground">
          {c.tap_ip}
        </div>
      )}
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
  /** Flow-space position for "create here" so the new frame lands
   *  under the cursor, not at the origin. */
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
  const assignMut = useMutation({
    mutationFn: ({ cid, nid }: { cid: string; nid: string | null }) =>
      api.assignClient(cid, nid),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["clients"] });
      qc.invalidateQueries({ queryKey: ["networks"] });
    },
    onError: (e) => pushToast("error", `assign failed: ${fmtErr(e)}`),
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

  // ── Derive nodes/edges from data + persisted positions ────────────────
  const { nodes, edges } = useMemo(() => {
    const ns: Node[] = [];
    const es: Edge[] = [];
    const positions = layout.layout.graph.positions;
    const frames = layout.layout.graph.frames;

    for (const n of networks) {
      const f = frames[n.id] ?? DEFAULT_FRAME;
      ns.push({
        id: `frame:${n.id}`,
        type: "frame",
        data: { net: n },
        position: { x: f.x, y: f.y },
        style: { width: f.width, height: f.height, zIndex: -1 },
        // The frame is a draggable container; clients are children
        // and get clipped to it.
        draggable: true,
      });
      const hubKey = `hub:${n.id}`;
      const hubPos = positions[hubKey] ?? DEFAULT_HUB_OFFSET;
      ns.push({
        id: hubKey,
        type: "hub",
        data: { net: n, onDelete: () => deleteNetMut.mutate(n.id) },
        position: hubPos,
        parentId: `frame:${n.id}`,
        extent: "parent",
      });
    }

    let stackY = 20;
    for (const c of clients) {
      const ckey = `client:${c.client_uuid}`;
      const stored = positions[ckey];
      if (c.net_uuid) {
        // Inside a frame.
        const pos = stored ?? { x: 200, y: 60 + (clients.indexOf(c) * 64) % 200 };
        ns.push({
          id: ckey,
          type: "client",
          data: { client: c },
          position: pos,
          parentId: `frame:${c.net_uuid}`,
          extent: "parent",
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
        // Free-floating; place to the right of the rightmost frame
        // by default.
        const pos = stored ?? { x: 700, y: stackY };
        stackY += 70;
        ns.push({
          id: ckey,
          type: "client",
          data: { client: c },
          position: pos,
        });
      }
    }
    return { nodes: ns, edges: es };
  }, [networks, clients, layout.layout, deleteNetMut]);

  // ── Drag → assign_client ──────────────────────────────────────────────
  const onNodeDragStop = useCallback(
    (_e: unknown, node: Node) => {
      // Persist position right away (debounced inside useUiLayout).
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
      // Client / hub move — store position by node id.
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

      if (node.type !== "client") return;
      const cuid = node.id.replace(/^client:/, "");
      const client = clients.find((c) => c.client_uuid === cuid);
      if (!client) return;

      // Find the frame node we're physically inside, if any.
      // `flow.getIntersectingNodes` returns frames + other clients;
      // filter to frames.
      const intersect = flow.getIntersectingNodes(node).filter(
        (n) => n.type === "frame",
      );
      const targetFrame = intersect[0];
      const targetNid = targetFrame
        ? targetFrame.id.replace(/^frame:/, "")
        : null;
      const currentNid = client.net_uuid;
      if (currentNid === targetNid) return; // no-op (B3)
      assignMut.mutate({ cid: cuid, nid: targetNid });
    },
    [clients, flow, layout, assignMut],
  );

  // React Flow change handler — kept minimal; we drive most state
  // through the queries.
  const onNodesChange = useCallback((_changes: NodeChange[]) => {
    /* React Flow tracks node selection internally; we don't need to
     * mirror it. Position changes flow through onNodeDragStop. */
  }, []);

  // ── Right-click menus ─────────────────────────────────────────────────
  const [menu, setMenu] = useState<CtxMenu | null>(null);
  const closeMenu = () => setMenu(null);
  // Close on any global click.
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
    <div ref={wrapperRef} className="relative flex min-h-0 flex-1">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        onNodesChange={onNodesChange}
        onNodeDragStop={onNodeDragStop}
        onNodeContextMenu={onNodeContextMenu}
        onPaneContextMenu={onPaneContextMenu}
        nodesConnectable={false}
        // Allow children to be dragged outside their parent? No — extent='parent' clips.
        fitView={networks.length > 0}
        proOptions={{ hideAttribution: true }}
      >
        <Background />
        <Controls position="bottom-right" />
      </ReactFlow>
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
                          // Pre-place the frame at the right-click
                          // position so the new card lands where the
                          // user expected.
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
