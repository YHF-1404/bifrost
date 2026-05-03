import { useEffect, useState } from "react";
import { sharedWS } from "./wsClient";
import type { WSStatus } from "./ws";

/** Subscribe to the WS connection status. */
export function useWSStatus(): WSStatus {
  const [status, setStatus] = useState<WSStatus>("connecting");
  useEffect(() => sharedWS().onStatus(setStatus), []);
  return status;
}
