// One process-wide WebSocket client, lazily started on first access.
//
// Both `useWS` (status indicator) and `metrics` (tick → store) hang
// off this same client so a single connection serves the whole tab.

import { defaultWsUrl, WSClient } from "./ws";

let shared: WSClient | null = null;

export function sharedWS(): WSClient {
  if (!shared) {
    shared = new WSClient(defaultWsUrl());
    shared.start();
  }
  return shared;
}
