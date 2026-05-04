// A custom React Flow edge that picks, on every render, the closest
// pair of side-midpoints between source and target nodes — so as the
// user drags nodes around, each edge "snaps" to the nearest side
// instead of always launching from a fixed handle.

import {
  BaseEdge,
  type EdgeProps,
  Position,
  getBezierPath,
  useInternalNode,
} from "@xyflow/react";

interface Side {
  x: number;
  y: number;
  pos: Position;
}

function midpoints(
  node: ReturnType<typeof useInternalNode> | undefined,
): Side[] {
  if (!node || !node.measured) return [];
  const w = node.measured.width ?? 0;
  const h = node.measured.height ?? 0;
  const x = node.internals.positionAbsolute.x;
  const y = node.internals.positionAbsolute.y;
  return [
    { x: x + w / 2, y: y, pos: Position.Top },
    { x: x + w, y: y + h / 2, pos: Position.Right },
    { x: x + w / 2, y: y + h, pos: Position.Bottom },
    { x: x, y: y + h / 2, pos: Position.Left },
  ];
}

export function FloatingEdge({
  id,
  source,
  target,
  markerEnd,
  style,
}: EdgeProps) {
  const sourceNode = useInternalNode(source);
  const targetNode = useInternalNode(target);
  if (!sourceNode || !targetNode) return null;

  const sourceMids = midpoints(sourceNode);
  const targetMids = midpoints(targetNode);
  if (sourceMids.length === 0 || targetMids.length === 0) return null;

  let best: { s: Side; t: Side; d: number } | null = null;
  for (const s of sourceMids) {
    for (const t of targetMids) {
      const dx = s.x - t.x;
      const dy = s.y - t.y;
      const d = dx * dx + dy * dy;
      if (best === null || d < best.d) best = { s, t, d };
    }
  }
  if (!best) return null;

  const [path] = getBezierPath({
    sourceX: best.s.x,
    sourceY: best.s.y,
    sourcePosition: best.s.pos,
    targetX: best.t.x,
    targetY: best.t.y,
    targetPosition: best.t.pos,
  });

  return <BaseEdge id={id} path={path} markerEnd={markerEnd} style={style} />;
}
