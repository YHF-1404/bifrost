// WebSocket client for the bifrost server event stream.
//
// Phase 1.1 — the connection itself is the feature. The server has
// no events to send yet, so this just maintains an open connection
// with exponential-backoff reconnects so the indicator on screen
// settles to "connected" once the page mounts.
//
// Phase 1.2+ adds parsing + per-event subscribers.

import type { ServerEvent } from "./types";

export type WSStatus = "connecting" | "open" | "closed";

type Listener<T> = (value: T) => void;

const RECONNECT_MIN_MS = 500;
const RECONNECT_MAX_MS = 15_000;

export class WSClient {
  private ws: WebSocket | null = null;
  private status: WSStatus = "closed";
  private statusListeners = new Set<Listener<WSStatus>>();
  private eventListeners = new Set<Listener<ServerEvent>>();
  private reconnectDelay = RECONNECT_MIN_MS;
  private stopped = false;
  private reconnectTimer: number | null = null;

  constructor(private readonly url: string) {}

  start() {
    this.stopped = false;
    this.openSocket();
  }

  stop() {
    this.stopped = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.ws?.close();
    this.ws = null;
    this.setStatus("closed");
  }

  onStatus(fn: Listener<WSStatus>): () => void {
    this.statusListeners.add(fn);
    fn(this.status);
    return () => this.statusListeners.delete(fn);
  }

  onEvent(fn: Listener<ServerEvent>): () => void {
    this.eventListeners.add(fn);
    return () => this.eventListeners.delete(fn);
  }

  private openSocket() {
    this.setStatus("connecting");
    const ws = new WebSocket(this.url);
    this.ws = ws;

    ws.onopen = () => {
      this.reconnectDelay = RECONNECT_MIN_MS;
      this.setStatus("open");
    };

    ws.onmessage = (ev) => {
      // Phase 1.1: messages are not expected. Be lenient.
      if (typeof ev.data !== "string") return;
      let parsed: ServerEvent;
      try {
        parsed = JSON.parse(ev.data);
      } catch {
        return;
      }
      for (const fn of this.eventListeners) fn(parsed);
    };

    ws.onclose = () => {
      this.ws = null;
      if (this.stopped) {
        this.setStatus("closed");
        return;
      }
      this.setStatus("connecting");
      this.scheduleReconnect();
    };

    ws.onerror = () => {
      // The browser will fire `close` right after, where we'll
      // schedule the reconnect. Nothing to do here.
    };
  }

  private scheduleReconnect() {
    const delay = this.reconnectDelay;
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, RECONNECT_MAX_MS);
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      if (!this.stopped) this.openSocket();
    }, delay);
  }

  private setStatus(s: WSStatus) {
    if (s === this.status) return;
    this.status = s;
    for (const fn of this.statusListeners) fn(s);
  }
}

/** Build the absolute ws:// URL from window.location, honoring https. */
export function defaultWsUrl(): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}/ws`;
}
