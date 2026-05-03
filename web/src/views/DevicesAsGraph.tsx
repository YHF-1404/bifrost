// Graph view of one network's devices, built on React Flow.
//
// One ServerNode at the centre, a ring of DeviceNodes around it, edges
// from server → each device. Edge animation indicates that the device
// is currently joined.
//
// Two interactions worth calling out:
//
//   * **Position persistence (server-side).** Drag-determined positions
//     are PUT to `/api/networks/:nid/layout` debounced after each
//     drag-end, and fetched on mount. So a fresh browser on a
//     different machine — or a Ctrl-Shift-Del — still loads the same
//     arrangement. Pre-server-load and on transient network errors we
//     fall back to localStorage so dragging stays snappy without a
//     round-trip per drop.
//
//   * **Floating edges.** Each node exposes four invisible handles
//     (top/right/bottom/left midpoints). The custom `FloatingEdge`
//     picks the closest pair on every render — drag a device to the
//     hub's right and the edge swings to enter on its left side. See
//     `components/graph/FloatingEdge.tsx`.

import { useQuery } from "@tanstack/react-query";
import {
  Background,
  BackgroundVariant,
  Controls,
  type Edge,
  type EdgeTypes,
  MiniMap,
  type Node,
  type NodeChange,
  ReactFlow,
  useEdgesState,
  useNodesState,
} from "@xyflow/react";
import { useCallback, useEffect, useMemo, useRef } from "react";
import "@xyflow/react/dist/style.css";

import { DeviceNode, type DeviceNodeData } from "@/components/graph/DeviceNode";
import { FloatingEdge } from "@/components/graph/FloatingEdge";
import { ServerNode, type ServerNodeData } from "@/components/graph/ServerNode";
import {
  HUB_POSITION,
  type XY,
  deviceRingPositions,
} from "@/components/graph/graphLayout";
import { api } from "@/lib/api";
import type { DeviceViewProps } from "./NetworkDetail";

const nodeTypes = {
  server: ServerNode,
  device: DeviceNode,
};

const edgeTypes: EdgeTypes = {
  floating: FloatingEdge,
};

// localStorage key — versioned so a future schema change can ignore
// stale entries instead of crashing. Used as a fallback while the
// server fetch is in flight (so dragging stays snappy on first load
// after a refresh) and as a write-through cache so an offline server
// doesn't lose drags either.
const POSITIONS_KEY_PREFIX = "bifrost.graph.positions.v1.";

type PositionMap = Record<string, XY>;

function loadLocalPositions(networkId: string): PositionMap {
  try {
    const raw = localStorage.getItem(POSITIONS_KEY_PREFIX + networkId);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (parsed && typeof parsed === "object") return parsed as PositionMap;
    return {};
  } catch {
    return {};
  }
}

function saveLocalPositions(networkId: string, map: PositionMap) {
  try {
    localStorage.setItem(POSITIONS_KEY_PREFIX + networkId, JSON.stringify(map));
  } catch {
    // Quota exceeded / private mode — fine, server-side is authoritative.
  }
}

/** Build the desired (server + device) node array from current
 *  `devices`. Positions come from the saved-positions map first, then
 *  fall back to the deterministic ring layout. */
function buildNodes(
  devices: DeviceViewProps["devices"],
  networkId: string,
  networkName: string,
  onUpdate: DeviceViewProps["onUpdate"],
  onRenameNetwork: DeviceViewProps["onRenameNetwork"],
  saved: PositionMap,
): Node[] {
  const positions = deviceRingPositions(devices.length);
  const onlineCount = devices.filter((d) => d.online && d.admitted).length;
  const totalCount = devices.filter((d) => d.admitted).length;

  const serverId = `server:${networkId}`;
  const serverNode: Node<ServerNodeData> = {
    id: serverId,
    type: "server",
    position: saved[serverId] ?? HUB_POSITION,
    data: {
      networkName,
      onlineCount,
      totalCount,
      onRenameNetwork,
    },
  };

  const deviceNodes: Node<DeviceNodeData>[] = devices.map((d, i) => {
    const id = `device:${d.client_uuid}`;
    return {
      id,
      type: "device",
      position: saved[id] ?? positions[i],
      data: { device: d, onUpdate },
    };
  });

  return [serverNode, ...deviceNodes];
}

function buildEdges(
  devices: DeviceViewProps["devices"],
  networkId: string,
): Edge[] {
  return devices.map((d) => {
    const live = d.online && d.admitted;
    return {
      id: `edge:${d.client_uuid}`,
      type: "floating",
      source: `server:${networkId}`,
      target: `device:${d.client_uuid}`,
      animated: live,
      style: {
        stroke: live
          ? "rgb(16 185 129)"
          : d.admitted
            ? "rgb(148 163 184)"
            : "rgb(99 102 241)",
        strokeWidth: live ? 2 : 1.5,
        strokeDasharray: d.admitted ? undefined : "4 3",
      },
    };
  });
}

// Debounce latency for layout PUTs. Long enough that flicking a node
// across the canvas doesn't generate a request per pixel, short enough
// that a fresh browser on a different machine sees the new position
// without a noticeable lag. We don't try to coalesce across multiple
// nodes — each drag end fires once, and the request body is the
// *whole* map, so the latest write always wins.
const LAYOUT_PUT_DEBOUNCE_MS = 300;

