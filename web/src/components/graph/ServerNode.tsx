import { Handle, Position } from "@xyflow/react";
import { Badge } from "@/components/ui/Badge";
import { InlineEdit } from "@/components/InlineEdit";

// Extended with `Record<string, unknown>` so it satisfies the
// constraint `@xyflow/react`'s `Node<TData>` puts on data payloads.
export type ServerNodeData = {
  /** Network name from `bifrost-server admin mknet`. */
  networkName: string;
  /** How many devices are joined right now. */
  onlineCount: number;
  /** Total admitted devices in this network. */
  totalCount: number;
  /** Rename callback (PATCH /api/networks/:nid). */
  onRenameNetwork: (name: string) => void;
} & Record<string, unknown>;

interface Props {
  data: ServerNodeData;
}

/**
 * The central hub. One per network — represents the server itself.
 * Fixed at the canvas centre by the layout. The network name is
 * editable inline; the same mutation backs the name on the Networks
 * index page, so changes propagate both ways via the
 * `network.changed` WS event.
 *
 * Four Handles, one per side, are emitted so the floating-edge
 * renderer (see `FloatingEdge.tsx`) can pick whichever side is
 * closest to each device — the line snaps to the nearest pair of
 * midpoints rather than always launching from the top.
 */
export function ServerNode({ data }: Props) {
  return (
    <div className="flex w-44 flex-col items-center gap-1.5 rounded-xl border-2 border-primary bg-background px-4 py-3 shadow-md">
      {/* Four invisible handles, one per side. The floating-edge code
          ignores the specific handle picked by ReactFlow and computes
          its own anchor based on the node's bounding box. */}
      <SideHandle id="src-top" position={Position.Top} type="source" />
      <SideHandle id="src-right" position={Position.Right} type="source" />
      <SideHandle id="src-bottom" position={Position.Bottom} type="source" />
      <SideHandle id="src-left" position={Position.Left} type="source" />

      <div className="font-semibold">Hub</div>
      <InlineEdit
        value={data.networkName}
        placeholder="(unnamed network)"
        examplePlaceholder="e.g. office-vpn"
        validate={(v) => (v.trim() === "" ? "name is required" : null)}
        className="max-w-full text-xs text-muted-foreground"
        inputClassName="w-32 text-xs"
        onCommit={(v) => data.onRenameNetwork(v.trim())}
      />
      <Badge
        variant={data.onlineCount > 0 ? "success" : "muted"}
        className="text-[11px]"
      >
        {data.onlineCount} / {data.totalCount} online
      </Badge>
    </div>
  );
}

function SideHandle({
  id,
  position,
  type,
}: {
  id: string;
  position: Position;
  type: "source" | "target";
}) {
  return (
    <Handle
      id={id}
      type={type}
      position={position}
      className="!h-0 !w-0 !border-0 !bg-transparent"
      isConnectable={false}
    />
  );
}
