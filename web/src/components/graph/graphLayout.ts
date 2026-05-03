// Static circular layout: hub in the centre, devices on a circle.
//
// Cheap and deterministic; same input order → same output positions,
// so React Flow's diff is stable across re-renders. Force-directed
// layouts are appealing but overkill for the small N (≤ ~20) we
// expect on a single network.

export interface XY {
  x: number;
  y: number;
}

const HUB_X = 0;
const HUB_Y = 0;

/** Radius grows with N so nodes don't overlap when there are many.
 *  At N = 1: 220, N = 8: 240, N = 16: 320. */
function radiusFor(n: number): number {
  if (n <= 1) return 220;
  return Math.max(220, 50 + n * 18);
}

/** Place `n` evenly-spaced points on a circle around the hub. The
 *  first point is at the top (12 o'clock). */
export function deviceRingPositions(n: number): XY[] {
  if (n === 0) return [];
  const r = radiusFor(n);
  const out: XY[] = [];
  for (let i = 0; i < n; i++) {
    const angle = -Math.PI / 2 + (2 * Math.PI * i) / n; // start at top
    out.push({
      x: HUB_X + r * Math.cos(angle),
      y: HUB_Y + r * Math.sin(angle),
    });
  }
  return out;
}

export const HUB_POSITION: XY = { x: HUB_X, y: HUB_Y };
