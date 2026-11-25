import type { BodyPayload, Flow, HeaderPair } from "./types";

const MAX_BYTES = 16 * 1024 * 1024;
const fields = ["URL", "Method", "Status", "Error", "Content type", "Request header", "Query parameter", "Request body", "Response header", "Response body", "WebSocket message"] as const;
export interface ContextParams {
  query: string;
  field: typeof fields[number];
  regex?: boolean;
  caseSensitive?: boolean;
  messageIndex?: number | null;
  maxContextBytes?: number | null;
}
interface Source { text: string; structured?: boolean; pair?: HeaderPair; url?: boolean; body?: Pick<BodyPayload, "kind" | "truncated">; messageIndex?: number; truncated?: boolean }
export interface PreparedContext { flowId: string; sources: Source[]; hasMore: boolean; nextMessageIndex: number | null }
const encode = (text: string) => new TextEncoder().encode(text);
const byteLength = (text: string) => text.length > MAX_BYTES ? MAX_BYTES + 1 : encode(text).length;
function validate(q: ContextParams) {
  if (typeof q.query !== "string" || !q.query.length || q.query.length > 1024 || encode(q.query).length > 1024) throw new Error("query must contain 1–1024 UTF-8 bytes");
  if (!fields.includes(q.field)) throw new Error("Invalid context field");
  const limit = q.maxContextBytes ?? 512;
  if (!Number.isInteger(limit) || limit < 1 || limit > 4096) throw new Error("maxContextBytes must be 1–4096");
  if (q.messageIndex != null && (!Number.isSafeInteger(q.messageIndex) || q.messageIndex < 0 || q.field !== "WebSocket message")) throw new Error("messageIndex is only valid for WebSocket message context");
}
/** Select references before crossing the worker boundary; never clone an archive. */
export function prepareContext(flow: Flow, q: ContextParams): PreparedContext {
  validate(q);
  const result: PreparedContext = { flowId: flow.id, sources: [], hasMore: false, nextMessageIndex: null };
  let bytes = 0;
  const add = (source: Source) => {
    const size = byteLength(source.text);
    if (size > MAX_BYTES) throw new Error("Selected field exceeds the 16 MiB context scan limit");
    if (result.sources.length >= 2000 || bytes + size > MAX_BYTES) {
      result.hasMore = true; result.nextMessageIndex = source.messageIndex ?? null; return false;
    }
    bytes += size; result.sources.push(source); return true;
  };
  const body = (value: BodyPayload | undefined) => { if (value) add({ text: value.data, body: { kind: value.kind, truncated: value.truncated }, structured: true }); };
  switch (q.field) {
    case "URL": add({ text: flow.url, url: true }); break;
    case "Method": add({ text: flow.method }); break;
    case "Status": add({ text: `${flow.status ?? ""} ${flow.statusText ?? ""}` }); break;
    case "Error": add({ text: flow.error ?? "", structured: true }); break;
    case "Content type": add({ text: flow.mimeType ?? "" }); break;
    case "Request body": body(flow.request?.body); break;
    case "Response body": body(flow.response?.body); break;
    case "Request header": case "Response header": case "Query parameter": {
      const pairs = q.field === "Request header" ? flow.request?.headers : q.field === "Response header" ? flow.response?.headers : flow.request?.query;
      for (const pair of pairs ?? []) {
        if (byteLength(pair.name) + byteLength(pair.value) + 2 > MAX_BYTES) throw new Error("Selected field exceeds the 16 MiB context scan limit");
        if (!add({ text: `${pair.name}: ${pair.value}`, pair })) break;
      }
      break;
    }
    case "WebSocket message": {
      const start = q.messageIndex ?? 0;
      if (q.messageIndex != null && start >= flow.wsMessages.length) throw new Error("messageIndex is outside the retained WebSocket messages");
      for (let index = start; index < flow.wsMessages.length; index++) {
        const message = flow.wsMessages[index]!;
        if (index - start >= 2000) { result.hasMore = true; result.nextMessageIndex = index; break; }
        if (message.opcode === "text" && !add({ text: message.data, structured: true, messageIndex: index, truncated: message.size > encode(message.data).length })) break;
      }
    }
  }
  return result;
}
function sensitive(key: string): boolean {
  const normalized = key.replace(/[^a-z0-9]/gi, "").toLowerCase();
  return ["authorization", "cookie", "password", "passwd", "secret", "token", "apikey", "credential", "sessionid"].some(part => normalized.includes(part));
}
function redactUrl(text: string): string {
  const relative = text.startsWith("/");
  let url: URL;
  try { url = new URL(relative ? `http://hamsy.invalid${text}` : text); } catch { return text; }
  if (!["http:", "https:", "ws:", "wss:"].includes(url.protocol)) return text;
  if (url.username) url.username = "[REDACTED]";
  if (url.password) url.password = "[REDACTED]";
  if (url.search) {
    const pairs = [...url.searchParams].map(([key, value]) => [key, sensitive(key) ? "[REDACTED]" : value] as const);
    url.search = ""; for (const [key, value] of pairs) url.searchParams.append(key, value);
  }
  if (url.hash) url.hash = "[REDACTED]";
  return relative ? `${url.pathname}${url.search}${url.hash}` : url.toString();
}
/** Same known-key policy as agent_privacy; body parsing precedes all cropping. */
function sanitize(value: unknown, depth = 0): unknown {
  if (depth > 128) throw new Error("Structured context exceeds nesting limit");
  if (Array.isArray(value)) return value.map(item => sanitize(item, depth + 1));
  if (value === null || typeof value !== "object") return value;
  const obj = value as Record<string, unknown>;
  if (typeof obj.name === "string" && sensitive(obj.name) && "value" in obj) obj.value = "[REDACTED]";
  if ("kind" in obj && "data" in obj && "size" in obj) { obj.data = "[body omitted]"; obj.agentOmitted = true; }
  if ("mimeType" in obj) { delete obj.text; delete obj.encoding; delete obj.params; }
  for (const key of ["requestBody", "responseBody"]) {
    const condition = obj[key]; if (condition && typeof condition === "object" && "value" in condition) (condition as Record<string, unknown>).value = "[rule payload omitted]";
  }
  const replaces = ["replaceInRequestBody", "replaceInResponseBody"].includes(String(obj.type));
  if (["jsonPatchRequest", "jsonPatchResponse"].includes(String(obj.type)) && Array.isArray(obj.ops)) {
    for (const op of obj.ops) if (op && typeof op === "object" && op.value != null) op.value = "[rule payload omitted]";
  }
  for (const [key, item] of Object.entries(obj)) {
    if (sensitive(key)) { obj[key] = Array.isArray(item) ? [] : "[REDACTED]"; continue; }
    if (["wsMessages", "_webSocketMessages"].includes(key)) { obj[key] = []; continue; }
    if (typeof item === "string" && (["body", "replacement"].includes(key) || (replaces && ["find", "replace"].includes(key)))) { obj[key] = "[rule payload omitted]"; continue; }
    obj[key] = sanitize(typeof item === "string" && ["url", "path", "upstreamProxy", "value", "urlValue", "redirectURL", "to", "find", "replace"].includes(key) ? redactUrl(item) : item, depth + 1);
  }
  return obj;
}
function redactText(text: string): string {
  try { return JSON.stringify(sanitize(JSON.parse(text))); } catch (error) {
    if (/^[\s]*[\[{]/.test(text)) throw new Error("Complete valid structured data is required for context redaction");
    if (error instanceof Error && error.message.includes("nesting limit")) throw error;
  }
  const pairs = [...new URLSearchParams(text)];
  if (!pairs.some(([key]) => sensitive(key))) return text;
  return new URLSearchParams(pairs.map(([key, value]) => [key, sensitive(key) ? "[REDACTED]" : value])).toString();
}
function sourceText(source: Source): string {
  if (source.truncated || source.body?.truncated || source.body?.kind === "truncated") throw new Error("Context is unavailable for truncated bodies or WebSocket messages");
  let text = source.text;
  if (source.body?.kind === "none") return "";
  if (source.body?.kind === "base64") {
    try { text = new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(atob(text), c => c.charCodeAt(0))); }
    catch { return ""; }
    if (/[\x00-\x08\x0e-\x1f]/.test(text)) return "";
  }
  if (source.pair) {
    const pair = sanitize({ ...source.pair }) as HeaderPair;
    return `${pair.name}: ${pair.value}`;
  }
  return source.url ? redactUrl(text) : source.structured ? redactText(text) : text;
}
function excerpt(text: string, start: number, end: number, limit: number) {
  const bytes = encode(text), startByte = encode(text.slice(0, start)).length, endByte = encode(text.slice(0, end)).length;
  const boundary = (position: number) => position === bytes.length || (bytes[position]! & 0xc0) !== 0x80;
  let matchEnd = Math.min(endByte, startByte + limit);
  while (!boundary(matchEnd)) matchEnd--;
  const room = limit - (matchEnd - startByte);
  let before = Math.max(0, startByte - Math.floor(room / 2));
  while (!boundary(before)) before++;
  let after = Math.min(bytes.length, matchEnd + room - (startByte - before));
  while (!boundary(after)) after--;
  const decode = (a: number, b: number) => new TextDecoder().decode(bytes.slice(a, b));
  return { before: decode(before, startByte), match: decode(startByte, matchEnd), after: decode(matchEnd, after), truncatedBefore: before > 0, truncatedAfter: after < bytes.length, matchTruncated: matchEnd < endByte };
}
export function readContext(prepared: PreparedContext, q: ContextParams) {
  validate(q);
  const pattern = new RegExp(q.regex ? q.query : q.query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), q.caseSensitive ? "mu" : "imu");
  const matches = [];
  let scanned = 0, hasMore = prepared.hasMore, nextMessageIndex = prepared.nextMessageIndex;
  for (const source of prepared.sources) {
    if (matches.length >= 3) { hasMore = true; nextMessageIndex = source.messageIndex ?? null; break; }
    scanned++;
    const text = sourceText(source), match = pattern.exec(text);
    if (match) matches.push({ field: q.field, ...(source.messageIndex == null ? {} : { messageIndex: source.messageIndex }), ...excerpt(text, match.index, match.index + match[0].length, q.maxContextBytes ?? 512) });
  }
  return { flowId: prepared.flowId, field: q.field, matches, scanned, hasMore: nextMessageIndex != null, truncated: hasMore, nextMessageIndex, redacted: true };
}
export function getMatchContext(flow: Flow, q: ContextParams) { return readContext(prepareContext(flow, q), q); }