export function DevicesAsGraph(props: DeviceViewProps) {
  const { devices, networkId, networkName, onUpdate, onRenameNetwork } = props;

  // Server-side layout — single source of truth across browsers. The
  // localStorage map is just a warm cache while the GET is in flight
  // and a write-through copy in case the PUT fails.
  const layoutQ = useQuery({
    queryKey: ["layout", networkId] as const,
    queryFn: () => api.getLayout(networkId),
    // Don't refetch on every focus; positions only change when this
    // tab edits them, and `network.changed` events don't carry layout
    // info anyway.
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });

  // Working copy of positions. Seeded from localStorage immediately
  // (zero round-trip), then replaced with the server's view as soon
  // as that GET completes. Mutated by drag handlers.
  const positionsRef = useRef<PositionMap>(loadLocalPositions(networkId));

  // Once the server's layout arrives, merge it in. Server wins for
  // any id present in both maps; ids only known locally (e.g. a node
  // dragged while offline) survive until the next PUT goes through.
  useEffect(() => {
    if (!layoutQ.data) return;
    positionsRef.current = {
      ...positionsRef.current,
      ...layoutQ.data.positions,
    };
    setNodes((current) =>
      current.map((n) => {
        const saved = positionsRef.current[n.id];
        return saved ? { ...n, position: saved } : n;
      }),
    );
    // setNodes is stable per React Flow's contract.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layoutQ.data]);

  const initialNodes = useMemo(
    () =>
      buildNodes(
        devices,
        networkId,
        networkName,
        onUpdate,
        onRenameNetwork,
        positionsRef.current,
      ),
    // Only seed once. Subsequent updates flow through the useEffect
    // below so user drag positions don't get clobbered.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );
  const initialEdges = useMemo(
    () => buildEdges(devices, networkId),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

  // Debounce PUTs so flicking a node across the canvas doesn't fire
  // one request per pixel.
  const putTimerRef = useRef<number | null>(null);
  const schedulePutLayout = useCallback(() => {
    if (putTimerRef.current !== null) {
      window.clearTimeout(putTimerRef.current);
    }
    putTimerRef.current = window.setTimeout(() => {
      putTimerRef.current = null;
      // Fire-and-forget. PUT failures aren't user-visible — the next
      // successful PUT overwrites them. localStorage is the safety net.
      void api
        .putLayout(networkId, { positions: positionsRef.current })
        .catch(() => {});
    }, LAYOUT_PUT_DEBOUNCE_MS);
  }, [networkId]);

  // Wrap onNodesChange so we can persist positions whenever the user
  // releases a drag. React Flow emits `position` changes both during
  // and at the end of a drag — we only persist on `dragging: false`
  // so localStorage and the network only see end-state positions.
  const handleNodesChange = useCallback(
    (changes: NodeChange[]) => {
      onNodesChange(changes);
      let touched = false;
      for (const c of changes) {
        if (c.type === "position" && c.dragging === false && c.position) {
          positionsRef.current[c.id] = c.position;
          touched = true;
        }
      }
      if (touched) {
        saveLocalPositions(networkId, positionsRef.current);
        schedulePutLayout();
      }
    },
    [onNodesChange, networkId, schedulePutLayout],
  );

  // Stable signature of "which devices are present, in what order".
  // Re-layout positions only when this changes; pure data changes
  // (name / IP / sparkline tick) update node `data` in place.
  const idsKey = useMemo(
    () => devices.map((d) => d.client_uuid).join("|"),
    [devices],
  );

  // Sync data fields on every prop change without disturbing
  // positions the user may have dragged or that we restored from
  // server / localStorage on mount.
  useEffect(() => {
    setNodes((current) => {
      const next = buildNodes(
        devices,
        networkId,
        networkName,
        onUpdate,
        onRenameNetwork,
        positionsRef.current,
      );
      const byId = new Map(current.map((n) => [n.id, n]));
      return next.map((n) => {
        const existing = byId.get(n.id);
        if (!existing) return n; // brand-new node — use computed position
        return { ...n, position: existing.position };
      });
    });
    setEdges(buildEdges(devices, networkId));
    // We deliberately exclude `setNodes`/`setEdges` (stable refs from
    // React Flow) and the per-render callbacks (they change every
    // render but the closures capture the right network id). idsKey
    // gates layout-changing updates; networkName is a data update.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [devices, networkId, networkName, idsKey]);

  if (devices.length === 0) {
    return (
      <div className="rounded-md border border-dashed border-border bg-background p-12 text-center text-sm text-muted-foreground">
        No devices yet. Devices show up here once a client connects
        and runs <code className="font-mono">join</code>.
      </div>
    );
  }

  return (
    // `flex-1` + `min-h-0` lets us fill whatever vertical space the
    // ancestor flex column has. `w-full` does the same horizontally.
    //
    // The inner `absolute inset-0` wrapper is load-bearing: ReactFlow
    // renders its root with inline `height: 100%`, but a percentage
    // height only resolves against a parent whose height is *definite*
    // in CSS terms — and a flex item's used size, while pixel-precise
    // at layout time, still has computed `height: auto` for percentage
    // resolution. An absolutely-positioned div with all four offsets
    // pinned to 0 *does* have a definite pixel size, so ReactFlow's
    // 100% can finally resolve. Without this, the canvas collapses to
    // 0px tall and the page looks blank.
    <div className="relative min-h-0 w-full flex-1 rounded-lg border border-border">
      <div className="absolute inset-0">
        <ReactFlow
          nodes={nodes}
          edges={edges}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          onNodesChange={handleNodesChange}
          onEdgesChange={onEdgesChange}
          fitView
          fitViewOptions={{ padding: 0.2, maxZoom: 1.1 }}
          nodesConnectable={false}
          nodesDraggable
          edgesFocusable={false}
          proOptions={{ hideAttribution: true }}
        >
          <Background variant={BackgroundVariant.Dots} gap={20} />
          <Controls showInteractive={false} />
          <MiniMap
            pannable
            zoomable
            ariaLabel="Mini-map of devices in the network"
            nodeColor={(n) => (n.type === "server" ? "#0f172a" : "#10b981")}
          />
        </ReactFlow>
      </div>
    </div>
  );
}
