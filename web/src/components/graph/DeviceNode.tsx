import { Handle, Position } from "@xyflow/react";
import type { DeviceUpdateBody } from "@/lib/api";
import { isCidr, shortUuid } from "@/lib/format";
import { useDeviceMetrics } from "@/lib/metrics";
import type { Device } from "@/lib/types";
import { Badge } from "@/components/ui/Badge";
import { InlineEdit } from "@/components/InlineEdit";
import { Sparkline } from "@/components/Sparkline";
import { Switch } from "@/components/ui/Switch";

// Same `Record<string, unknown>` extension as ServerNodeData — see
// the comment there.
export type DeviceNodeData = {
  device: Device;
  onUpdate: (cid: string, body: DeviceUpdateBody) => void;
} & Record<string, unknown>;

interface Props {
  data: DeviceNodeData;
}

/**
 * One per device. Mirrors the table row's fields:
 *   - status badge (online / offline / pending)
 *   - editable display_name and tap_ip (admitted only)
 *   - LAN subnets as tag chips
 *   - twin sparklines (in / out) over the last 60 s
 *   - per-node Switch that flips admitted on/off (same model as the
 *     table view's switch).
 *
 * All edits go through the same `onUpdate` callback the table uses,
 * so cache + invalidation behavior is identical between views.
 */
export function DeviceNode({ data }: Props) {
  const { device: d, onUpdate } = data;
  const m = useDeviceMetrics(d.net_uuid, d.client_uuid);
  const live = d.online && d.admitted;
  const inSeries = m?.samples.map((s) => s.bps_in) ?? [];
  const outSeries = m?.samples.map((s) => s.bps_out) ?? [];
  const yMax = Math.max(16, ...inSeries, ...outSeries);

  return (
    <div
      className={[
        "flex w-56 flex-col gap-1.5 rounded-xl border-2 bg-background px-3 py-2.5 shadow-sm",
        live ? "border-emerald-300" : d.admitted ? "border-border" : "border-primary/40",
      ].join(" ")}
    >
      <Handle
        type="target"
        position={Position.Top}
        className="!h-0 !w-0 !border-0 !bg-transparent"
        isConnectable={false}
      />

      <div className="flex items-center gap-2">
        {live ? (
          <Badge variant="success">online</Badge>
        ) : !d.admitted && d.online ? (
          <Badge variant="default">pending</Badge>
        ) : !d.admitted ? (
          <Badge variant="muted">pending · offline</Badge>
        ) : (
          <Badge variant="muted">offline</Badge>
        )}
        <span
          className="ml-auto truncate font-mono text-[10px] text-muted-foreground"
          title={d.client_uuid}
        >
          {shortUuid(d.client_uuid)}
        </span>
      </div>

      <InlineEdit
        value={d.display_name}
        placeholder="click to name"
        className="text-sm font-medium"
        onCommit={(v) => onUpdate(d.client_uuid, { name: v })}
      />

      <InlineEdit
        value={d.tap_ip ?? ""}
        placeholder="click to set IP"
        className="font-mono text-xs"
        inputClassName="w-full font-mono text-xs"
        validate={(v) => (v === "" || isCidr(v) ? null : "expected x.x.x.x/N")}
        onCommit={(v) => onUpdate(d.client_uuid, { tap_ip: v })}
      />

      <InlineEdit
        value={d.lan_subnets.join(", ")}
        placeholder="LAN subnets"
        className="text-xs"
        inputClassName="w-full font-mono text-xs"
        display={(v) =>
          v === "" ? (
            <span className="text-xs italic text-muted-foreground">no LAN</span>
          ) : (
            <div className="flex flex-wrap gap-1">
              {v.split(/\s*,\s*/).map((s) => (
                <Badge key={s} variant="outline" className="font-mono text-[10px]">
                  {s}
                </Badge>
              ))}
            </div>
          )
        }
        validate={(v) => {
          if (v === "") return null;
          for (const p of v.split(/\s*,\s*/)) {
            if (!isCidr(p)) return `bad CIDR: ${p}`;
          }
          return null;
        }}
        onCommit={(v) => {
          const list = v === "" ? [] : v.split(/\s*,\s*/).filter(Boolean);
          onUpdate(d.client_uuid, { lan_subnets: list });
        }}
      />

      {live && (
        <div className="flex items-center gap-1.5">
          <span className="text-emerald-600" aria-hidden>
            ▼
          </span>
          <Sparkline
            values={inSeries}
            variant="fill"
            yMax={yMax}
            width={120}
            height={16}
            className="text-emerald-600"
          />
        </div>
      )}
      {live && (
        <div className="flex items-center gap-1.5">
          <span className="text-sky-600" aria-hidden>
            ▲
          </span>
          <Sparkline
            values={outSeries}
            variant="fill"
            yMax={yMax}
            width={120}
            height={16}
            className="text-sky-600"
          />
        </div>
      )}

      <div className="mt-1 flex items-center gap-2">
        <Switch
          size="sm"
          checked={d.admitted}
          onChange={(next) => onUpdate(d.client_uuid, { admitted: next })}
          label={d.admitted ? "Kick" : "Admit"}
        />
        <span className="text-[11px] text-muted-foreground">
          {d.admitted ? "admitted" : "not admitted"}
        </span>
      </div>
    </div>
  );
}
