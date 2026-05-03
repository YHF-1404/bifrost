import { Handle, Position } from "@xyflow/react";
import { Badge } from "@/components/ui/Badge";

// Extended with `Record<string, unknown>` so it satisfies the
// constraint `@xyflow/react`'s `Node<TData>` puts on data payloads.
export type ServerNodeData = {
  /** Network name from `bifrost-server admin mknet`. */
  networkName: string;
  /** How many devices are joined right now. */
  onlineCount: number;
  /** Total admitted devices in this network. */
  totalCount: number;
} & Record<string, unknown>;

interface Props {
  data: ServerNodeData;
}

/**
 * The central hub. One per network — represents the server itself.
 * Fixed at the canvas centre by the layout.
 */
export function ServerNode({ data }: Props) {
  return (
    <div className="flex w-44 flex-col items-center gap-1.5 rounded-xl border-2 border-primary bg-background px-4 py-3 shadow-md">
      {/* Source-only handle: edges flow server → device. The Handle
          itself is invisible (rendered with size 0 via the className). */}
      <Handle
        type="source"
        position={Position.Top}
        className="!h-0 !w-0 !border-0 !bg-transparent"
        isConnectable={false}
      />
      <div className="font-semibold">Hub</div>
      <div className="text-xs text-muted-foreground" title={data.networkName}>
        {data.networkName || "(unnamed)"}
      </div>
      <Badge variant={data.onlineCount > 0 ? "success" : "muted"} className="text-[11px]">
        {data.onlineCount} / {data.totalCount} online
      </Badge>
    </div>
  );
}
