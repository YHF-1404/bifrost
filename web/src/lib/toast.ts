// 50-LOC toast system. No deps; mounts as one component at the app
// root. Modules call `pushToast({...})` from anywhere — query
// `onError` handlers, button clicks, etc.

import { useSyncExternalStore } from "react";

export type ToastKind = "info" | "success" | "error";

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

const DEFAULT_TTL_MS = 5_000;

let nextId = 1;
let toasts: Toast[] = [];
const listeners = new Set<() => void>();

function notify() {
  for (const fn of listeners) fn();
}

export function pushToast(kind: ToastKind, message: string, ttlMs = DEFAULT_TTL_MS) {
  const id = nextId++;
  toasts = [...toasts, { id, kind, message }];
  notify();
  if (ttlMs > 0) {
    window.setTimeout(() => dismissToast(id), ttlMs);
  }
}

export function dismissToast(id: number) {
  toasts = toasts.filter((t) => t.id !== id);
  notify();
}

export function useToasts(): Toast[] {
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => {
        listeners.delete(fn);
      };
    },
    () => toasts,
    () => toasts,
  );
}
