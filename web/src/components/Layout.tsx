import { Outlet } from "react-router-dom";
import { useWSStatus } from "@/lib/useWS";
import { Badge } from "@/components/ui/Badge";
import { cn } from "@/lib/cn";

/**
 * Phase 3 — the layout is just a thin wrapper around UnifiedView.
 * The toolbar and view-mode toggle live INSIDE that view, since
 * Phase 3 is a single-page app — no nav links to render up here.
 * What's left is the WebSocket status indicator pinned top-right.
 */
export function Layout() {
  const ws = useWSStatus();
  return (
    <div className="relative flex min-h-screen flex-col">
      {/* Floating WS-status pill, stays out of the way of the
          unified-view toolbar (which lives inside <Outlet />). */}
      <div className="pointer-events-none absolute right-3 top-2 z-30">
        <Badge
          variant={ws === "open" ? "success" : ws === "connecting" ? "muted" : "destructive"}
        >
          <span
            className={cn(
              "inline-block h-1.5 w-1.5 rounded-full",
              ws === "open"
                ? "bg-emerald-500"
                : ws === "connecting"
                  ? "bg-muted-foreground"
                  : "bg-destructive",
            )}
          />
          {ws === "open" ? "live" : ws === "connecting" ? "connecting" : "offline"}
        </Badge>
      </div>
      <main className="flex min-h-0 flex-1 flex-col">
        <Outlet />
      </main>
    </div>
  );
}
