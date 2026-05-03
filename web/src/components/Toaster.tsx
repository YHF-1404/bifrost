import { dismissToast, useToasts, type ToastKind } from "@/lib/toast";
import { cn } from "@/lib/cn";

const STYLES: Record<ToastKind, string> = {
  info: "bg-background border-border",
  success: "bg-emerald-50 border-emerald-200 text-emerald-900",
  error: "bg-destructive/10 border-destructive/30 text-destructive",
};

export function Toaster() {
  const toasts = useToasts();
  if (toasts.length === 0) return null;
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-4 z-50 flex flex-col items-center gap-2 px-4">
      {toasts.map((t) => (
        <div
          key={t.id}
          role="status"
          className={cn(
            "pointer-events-auto flex max-w-md items-start gap-3 rounded-md border px-3 py-2 text-sm shadow-md",
            STYLES[t.kind],
          )}
        >
          <span className="flex-1 break-words">{t.message}</span>
          <button
            type="button"
            onClick={() => dismissToast(t.id)}
            className="text-xs opacity-60 hover:opacity-100"
            aria-label="dismiss"
          >
            ✕
          </button>
        </div>
      ))}
    </div>
  );
}
