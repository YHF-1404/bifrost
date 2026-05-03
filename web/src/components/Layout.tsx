import { Link, NavLink, Outlet } from "react-router-dom";
import { useWSStatus } from "@/lib/useWS";
import { Badge } from "@/components/ui/Badge";
import { cn } from "@/lib/cn";

export function Layout() {
  const ws = useWSStatus();
  return (
    <div className="flex min-h-screen flex-col">
      <header className="sticky top-0 z-10 flex h-12 items-center gap-4 border-b border-border bg-background/95 px-4 backdrop-blur">
        <Link to="/" className="font-semibold">
          Bifrost
        </Link>
        <nav className="flex items-center gap-1 text-sm">
          <NavLink
            to="/networks"
            className={({ isActive }) =>
              cn(
                "rounded-md px-2 py-1",
                isActive
                  ? "bg-accent text-accent-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )
            }
          >
            Networks
          </NavLink>
        </nav>
        <div className="ml-auto">
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
      </header>
      <main className="flex-1 px-4 py-6 sm:px-6">
        <Outlet />
      </main>
    </div>
  );
}
