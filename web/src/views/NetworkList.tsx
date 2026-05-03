import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api } from "@/lib/api";
import { Badge } from "@/components/ui/Badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Table, TBody, TD, TH, THead, TR } from "@/components/ui/Table";

export function NetworkList() {
  const q = useQuery({
    queryKey: ["networks"],
    queryFn: () => api.listNetworks(),
    // Events drive freshness; this is a slow safety-net poll.
    refetchInterval: 30_000,
  });

  return (
    <div className="mx-auto max-w-5xl">
      <Card>
        <CardHeader>
          <CardTitle>Networks</CardTitle>
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
              No networks yet. Create one with{" "}
              <code className="rounded bg-muted px-1 py-0.5 font-mono">
                bifrost-server admin mknet &lt;name&gt;
              </code>
              .
            </div>
          ) : (
            <Table>
              <THead>
                <TR>
                  <TH>Name</TH>
                  <TH>Devices</TH>
                  <TH>UUID</TH>
                </TR>
              </THead>
              <TBody>
                {q.data?.map((n) => (
                  <TR key={n.id}>
                    <TD>
                      <Link
                        to={`/networks/${n.id}`}
                        className="font-medium hover:underline"
                      >
                        {n.name}
                      </Link>
                    </TD>
                    <TD>
                      <div className="flex items-center gap-2">
                        <Badge variant={n.online_count > 0 ? "success" : "muted"}>
                          {n.online_count} online
                        </Badge>
                        <span className="text-xs text-muted-foreground">
                          / {n.device_count} total
                        </span>
                      </div>
                    </TD>
                    <TD className="font-mono text-xs text-muted-foreground">{n.id}</TD>
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
