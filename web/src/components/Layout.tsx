import { Outlet } from "react-router-dom";

/**
 * Phase 3 — the layout is just a thin wrapper around UnifiedView.
 * The toolbar (with view-mode toggle, save chip, and WS status
 * indicator) lives INSIDE that view, since Phase 3 is a single-
 * page app — no nav links and no floating overlay to render up
 * here, just a flex column that gives the unified view full
 * viewport height.
 */
export function Layout() {
  return (
    <div className="flex min-h-screen flex-col">
      <main className="flex min-h-0 flex-1 flex-col">
        <Outlet />
      </main>
    </div>
  );
}
