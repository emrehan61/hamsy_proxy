// Each open window advertises its HAR tabs. Data stays in IndexedDB until an
// agent asks to read that session; disconnecting withdraws this window's tabs.
import { createEffect, createRoot, onCleanup } from "solid-js";
import { activeSessionId, getHarSession, loadSessionFromDb, sessions } from "./harSessions";
import type { Flow } from "../lib/types";
import type { AgentSearchParams } from "../lib/agentSearch";
import { readSession, type SessionRead } from "../lib/sessionReads";

export function startAgentSessions(): () => void {
  return createRoot(dispose => {
    let socket: WebSocket | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    let retry = 500;
    const searches = new Set<() => void>();
    const search = (flows: Flow[], params: AgentSearchParams): Promise<unknown> => {
      if (searches.size >= 2) return Promise.resolve({ error: "Search busy; wait for the current search to finish." });
      return new Promise(resolve => {
        const worker = new Worker(new URL("../lib/agentSearch.worker.ts", import.meta.url), { type: "module" });
        const finish = (result: unknown) => {
          clearTimeout(timeout);
          worker.terminate();
          searches.delete(cancel);
          resolve(result);
        };
        const cancel = () => finish({ error: "Search stopped. The window closed or the search exceeded 6 seconds; simplify the regex or narrow the search." });
        const timeout = setTimeout(cancel, 6000);
        searches.add(cancel);
        worker.onmessage = event => finish(event.data);
        worker.onerror = () => finish({ error: "Search worker failed. Try a simpler regex or narrower search." });
        // A lookahead entry lets the worker signal more pages without copying
        // an entire large archive into a worker for each call.
        const start = params.afterSeq == null ? 0 : flows.findIndex(f => f.seq > params.afterSeq!);
        try { worker.postMessage({ flows: start < 0 ? [] : flows.slice(start, start + 2001), params }); }
        catch { finish({ error: "Search could not start." }); }
      });
    };

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
          const result = request.operation === "search_flows"
            ? await search(session.flows, JSON.parse(request.query.params ?? "{}") as AgentSearchParams)
            : readSession(session.flows, request);
          if (!getHarSession(request.sessionId)) throw new Error("Session closed");
          response = { type: "reply", requestId: request.requestId, result };
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
        for (const cancel of searches) cancel();
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
      for (const cancel of searches) cancel();
      if (timer !== undefined) clearTimeout(timer);
      socket?.close();
    });
    return dispose;
  });
}
