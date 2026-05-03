import { type HTMLAttributes } from "react";
import { cn } from "@/lib/cn";

type Variant = "default" | "outline" | "success" | "muted" | "destructive";

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  variant?: Variant;
}

const variants: Record<Variant, string> = {
  default: "bg-primary/10 text-primary border-primary/20",
  outline: "bg-background text-foreground border-border",
  success: "bg-emerald-50 text-emerald-700 border-emerald-200",
  muted: "bg-muted text-muted-foreground border-transparent",
  destructive: "bg-destructive/10 text-destructive border-destructive/20",
};

export function Badge({ variant = "default", className, ...rest }: BadgeProps) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium",
        variants[variant],
        className,
      )}
      {...rest}
    />
  );
}
