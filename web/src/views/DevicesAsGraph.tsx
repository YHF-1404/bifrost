// Graph view of one network's devices, built on React Flow.
//
// One ServerNode at the centre, a ring of DeviceNodes around it,
// edges from server → each device. Edge animation indicates that
// the device is currently joined.

import {
  Background,
  BackgroundVariant,
  Controls,
  type Edge,
  MiniMap,
  type Node,
  ReactFlow,
} from "@xyflow/react";
import { useMemo } from "react";
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

export function DevicesAsGraph(props: DeviceViewProps) {
  const { devices, networkId, onUpdate, onApprove, onDeny } = props;

  // Build nodes + edges. `useMemo` keyed on the IDs so changes that
  // don't add / remove devices skip the recomputation; positions are
  // stable across edits to a device's fields.
  const idsKey = useMemo(
    () => devices.map((d) => d.client_uuid).join("|"),
    [devices],
  );

  const nodes: Node[] = useMemo(() => {
    const positions = deviceRingPositions(devices.length);
    const onlineCount = devices.filter((d) => d.online && d.admitted).length;

    const serverNode: Node<ServerNodeData> = {
      id: `server:${networkId}`,
      type: "server",
      position: HUB_POSITION,
      // The hub never moves; users dragging it around would be
      // confusing in a circular layout.
      draggable: false,
      data: {
        // Network name isn't on the device payload — leave blank for
        // now (it can be added by the parent in 1.4.x if desired).
        networkName: "",
        onlineCount,
        totalCount: devices.filter((d) => d.admitted).length,
      },
    };

    const deviceNodes: Node<DeviceNodeData>[] = devices.map((d, i) => ({
      id: `device:${d.client_uuid}`,
      type: "device",
      position: positions[i],
      data: { device: d, onUpdate, onApprove, onDeny },
    }));

    return [serverNode, ...deviceNodes];
    // The mutation handlers come from a stable parent — but we
    // include them so a new closure forces re-render too.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [idsKey, devices, networkId, onUpdate, onApprove, onDeny]);

  const edges: Edge[] = useMemo(
    () =>
      devices.map((d) => {
        const live = d.online && d.admitted;
        return {
          id: `edge:${d.client_uuid}`,
          source: `server:${networkId}`,
          target: `device:${d.client_uuid}`,
          animated: live,
          style: {
            stroke: live ? "rgb(16 185 129)" : d.admitted ? "rgb(148 163 184)" : "rgb(99 102 241)",
            strokeWidth: live ? 2 : 1.5,
            strokeDasharray: d.admitted ? undefined : "4 3",
          },
        };
      }),
    [devices, networkId],
  );

  if (devices.length === 0) {
    return (
      <div className="rounded-md border border-dashed border-border bg-background p-12 text-center text-sm text-muted-foreground">
        No devices yet. Devices show up here once a client connects
        and runs <code className="font-mono">join</code>.
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-border" style={{ height: 560 }}>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.2, maxZoom: 1.1 }}
        nodesConnectable={false}
        nodesDraggable={true}
        edgesFocusable={false}
        proOptions={{ hideAttribution: true }}
        // The user's edits land in the parent's query cache — there's
        // no React-Flow-side state to commit on node move, so we don't
        // wire onNodesChange.
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
  );
}
