import { fmtBps } from "@/lib/format";
import { useDeviceMetrics } from "@/lib/metrics";
import { Sparkline } from "./Sparkline";

interface Props {
  network: string;
  clientUuid: string;
  online: boolean;
}

/**
 * Two-row cell: download (in) on top, upload (out) on bottom. Each row
 * has a triangle indicator, the current bps formatted human-readably,
 * and a 60-sample sparkline. The two sparklines share their y-max so
 * the visual scale is comparable between in / out.
 */
export function ThroughputCell({ network, clientUuid, online }: Props) {
  const m = useDeviceMetrics(network, clientUuid);

  if (!online) {
    return <span className="text-muted-foreground">—</span>;
  }
  if (!m || m.samples.length === 0) {
    return (
      <span className="text-xs text-muted-foreground italic">waiting…</span>
    );
  }

  const last = m.samples[m.samples.length - 1];
  const inSeries = m.samples.map((s) => s.bps_in);
  const outSeries = m.samples.map((s) => s.bps_out);
  const yMax = Math.max(16, ...inSeries, ...outSeries);

  return (
    <div className="flex flex-col gap-0.5 font-mono text-[11px]">
      <div className="flex items-center gap-2">
        <span aria-hidden className="text-emerald-600">
          ▼
        </span>
        {/* w-20 + whitespace-nowrap: longest possible fmtBps output is
            "99.9 GB/s" (9 chars). 64 px (w-16) wasn't quite enough so
            "98.0 B/s" wrapped at the space; 80 px gives a comfortable
            margin and the explicit nowrap keeps it on one line even
            at edge cases. */}
        <span className="w-20 whitespace-nowrap tabular-nums text-right">
          {fmtBps(last.bps_in)}
        </span>
        <Sparkline
          values={inSeries}
          variant="fill"
          yMax={yMax}
          className="text-emerald-600"
        />
      </div>
      <div className="flex items-center gap-2">
        <span aria-hidden className="text-sky-600">
          ▲
        </span>
        <span className="w-20 whitespace-nowrap tabular-nums text-right">
          {fmtBps(last.bps_out)}
        </span>
        <Sparkline
          values={outSeries}
          variant="fill"
          yMax={yMax}
          className="text-sky-600"
        />
      </div>
    </div>
  );
}
