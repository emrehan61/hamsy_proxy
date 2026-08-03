// Reconnecting WebSocket client for GET /api/ws.
//
// Lifecycle: the singleton auto-starts by calling `connect()` at the bottom of
// this module. Consumers just subscribe via `onMessage`/`onStatusChange`, or
// call `wsClient.disconnect()` if they ever need to tear it down explicitly.

import type { WsClientMessage, WsServerMessage } from "./types";

export type WsStatus = "connecting" | "open" | "closed" | "reconnecting";

type MessageHandler = (msg: WsServerMessage) => void;
type StatusHandler = (status: WsStatus) => void;

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;

function wsUrl(): string {
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${location.host}/api/ws`;
}

export class WsClient {
  private socket: WebSocket | null = null;
  private messageHandlers = new Set<MessageHandler>();
  private statusHandlers = new Set<StatusHandler>();
  private backoffMs = INITIAL_BACKOFF_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private explicitlyClosed = false;
  private status: WsStatus = "closed";

  connect(): void {
    this.explicitlyClosed = false;
    if (this.socket && (this.socket.readyState === WebSocket.OPEN || this.socket.readyState === WebSocket.CONNECTING)) {
      return;
    }
    this.openSocket();
  }

  disconnect(): void {
    this.explicitlyClosed = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.socket?.close();
    this.socket = null;
    this.setStatus("closed");
  }

  send(msg: WsClientMessage): void {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;
    try {
      this.socket.send(JSON.stringify(msg));
    } catch {
      // Ignore send failures — the socket may be closing concurrently.
    }
  }

  onMessage(cb: MessageHandler): () => void {
    this.messageHandlers.add(cb);
    return () => this.messageHandlers.delete(cb);
  }

  onStatusChange(cb: StatusHandler): () => void {
    this.statusHandlers.add(cb);
    return () => this.statusHandlers.delete(cb);
  }

  private setStatus(status: WsStatus): void {
    this.status = status;
    for (const handler of this.statusHandlers) handler(status);
  }

  private openSocket(): void {
    this.setStatus(this.status === "closed" ? "connecting" : "reconnecting");
    let socket: WebSocket;
    try {
      socket = new WebSocket(wsUrl());
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.socket = socket;

    socket.onopen = () => {
      this.backoffMs = INITIAL_BACKOFF_MS;
      this.setStatus("open");
    };

    socket.onmessage = (event) => {
      if (typeof event.data !== "string") return;
      let parsed: WsServerMessage;
      try {
        parsed = JSON.parse(event.data) as WsServerMessage;
      } catch {
        return; // Ignore malformed frames.
      }
      for (const handler of this.messageHandlers) {
        try {
          handler(parsed);
        } catch {
          // A subscriber threw — don't let it break other subscribers or the socket.
        }
      }
    };

    socket.onclose = () => {
      this.socket = null;
      if (this.explicitlyClosed) {
        this.setStatus("closed");
        return;
      }
      this.scheduleReconnect();
    };

    socket.onerror = () => {
      // onclose will fire right after; reconnect logic lives there.
    };
  }

  private scheduleReconnect(): void {
    if (this.explicitlyClosed) return;
    this.setStatus("reconnecting");
    if (this.reconnectTimer !== null) return;
    const jitter = Math.random() * 0.3 * this.backoffMs;
    const delay = this.backoffMs + jitter;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
      this.openSocket();
    }, delay);
  }
}

export const wsClient = new WsClient();
wsClient.connect();
