// Phase 3 — TanStack-Query backed hook for the unified ui-layout.
//
// One GET on mount, debounced PUT after every interaction. The hook
// also applies the change to the local query cache eagerly so other
// components observing the layout (e.g. the graph view) see the new
// values without a server round-trip.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef } from "react";
import { api, type UiLayout } from "./api";

const QK = ["ui-layout"] as const;
const PUT_DEBOUNCE_MS = 300;

/** A starter UiLayout used for first paint before the GET resolves. */
export const DEFAULT_LAYOUT: UiLayout = {
  table: { left_ratio: null, left_collapsed: false },
  graph: { positions: {}, frames: {} },
};

export function useUiLayout() {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: QK,
    queryFn: () => api.getUiLayout(),
    // Layout is small and rarely-changing; 5s staleTime + no
    // background refetch is plenty.
    staleTime: 5000,
  });

  const mut = useMutation({
    mutationFn: (next: UiLayout) => api.putUiLayout(next),
  });

  const pendingTimer = useRef<number | null>(null);
  const pending = useRef<UiLayout | null>(null);

  // Always flush any in-flight PUT on unmount so we never lose a
  // dragged-then-immediately-unmounted layout.
  useEffect(() => {
    return () => {
      if (pendingTimer.current !== null) {
        clearTimeout(pendingTimer.current);
        pendingTimer.current = null;
        if (pending.current) {
          // Best-effort fire-and-forget on unmount — caller can't
          // observe the result anyway.
          void api.putUiLayout(pending.current).catch(() => {});
          pending.current = null;
        }
      }
    };
  }, []);

  /** Set a partial patch and schedule a debounced PUT. The cache is
   *  updated immediately so observers reflect the new value. */
  const update = useCallback(
    (patch: (prev: UiLayout) => UiLayout) => {
      const prev = qc.getQueryData<UiLayout>(QK) ?? DEFAULT_LAYOUT;
      const next = patch(prev);
      qc.setQueryData(QK, next);
      pending.current = next;
      if (pendingTimer.current !== null) {
        clearTimeout(pendingTimer.current);
      }
      pendingTimer.current = window.setTimeout(() => {
        pendingTimer.current = null;
        const toSend = pending.current;
        pending.current = null;
        if (toSend) mut.mutate(toSend);
      }, PUT_DEBOUNCE_MS);
    },
    [qc, mut],
  );

  return {
    layout: q.data ?? DEFAULT_LAYOUT,
    isLoading: q.isLoading,
    isSaving: mut.isPending,
    isDirty: pending.current !== null || pendingTimer.current !== null,
    update,
  };
}
