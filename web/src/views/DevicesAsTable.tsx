// Table view of one network's devices. Pure presentational — all
// data + mutation handlers come in via props from NetworkDetail.

import { isCidr, shortUuid } from "@/lib/format";
import { Badge } from "@/components/ui/Badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { InlineEdit } from "@/components/InlineEdit";
import { Switch } from "@/components/ui/Switch";
import { Table, TBody, TD, TH, THead, TR } from "@/components/ui/Table";
import { ThroughputCell } from "@/components/ThroughputCell";
import type { DeviceViewProps } from "./NetworkDetail";

export function DevicesAsTable({ devices, onUpdate }: DeviceViewProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Devices</CardTitle>
      </CardHeader>
      <CardContent>
        {devices.length === 0 ? (
          <div className="rounded-md border border-dashed border-border p-8 text-center text-sm text-muted-foreground">
            No devices yet. Devices show up here once a client connects
            and runs <code className="font-mono">join</code>.
          </div>
        ) : (
          <Table>
            <THead>
              <TR>
                <TH className="w-20">Admit</TH>
                <TH>Status</TH>
                <TH>Name</TH>
                <TH>TAP IP</TH>
                <TH>LAN subnets</TH>
                <TH>Throughput</TH>
                <TH>Client UUID</TH>
              </TR>
            </THead>
            <TBody>
              {devices.map((d) => (
                <TR key={`${d.client_uuid}:${d.net_uuid}`}>
                  <TD>
                    <Switch
                      checked={d.admitted}
                      onChange={(next) =>
                        onUpdate(d.client_uuid, { admitted: next })
                      }
                      label={d.admitted ? "Kick this device" : "Admit this device"}
                    />
                  </TD>
                  <TD>
                    {d.online && d.admitted ? (
                      <Badge variant="success">online</Badge>
                    ) : !d.admitted && d.online ? (
                      <Badge variant="default">pending</Badge>
                    ) : !d.admitted ? (
                      <Badge variant="muted">pending · offline</Badge>
                    ) : (
                      <Badge variant="muted">offline</Badge>
                    )}
                  </TD>
                  <TD>
                    <InlineEdit
                      value={d.display_name}
                      placeholder="click to name"
                      onCommit={(v) => onUpdate(d.client_uuid, { name: v })}
                    />
                  </TD>
                  <TD className="font-mono text-xs">
                    <InlineEdit
                      value={d.tap_ip ?? ""}
                      placeholder="click to set"
                      examplePlaceholder="e.g. 10.0.0.5/24"
                      inputClassName="w-32 font-mono"
                      validate={(v) =>
                        v === "" || isCidr(v) ? null : "expected x.x.x.x/N"
                      }
                      onCommit={(v) => onUpdate(d.client_uuid, { tap_ip: v })}
                    />
                  </TD>
                  <TD>
                    <InlineEdit
                      value={d.lan_subnets.join(", ")}
                      placeholder="comma-separated CIDRs"
                      examplePlaceholder="e.g. 192.168.1.0/24"
                      inputClassName="w-64 font-mono"
                      display={(v) =>
                        v === "" ? (
                          <span className="text-muted-foreground italic">
                            click to set
                          </span>
                        ) : (
                          <div className="flex flex-wrap gap-1">
                            {v.split(/\s*,\s*/).map((s) => (
                              <Badge key={s} variant="outline" className="font-mono">
                                {s}
                              </Badge>
                            ))}
                          </div>
                        )
                      }
                      validate={(v) => {
                        if (v === "") return null;
                        const parts = v.split(/\s*,\s*/);
                        for (const p of parts) {
                          if (!isCidr(p)) return `bad CIDR: ${p}`;
                        }
                        return null;
                      }}
                      onCommit={(v) => {
                        const list =
                          v === "" ? [] : v.split(/\s*,\s*/).filter(Boolean);
                        onUpdate(d.client_uuid, { lan_subnets: list });
                      }}
                    />
                  </TD>
                  <TD>
                    <ThroughputCell
                      network={d.net_uuid}
                      clientUuid={d.client_uuid}
                      online={d.online && d.admitted}
                    />
                  </TD>
                  <TD className="font-mono text-xs text-muted-foreground" title={d.client_uuid}>
                    {shortUuid(d.client_uuid)}…
                  </TD>
                </TR>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  );
}
