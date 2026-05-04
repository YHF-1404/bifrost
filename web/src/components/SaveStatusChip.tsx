// Phase 3 — small chip in the toolbar that reflects layout-save state.

import { cn } from "@/lib/cn";

type State = "idle" | "dirty" | "saving";

export function SaveStatusChip({ isDirty, isSaving }: { isDirty: boolean; isSaving: boolean }) {
  const state: State = isSaving ? "saving" : isDirty ? "dirty" : "idle";
  const label =
    state === "saving" ? "saving…" : state === "dirty" ? "unsaved" : "saved";
  const color =
    state === "saving"
      ? "bg-blue-500"
      : state === "dirty"
        ? "bg-amber-500"
        : "bg-emerald-500";
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
      <span className={cn("inline-block h-1.5 w-1.5 rounded-full", color)} />
      layout {label}
    </span>
  );
}
