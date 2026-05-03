import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { api } from "@/lib/api";
import { fmtErr } from "@/lib/format";
import { pushToast } from "@/lib/toast";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { InlineEdit } from "@/components/InlineEdit";
import { Table, TBody, TD, TH, THead, TR } from "@/components/ui/Table";

export function NetworkList() {
  const qc = useQueryClient();
  const queryKey = ["networks"] as const;
  const q = useQuery({
    queryKey,
    queryFn: () => api.listNetworks(),
    // Events drive freshness; this is a slow safety-net poll.
    refetchInterval: 30_000,
  });

  // ── Mutations ────────────────────────────────────────────────────────

  const createNet = useMutation({
    mutationFn: (name: string) => api.createNetwork(name),
    onSuccess: (created) =>
      pushToast("success", `created network “${created.name}”`),
    onError: (e) => pushToast("error", `create failed: ${fmtErr(e)}`),
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const renameNet = useMutation({
    mutationFn: ({ id, name }: { id: string; name: string }) =>
      api.renameNetwork(id, name),
    onError: (e) => pushToast("error", `rename failed: ${fmtErr(e)}`),
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  const deleteNet = useMutation({
    mutationFn: (id: string) => api.deleteNetwork(id),
    onSuccess: () => pushToast("info", "network deleted"),
    onError: (e) => pushToast("error", `delete failed: ${fmtErr(e)}`),
    onSettled: () => qc.invalidateQueries({ queryKey }),
  });

  // ── New-network form ─────────────────────────────────────────────────

  const [showNew, setShowNew] = useState(false);
  const [newName, setNewName] = useState("");
  const submitNew = (e: FormEvent) => {
    e.preventDefault();
    const name = newName.trim();
    if (!name) return;
    createNet.mutate(name, {
      onSuccess: () => {
        setNewName("");
        setShowNew(false);
      },
    });
  };

  return (
    <div className="mx-auto max-w-5xl">
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <CardTitle>Networks</CardTitle>
            <div>
              {showNew ? (
                <form
                  onSubmit={submitNew}
                  className="flex items-center gap-2"
                >
                  <input
                    autoFocus
                    placeholder="network name"
                    value={newName}
                    onChange={(e) => setNewName(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Escape") {
                        setShowNew(false);
                        setNewName("");
                      }
                    }}
                    className="rounded border border-border bg-background px-2 py-1 text-sm outline-none focus:ring-1 focus:ring-primary"
                  />
                  <Button type="submit" size="sm" disabled={!newName.trim()}>
                    Create
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    onClick={() => {
                      setShowNew(false);
                      setNewName("");
                    }}
                  >
                    Cancel
                  </Button>
                </form>
              ) : (
                <Button size="sm" onClick={() => setShowNew(true)}>
                  + New network
                </Button>
              )}
            </div>
          </div>
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
              No networks yet. Click <strong>+ New network</strong> above to
              create one.
            </div>
          ) : (
            <Table>
              <THead>
                <TR>
                  <TH>Name</TH>
                  <TH>Devices</TH>
                  <TH>UUID</TH>
                  <TH className="text-right">Actions</TH>
                </TR>
              </THead>
              <TBody>
                {q.data?.map((n) => (
                  <TR key={n.id}>
                    <TD>
                      <div className="flex items-center gap-2">
                        <Link
                          to={`/networks/${n.id}`}
                          className="font-medium hover:underline"
                          aria-label={`Open ${n.name}`}
                        >
                          ↗
                        </Link>
                        <InlineEdit
                          value={n.name}
                          placeholder="click to rename"
                          className="font-medium"
                          validate={(v) =>
                            v.trim() === "" ? "name is required" : null
                          }
                          onCommit={(v) =>
                            renameNet.mutate({ id: n.id, name: v.trim() })
                          }
                        />
                      </div>
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
                    <TD className="font-mono text-xs text-muted-foreground">
                      {n.id}
                    </TD>
                    <TD className="text-right">
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          const ok = window.confirm(
                            `Delete network "${n.name}"? Its ${n.device_count} device row(s) will be removed.`,
                          );
                          if (ok) deleteNet.mutate(n.id);
                        }}
                      >
                        Delete
                      </Button>
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
