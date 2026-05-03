import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api, ApiError, type DeviceUpdateBody } from "@/lib/api";
import { pushToast } from "@/lib/toast";
import type { Device } from "@/lib/types";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { InlineEdit } from "@/components/InlineEdit";
import { Table, TBody, TD, TH, THead, TR } from "@/components/ui/Table";
import { ThroughputCell } from "@/components/ThroughputCell";

function shortUuid(s: string) {
  return s.replace(/-/g, "").slice(0, 8);
}

function isCidr(s: string): boolean {
  // Loose check: x.x.x.x/N or x:x:...:x/N. Server validates strictly.
  return /^[0-9a-fA-F:.]+\/\d{1,3}$/.test(s);
}

function fmtErr(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function DeviceTable() {
  const { nid } = useParams<{ nid: string }>();
  const qc = useQueryClient();
  const queryKey = ["devices", nid] as const;

  const q = useQuery({
    queryKey,
    queryFn: () => api.listDevices(nid!),
    // Events (device.online / device.changed / ...) drive freshness;
    // the timed poll is a slow safety net in case a WS event is
    // missed (lagged subscriber, dropped connection mid-event).
    refetchInterval: 30_000,
    enabled: !!nid,
  });

  // ── Mutations ────────────────────────────────────────────────────────

  const updateDevice = useMutation({
    mutationFn: ({ cid, body }: { cid: string; body: DeviceUpdateBody }) =>
      api.updateDevice(nid!, cid, body),
    onMutate: async ({ cid, body }) => {
      await qc.cancelQueries({ queryKey });
      const prev = qc.getQueryData<Device[]>(queryKey);
      qc.setQueryData<Device[]>(queryKey, (old) =>
        old?.map((d) =>
          d.client_uuid === cid
            ? {
                ...d,
                ...(body.name !== undefined ? { display_name: body.name } : {}),
                ...(body.tap_ip !== undefined
                  ? { tap_ip: body.tap_ip === "" ? null : body.tap_ip }
                  : {}),
                ...(body.lan_subnets !== undefined
                  ? { lan_subnets: body.lan_subnets }
                  : {}),
                ...(body.admitted !== undefined ? { admitted: body.admitted } : {}),
              }
            : d,
        ) ?? [],
      );
      return { prev };
    },
    onError: (err, _vars, ctx) => {
      if (ctx?.prev) qc.setQueryData(queryKey, ctx.prev);
      pushToast("error", `update failed: ${fmtErr(err)}`);
    },
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const approveDevice = useMutation({
    mutationFn: (cid: string) => api.approveDevice(nid!, cid),
    onSuccess: () => pushToast("success", "device admitted"),
    onError: (e) => pushToast("error", `approve failed: ${fmtErr(e)}`),
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const denyDevice = useMutation({
    mutationFn: (cid: string) => api.denyDevice(nid!, cid),
    onSuccess: () => pushToast("info", "device denied"),
    onError: (e) => pushToast("error", `deny failed: ${fmtErr(e)}`),
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const [pushing, setPushing] = useState(false);
  const pushRoutes = async () => {
    if (!nid) return;
    setPushing(true);
    try {
      const r = await api.pushRoutes(nid);
      pushToast(
        "success",
        `pushed ${r.routes.length} route(s) to ${r.count} client(s)`,
      );
    } catch (e) {
      pushToast("error", `push failed: ${fmtErr(e)}`);
    } finally {
      setPushing(false);
    }
  };

  // ── Render ───────────────────────────────────────────────────────────

  return (
    <div className="mx-auto max-w-6xl">
      <div className="mb-4 flex items-center gap-3 text-sm">
        <Link to="/networks" className="text-muted-foreground hover:underline">
          ← Networks
        </Link>
        <span className="font-mono text-xs text-muted-foreground">{nid}</span>
        <div className="ml-auto">
          <Button size="sm" onClick={pushRoutes} disabled={pushing || !q.data?.length}>
            {pushing ? "pushing…" : "Push routes"}
          </Button>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Devices</CardTitle>
        </CardHeader>
        <CardContent>
          {q.isLoading ? (
            <div className="text-sm text-muted-foreground">loading…</div>
          ) : q.isError ? (
            <div className="text-sm text-destructive">
              failed to load: {(q.error as Error).message}
            </div>
          ) : q.data && q.data.length === 0 ? (
            <div className="rounded-md border border-dashed border-border p-8 text-center text-sm text-muted-foreground">
              No devices yet. Devices show up here once a client connects
              and runs <code className="font-mono">join</code>.
            </div>
          ) : (
            <Table>
              <THead>
                <TR>
                  <TH>Status</TH>
                  <TH>Name</TH>
                  <TH>TAP IP</TH>
                  <TH>LAN subnets</TH>
                  <TH>Throughput</TH>
                  <TH>Client UUID</TH>
                  <TH className="text-right">Actions</TH>
                </TR>
              </THead>
              <TBody>
                {q.data?.map((d) => (
                  <TR key={`${d.client_uuid}:${d.net_uuid}`}>
                    <TD>
                      {d.online && d.admitted ? (
                        <Badge variant="success">online</Badge>
                      ) : d.admitted ? (
                        <Badge variant="muted">offline</Badge>
                      ) : (
                        <Badge variant="default">pending</Badge>
                      )}
                    </TD>
                    <TD>
                      {d.admitted ? (
                        <InlineEdit
                          value={d.display_name}
                          placeholder="click to name"
                          onCommit={(v) =>
                            updateDevice.mutate({
                              cid: d.client_uuid,
                              body: { name: v },
                            })
                          }
                        />
                      ) : (
                        <span className="text-muted-foreground italic">unnamed</span>
                      )}
                    </TD>
                    <TD className="font-mono text-xs">
                      {d.admitted ? (
                        <InlineEdit
                          value={d.tap_ip ?? ""}
                          placeholder="click to set"
                          inputClassName="w-32 font-mono"
                          validate={(v) =>
                            v === "" || isCidr(v) ? null : "expected x.x.x.x/N"
                          }
                          onCommit={(v) =>
                            updateDevice.mutate({
                              cid: d.client_uuid,
                              body: { tap_ip: v },
                            })
                          }
                        />
                      ) : (
                        "—"
                      )}
                    </TD>
                    <TD>
                      {d.admitted ? (
                        <InlineEdit
                          value={d.lan_subnets.join(", ")}
                          placeholder="comma-separated CIDRs"
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
                            updateDevice.mutate({
                              cid: d.client_uuid,
                              body: { lan_subnets: list },
                            });
                          }}
                        />
                      ) : (
                        <span className="text-muted-foreground">—</span>
                      )}
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
                    <TD className="text-right">
                      {d.admitted ? (
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() =>
                            updateDevice.mutate({
                              cid: d.client_uuid,
                              body: { admitted: false },
                            })
                          }
                        >
                          Kick
                        </Button>
                      ) : (
                        <div className="flex justify-end gap-1">
                          <Button
                            size="sm"
                            onClick={() => approveDevice.mutate(d.client_uuid)}
                          >
                            Approve
                          </Button>
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={() => denyDevice.mutate(d.client_uuid)}
                          >
                            Deny
                          </Button>
                        </div>
                      )}
                    </TD>
                  </TR>
                ))}
              </TBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
