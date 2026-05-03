// Graph view of one network's devices, built on React Flow.
//
// One ServerNode at the centre, a ring of DeviceNodes around it,
// edges from server → each device. Edge animation indicates that
// the device is currently joined.
//
// User drags are preserved across re-renders: positions are kept in
// React state via `useNodesState` and only synced from props when
// the *set* of devices changes (add / remove). Edits to a device's
// fields (which fire a new `devices` prop on every device.changed
// event) leave node positions alone.

import {
  Background,
  BackgroundVariant,
  Controls,
  type Edge,
  MiniMap,
  type Node,
  ReactFlow,
  useEdgesState,
  useNodesState,
} from "@xyflow/react";
import { useEffect, useMemo } from "react";
import "@xyflow/react/dist/style.css";

import { DeviceNode, type DeviceNodeData } from "@/components/graph/DeviceNode";
import { ServerNode, type ServerNodeData } from "@/components/graph/ServerNode";
import {
  HUB_POSITION,
  deviceRingPositions,
} from "@/components/graph/graphLayout";
import type { DeviceViewProps } from "./NetworkDetail";

const nodeTypes = {
  server: ServerNode,
  device: DeviceNode,
};

/** Build the desired (server + device) node array from current
 *  `devices`. Positions come from the deterministic ring layout. */
function buildNodes(
  devices: DeviceViewProps["devices"],
  networkId: string,
  onUpdate: DeviceViewProps["onUpdate"],
): Node[] {
  const positions = deviceRingPositions(devices.length);
  const onlineCount = devices.filter((d) => d.online && d.admitted).length;
  const totalCount = devices.filter((d) => d.admitted).length;

  const serverNode: Node<ServerNodeData> = {
    id: `server:${networkId}`,
    type: "server",
    position: HUB_POSITION,
    draggable: false,
    data: {
      networkName: "",
      onlineCount,
      totalCount,
    },
  };

  const deviceNodes: Node<DeviceNodeData>[] = devices.map((d, i) => ({
    id: `device:${d.client_uuid}`,
    type: "device",
    position: positions[i],
    data: { device: d, onUpdate },
  }));

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

export function DevicesAsGraph(props: DeviceViewProps) {
  const { devices, networkId, onUpdate } = props;

  const initialNodes = useMemo(
    () => buildNodes(devices, networkId, onUpdate),
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

  // Stable signature of "which devices are present, in what order".
  // Re-layout positions only when this changes; pure data changes
  // (name / IP / sparkline tick) update node `data` in place.
  const idsKey = useMemo(
    () => devices.map((d) => d.client_uuid).join("|"),
    [devices],
  );

  // Sync data fields on every prop change without disturbing
  // positions the user may have dragged.
  useEffect(() => {
    setNodes((current) => {
      const next = buildNodes(devices, networkId, onUpdate);
      const byId = new Map(current.map((n) => [n.id, n]));
      return next.map((n) => {
        const existing = byId.get(n.id);
        if (!existing) return n; // brand-new node — use computed position
        return { ...n, position: existing.position };
      });
    });
    setEdges(buildEdges(devices, networkId));
    // We deliberately exclude `setNodes`/`setEdges` (stable refs from
    // React Flow) and `onUpdate` (changes per render but we want it
    // tracked). idsKey gates layout-changing updates.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [devices, networkId, idsKey]);

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
          onNodesChange={onNodesChange}
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
