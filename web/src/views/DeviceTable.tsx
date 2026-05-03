import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { api } from "@/lib/api";
import { Badge } from "@/components/ui/Badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Table, TBody, TD, TH, THead, TR } from "@/components/ui/Table";
import { ThroughputCell } from "@/components/ThroughputCell";

function shortUuid(s: string) {
  return s.replace(/-/g, "").slice(0, 8);
}

export function DeviceTable() {
  const { nid } = useParams<{ nid: string }>();
  const q = useQuery({
    queryKey: ["devices", nid],
    queryFn: () => api.listDevices(nid!),
    refetchInterval: 5000,
    enabled: !!nid,
  });

  return (
    <div className="mx-auto max-w-6xl">
      <div className="mb-4 flex items-center gap-3 text-sm">
        <Link to="/networks" className="text-muted-foreground hover:underline">
          ← Networks
        </Link>
        <span className="font-mono text-xs text-muted-foreground">{nid}</span>
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
                </TR>
              </THead>
              <TBody>
                {q.data?.map((d) => (
                  <TR key={`${d.client_uuid}:${d.net_uuid}`}>
                    <TD>
                      {d.online ? (
                        <Badge variant="success">online</Badge>
                      ) : d.admitted ? (
                        <Badge variant="muted">offline</Badge>
                      ) : (
                        <Badge variant="default">pending</Badge>
                      )}
                    </TD>
                    <TD>
                      {d.display_name || (
                        <span className="text-muted-foreground italic">unnamed</span>
                      )}
                    </TD>
                    <TD className="font-mono text-xs">{d.tap_ip ?? "—"}</TD>
                    <TD>
                      {d.lan_subnets.length === 0 ? (
                        <span className="text-muted-foreground">—</span>
                      ) : (
                        <div className="flex flex-wrap gap-1">
                          {d.lan_subnets.map((s) => (
                            <Badge key={s} variant="outline" className="font-mono">
                              {s}
                            </Badge>
                          ))}
                        </div>
                      )}
                    </TD>
                    <TD>
                      <ThroughputCell
                        network={d.net_uuid}
                        clientUuid={d.client_uuid}
                        online={d.online}
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
    </div>
  );
}
