import { useEffect, useState } from "react";
import { defaultWsUrl, WSClient, type WSStatus } from "./ws";

// One process-wide WS client. Sharing it means tab-internal navigations
// don't churn connections.
let shared: WSClient | null = null;

function getClient(): WSClient {
  if (!shared) {
    shared = new WSClient(defaultWsUrl());
    shared.start();
  }
  return shared;
}

/** Subscribe to the WS connection status. */
export function useWSStatus(): WSStatus {
  const [status, setStatus] = useState<WSStatus>("connecting");
  useEffect(() => getClient().onStatus(setStatus), []);
  return status;
}
