// Bridge: Hub WS events → TanStack Query cache invalidations.
//
// On every device.* event, drop the device list (and the network list,
// since counts may have changed) for the affected network. TanStack
// Query then refetches once for any mounted query — invalidations
// arriving in close succession coalesce automatically.
//
// On routes.changed the only displayed surface today is the toast
// emitted by the Push button itself, so we ignore it here. (When the
// graph view in 1.4 starts visualising routes, this is where it'll
// hook in.)
//
// Mount once at the app root via <EventInvalidator />.

import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { sharedWS } from "./wsClient";
import type { ServerEvent } from "./types";

export function useEventInvalidator() {
  const qc = useQueryClient();
  useEffect(() => {
    return sharedWS().onEvent((evt: ServerEvent) => {
      switch (evt.type) {
        case "device.online":
        case "device.offline":
        case "device.changed":
        case "device.pending":
        case "device.removed": {
          qc.invalidateQueries({ queryKey: ["devices", evt.network] });
          // Network header counts (online_count / device_count) are
          // derived from the same data — refresh them too.
          qc.invalidateQueries({ queryKey: ["networks"] });
          break;
        }
        case "network.created":
        case "network.changed":
        case "network.deleted": {
          qc.invalidateQueries({ queryKey: ["networks"] });
          // A delete cascades into device rows; refresh that scope too.
          if (evt.type === "network.deleted") {
            qc.invalidateQueries({ queryKey: ["devices", evt.network] });
          }
          break;
        }
        // metrics.tick handled by the metrics store; routes.changed
        // has no cached UI surface (yet).
        default:
          break;
      }
    });
  }, [qc]);
}

/** Render-less component that mounts the invalidator. */
export function EventInvalidator(): null {
  useEventInvalidator();
  return null;
}
