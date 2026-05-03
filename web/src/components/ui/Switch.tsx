import { cn } from "@/lib/cn";

interface SwitchProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  /** Show as the spinner state — visually pressed-in but not toggling. */
  busy?: boolean;
  /** Optional label for accessibility. */
  label?: string;
  className?: string;
  /** Optional size; default is "md". */
  size?: "sm" | "md";
}

/**
 * Plain toggle switch. Click flips `checked`; if `busy` is true the
 * thumb is dimmed and clicks are dropped (caller is responsible for
 * preventing concurrent flips).
 */
export function Switch({
  checked,
  onChange,
  busy,
  label,
  className,
  size = "md",
}: SwitchProps) {
  const dims =
    size === "sm"
      ? { track: "h-4 w-7", thumb: "h-3 w-3", on: "translate-x-3" }
      : { track: "h-5 w-9", thumb: "h-4 w-4", on: "translate-x-4" };
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => {
        if (!busy) onChange(!checked);
      }}
      className={cn(
        "relative inline-flex shrink-0 cursor-pointer items-center rounded-full border-2 border-transparent transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-1",
        dims.track,
        checked ? "bg-emerald-500" : "bg-muted-foreground/30",
        busy && "cursor-progress opacity-70",
        className,
      )}
    >
      <span
        aria-hidden
        className={cn(
          "pointer-events-none inline-block transform rounded-full bg-background shadow ring-0 transition-transform",
          dims.thumb,
          checked ? dims.on : "translate-x-0",
        )}
      />
    </button>
  );
}
