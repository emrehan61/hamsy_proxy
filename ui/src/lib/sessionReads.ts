// The read-only subset of an open HAR session available to agents. No network,
// file access, rule changes or replay can be requested through this dispatcher.
import type { Flow } from "./types";
import { flowsToHar } from "./har";

export interface SessionRead {
  type: "read";
  requestId: string;
  sessionId: string;
  operation: "list_flows" | "get_flow" | "export_har" | "search_flows";
  query: Record<string, string>;
}

function integer(value: string | undefined, fallback: number, min: number, max: number): number {
  if (value === undefined) return fallback;
  const number = Number(value);
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(number) || number < min || number > max) throw new Error("Invalid range");
  return number;
}

function summary(flow: Flow): Record<string, unknown> {
  const result: Record<string, unknown> = { ...flow };
  for (const key of ["request", "response", "originalRequest", "originalResponse", "timings", "wsMessages", "serverAddr", "tls"]) delete result[key];
  return result;
}

/** Clone only the requested record; never change the browser's original data. */
function detail(flow: Flow, includeBodies: boolean): Flow {
  const result = { ...flow, wsMessages: [] };
  for (const key of ["request", "response", "originalRequest", "originalResponse"] as const) {
    const record = result[key];
    if (!record) continue;
    if (!includeBodies || record.body.kind !== "text") {
      // Preserve original capture metadata. The MCP output marks omitted bodies.
      Object.assign(result, { [key]: { ...record, body: { ...record.body, data: "" } } });
    }
  }
  return result;
}

export function readSession(flows: Flow[], request: SessionRead): unknown {
  const q = request.query;
  switch (request.operation) {
    case "list_flows": {
      const limit = integer(q.limit, 50, 1, 201); // one lookahead row for MCP
      const afterSeq = q.afterSeq === undefined ? null : integer(q.afterSeq, 0, 0, Number.MAX_SAFE_INTEGER);
      const statusClass = q.statusClass === undefined ? null : integer(q.statusClass, 0, 1, 5);
      const methods = (q.methods ?? "").split(",").map(v => v.trim().toUpperCase()).filter(Boolean);
      const host = q.host?.toLowerCase();
      const app = q.app?.toLowerCase();
      const search = q.q?.toLowerCase();
      const selected: Record<string, unknown>[] = [];
      // Imported flows have stable ascending sequence numbers. Read from the
      // beginning so afterSeq can page through an entire archive without gaps.
      for (const flow of flows) {
        if (afterSeq !== null && flow.seq <= afterSeq) continue;
        if (host && flow.host.toLowerCase() !== host) continue;
        if (app && flow.app?.toLowerCase() !== app) continue;
        if (methods.length && !methods.includes(flow.method.toUpperCase())) continue;
        if (statusClass !== null && (flow.status === null || Math.floor(flow.status / 100) !== statusClass)) continue;
        if (q.onlyModified === "true" && !flow.modified) continue;
        if (search) {
          const fields = [flow.url, flow.host, flow.method, String(flow.status ?? ""),
            ...(flow.request?.headers.map(h => h.value) ?? []), ...(flow.response?.headers.map(h => h.value) ?? [])];
          if (!fields.some(value => value.toLowerCase().includes(search))) continue;
        }
        selected.push(summary(flow));
        if (selected.length >= limit) break;
      }
      return { flows: selected };
    }
    case "get_flow": {
      const flow = flows.find(flow => flow.id === q.id);
      if (!flow) throw new Error("Flow is not in this session");
      return detail(flow, q.includeBodies === "true");
    }
    case "export_har": {
      const ids = q.ids?.split(",").filter(Boolean) ?? [];
      if (!ids.length || ids.length > 20) throw new Error("Select 1–20 flows");
      const selected = new Set(ids);
      return flowsToHar(flows.filter(flow => selected.has(flow.id)).map(flow => detail(flow, false)), "agent-beta");
    }
    default: throw new Error("Unsupported session operation");
  }
}
