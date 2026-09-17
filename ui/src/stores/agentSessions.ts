// Each open window advertises its HAR tabs. Data stays in IndexedDB until an
// agent asks to read that session; disconnecting withdraws this window's tabs.
import { createEffect, createRoot, onCleanup } from "solid-js";
import { activeSessionId, getHarSession, loadSessionFromDb, sessions } from "./harSessions";
import { readSession, type SessionRead } from "../lib/sessionReads";

export function startAgentSessions(): () => void {
  return createRoot(dispose => {
    let socket: WebSocket | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    let retry = 500;

    const advertise = () => {
      const metadata = sessions().map(session => ({
        id: session.id, name: session.name, flowCount: session.flowCount,
        importedAt: session.importedAt, active: activeSessionId() === session.id,
      }));
      if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "sessions", sessions: metadata }));
    };
    createEffect(advertise);

    const connect = () => {
      if (disposed) return;
      const url = new URL("/api/sessions/ws", window.location.href);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      const connected = new WebSocket(url);
      socket = connected;
      connected.onopen = () => { retry = 500; advertise(); };
      connected.onmessage = async event => {
        let request: SessionRead;
        try {
          if (typeof event.data !== "string" || event.data.length > 65536) return;
          request = JSON.parse(event.data) as SessionRead;
          if (request.type !== "read" || typeof request.requestId !== "string" || typeof request.sessionId !== "string" || !request.query) return;
        } catch { return; }
        let response: unknown;
        try {
          // Do not reopen closed sessions just because an old request arrived.
          if (!getHarSession(request.sessionId)) throw new Error("Session closed");
          const session = await loadSessionFromDb(request.sessionId);
          if (!session?.loaded || !getHarSession(request.sessionId)) throw new Error("Session unavailable");
          response = { type: "reply", requestId: request.requestId, result: readSession(session.flows, request) };
          const text = JSON.stringify(response);
          if (new TextEncoder().encode(text).byteLength > 8 * 1024 * 1024) throw new Error("Response too large");
          if (connected.readyState === WebSocket.OPEN) connected.send(text);
        } catch {
          if (connected.readyState === WebSocket.OPEN) connected.send(JSON.stringify({
            type: "reply", requestId: request.requestId, error: "Session unavailable or response too large",
          }));
        }
      };
      connected.onclose = () => {
        if (!disposed) {
          timer = setTimeout(connect, retry);
          retry = Math.min(retry * 2, 10000);
        }
      };
      connected.onerror = () => connected.close();
    };
    connect();
    onCleanup(() => {
      disposed = true;
      if (timer !== undefined) clearTimeout(timer);
      socket?.close();
    });
    return dispose;
  });
}
