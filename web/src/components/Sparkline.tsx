import { useMemo } from "react";
import { cn } from "@/lib/cn";

interface SparklineProps {
  /** Values plotted from left (oldest) to right (newest). */
  values: number[];
  /** Total slot count. Missing trailing values stretch the line to the
   *  right edge; missing leading values are interpreted as zeros. */
  width?: number;
  height?: number;
  /** Tailwind text color for the line. */
  className?: string;
  /** "Filled area under the line" or "stroke only". */
  variant?: "fill" | "stroke";
  /** Forced max for the y-axis (so two charts can share scale).
   *  Defaults to max-of-values, with a 16-byte floor so 0 doesn't
   *  produce a flat line at full height. */
  yMax?: number;
}

/**
 * Tiny SVG sparkline. ~30 LOC of geometry; no chart library.
 *
 * The path is built by mapping `values[i]` to a (x, y) pair where
 *   x = i * width / (slots - 1)
 *   y = height - (v / yMax) * height
 * Padded by 1 px on top so the stroke isn't clipped at peaks.
 */
export function Sparkline({
  values,
  width = 80,
  height = 24,
  className,
  variant = "stroke",
  yMax,
}: SparklineProps) {
  const path = useMemo(() => {
    if (values.length === 0) return "";
    const n = values.length;
    const peak =
      yMax ?? Math.max(16, ...values.filter((v) => Number.isFinite(v)));
    const padTop = 1;
    const usableH = height - padTop;
    const step = n === 1 ? 0 : width / (n - 1);
    let d = "";
    for (let i = 0; i < n; i++) {
      const v = values[i];
      const x = (i * step).toFixed(1);
      const y = (height - (v / peak) * usableH).toFixed(1);
      d += i === 0 ? `M${x},${y}` : ` L${x},${y}`;
    }
    if (variant === "fill") {
      d += ` L${width.toFixed(1)},${height} L0,${height} Z`;
    }
    return d;
  }, [values, width, height, variant, yMax]);

  if (!path) {
    return (
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        className={cn("text-muted-foreground/40", className)}
        aria-hidden
      >
        <line
          x1={0}
          x2={width}
          y1={height - 1}
          y2={height - 1}
          stroke="currentColor"
          strokeWidth={1}
        />
      </svg>
    );
  }

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className={cn("text-foreground", className)}
      aria-hidden
    >
      <path
        d={path}
        fill={variant === "fill" ? "currentColor" : "none"}
        fillOpacity={variant === "fill" ? 0.15 : undefined}
        stroke="currentColor"
        strokeWidth={1.25}
        strokeLinejoin="round"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
