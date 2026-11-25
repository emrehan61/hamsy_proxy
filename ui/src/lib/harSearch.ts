import type { BodyPayload, Flow } from "./types";

export type SearchTab = "overview" | "request" | "response" | "websocket";

export interface HarSearchMatch {
  flowId: string;
  field: string;
  tab: SearchTab;
  before: string;
  match: string;
  after: string;
}

export interface HarSearchResult {
  matches: HarSearchMatch[];
  requestCount: number;
  error: string | null;
}

function bodyText(body: BodyPayload | undefined): string {
  if (!body || body.kind === "none") return "";
  if (body.kind !== "base64" && body.encoding !== "base64") return body.data;
  try {
    // HAR exporters often base64-encode JSON and other textual responses.
    const bytes = Uint8Array.from(atob(body.data), (c) => c.charCodeAt(0));
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return /[\x00-\x08\x0e-\x1f]/.test(text) ? "" : text;
  } catch {
    return "";
  }
}

export function* harSearchFields(flow: Flow, textWebSocketsOnly = false): Generator<[string, string, SearchTab]> {
  yield ["URL", flow.url, "overview"];
  yield ["Method", flow.method, "overview"];
  yield ["Status", `${flow.status ?? ""} ${flow.statusText ?? ""}`, "overview"];
  if (flow.error) yield ["Error", flow.error, "overview"];
  if (flow.mimeType) yield ["Content type", flow.mimeType, "overview"];
  if (flow.request) {
    for (const h of flow.request.headers) yield ["Request header", `${h.name}: ${h.value}`, "request"];
    for (const q of flow.request.query) yield ["Query parameter", `${q.name}: ${q.value}`, "request"];
    yield ["Request body", bodyText(flow.request.body), "request"];
  }
  if (flow.response) {
    for (const h of flow.response.headers) yield ["Response header", `${h.name}: ${h.value}`, "response"];
    yield ["Response body", bodyText(flow.response.body), "response"];
  }
  for (const [index, message] of flow.wsMessages.entries()) {
    if (textWebSocketsOnly && message.opcode !== "text") continue;
    yield [`WebSocket message ${index + 1}`, message.data, "websocket"];
  }
}

/** One result per matching field, with a bounded preview of its first match. */
export function searchHar(flows: Flow[], query: string, regex: boolean, caseSensitive: boolean, allowedIds?: Set<string>): HarSearchResult {
  const result: HarSearchResult = { matches: [], requestCount: 0, error: null };
  if (query.length === 0) return result;
  let pattern: RegExp;
  try {
    pattern = new RegExp(regex ? query : query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), caseSensitive ? "m" : "im");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    result.error = message.startsWith("Invalid regular expression") ? message : `Invalid regular expression: ${message}`;
    return result;
  }
  for (const flow of flows) {
    if (allowedIds && !allowedIds.has(flow.id)) continue;
    let found = false;
    for (const [field, text, tab] of harSearchFields(flow)) {
      if (!text) continue;
      const match = pattern.exec(text);
      if (!match) continue;
      found = true;
      const start = match.index;
      const end = start + match[0].length;
      result.matches.push({
        flowId: flow.id, field, tab,
        before: `${start > 65 ? "…" : ""}${text.slice(Math.max(0, start - 65), start)}`,
        match: match[0].length > 160 ? `${match[0].slice(0, 160)}…` : match[0],
        after: `${text.slice(end, end + 100)}${end + 100 < text.length ? "…" : ""}`,
      });
    }
    if (found) result.requestCount += 1;
  }
  return result;
}
