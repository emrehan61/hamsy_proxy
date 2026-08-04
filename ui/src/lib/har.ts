// HAR 1.2 parser/exporter — pure functions, no side effects, no store or
// component imports. This is the client-side counterpart to
// crates/hamsy-core/src/har.rs: `parseHar`/`parseHarText` are the mirror
// of `import_har`/`parse_entry`, and `flowsToHar` mirrors `export_har`, so
// hamsy-proxy-exported HARs round-trip through this module with the same
// `_hamsy` extension (`matchedRules`, `modified`, `flowId`, `resourceType`).
//
// Unlike the Rust importer (which drops any entry missing a top-level
// `response` object, and never reads HAR `timings` at all), this parser is
// deliberately more permissive: only `request.method`/`request.url` are
// required per entry, everything else — including a missing `response` —
// degrades to sensible defaults so HAR files from Chrome DevTools, Safari,
// Firefox, Charles, Fiddler, Proxyman, and Insomnia all import without
// throwing. See the per-field comments below for where this intentionally
// diverges from `har.rs`.

import type {
  BodyKind,
  BodyPayload,
  Flow,
  FlowState,
  HeaderPair,
  ResourceType,
  WsMessage,
} from "./types";

export interface HarPageInfo {
  id: string;
  title: string;
  startedDateTime: string;
  onContentLoad: number | null;
  onLoad: number | null;
}

export interface HarParseResult {
  flows: Flow[];
  pages: HarPageInfo[];
  creatorName: string;
  creatorVersion: string;
  harVersion: string;
  skipped: { index: number; reason: string }[];
}

// ---- generic JSON helpers ----

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

const RESOURCE_TYPES: readonly ResourceType[] = [
  "document",
  "stylesheet",
  "script",
  "image",
  "font",
  "xhr",
  "json",
  "media",
  "webSocket",
  "other",
];

function isResourceType(v: string): v is ResourceType {
  return (RESOURCE_TYPES as readonly string[]).includes(v);
}

// ---- ids ----

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function isUuid(s: string): boolean {
  return UUID_RE.test(s);
}

/** `crypto.randomUUID()` with a manual v4 fallback for non-secure contexts. */
function generateUuid(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {
    crypto.getRandomValues(bytes);
  } else {
    for (let i = 0; i < 16; i += 1) bytes[i] = Math.floor(Math.random() * 256);
  }
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0"));
  return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex.slice(6, 8).join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10, 16).join("")}`;
}

// ---- body payload construction (mirrors body.rs's `to_payload`, applied to
// already-decoded HAR text — HAR defines no request-body binary encoding, so
// `postData.text` is always literal content re-classified by its own bytes
// and `mimeType`; see body.rs's doc comment on `parse_post_data`) ----

function utf8ByteLength(s: string): number {
  return new TextEncoder().encode(s).length;
}

function hasControlChars(s: string): boolean {
  for (let i = 0; i < s.length; i += 1) {
    const c = s.charCodeAt(i);
    if (c < 0x20 && c !== 9 && c !== 10 && c !== 13) return true;
  }
  return false;
}

/** Mirrors body.rs's `is_textual_mime`. */
function isTextualMime(mime: string): boolean {
  const m = mime.split(";")[0]?.trim().toLowerCase() ?? "";
  if (m.startsWith("text/")) return true;
  if (m.endsWith("+json") || m.endsWith("+xml")) return true;
  return (
    m === "application/json" ||
    m === "application/xml" ||
    m === "application/xhtml+xml" ||
    m === "application/javascript" ||
    m === "application/x-javascript" ||
    m === "application/ecmascript" ||
    m === "application/x-www-form-urlencoded" ||
    m === "application/graphql" ||
    m === "application/manifest+json" ||
    m === "image/svg+xml"
  );
}

function utf8ToBase64(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) binary += String.fromCharCode(bytes[i] ?? 0);
  return btoa(binary);
}

function base64DecodedByteLength(b64: string): number {
  const clean = b64.replace(/\s/g, "");
  if (clean.length === 0) return 0;
  const padding = clean.endsWith("==") ? 2 : clean.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((clean.length * 3) / 4) - padding);
}

function noneBody(): BodyPayload {
  return { kind: "none", data: "", size: 0, truncated: false, encoding: null };
}

function base64Body(text: string, declaredSize: number | null): BodyPayload {
  return {
    kind: "base64",
    data: text,
    size: declaredSize ?? base64DecodedByteLength(text),
    truncated: false,
    encoding: null,
  };
}

/** Classifies literal text as `text` or `base64`, exactly like `to_payload`'s decision (minus truncation, which import never needs). */
function classifyText(text: string, mimeType: string | null, declaredSize: number | null): BodyPayload {
  if (text.length === 0) {
    return { kind: "none", data: "", size: declaredSize ?? 0, truncated: false, encoding: null };
  }
  const size = declaredSize ?? utf8ByteLength(text);
  const textual = mimeType !== null && isTextualMime(mimeType);
  if (textual || !hasControlChars(text)) {
    return { kind: "text", data: text, size, truncated: false, encoding: null };
  }
  return { kind: "base64", data: utf8ToBase64(text), size, truncated: false, encoding: null };
}

/** Builds a request body from HAR `request.postData`. Handles the `text` form, Chrome's `params` (urlencoded) form, and Charles/Proxyman's `encoding: "base64"`. */
function parseRequestBody(postData: unknown): BodyPayload {
  if (!isRecord(postData)) return noneBody();
  const mimeType = typeof postData.mimeType === "string" ? postData.mimeType : null;

  if (typeof postData.text === "string") {
    if (postData.encoding === "base64") return base64Body(postData.text, null);
    return classifyText(postData.text, mimeType, null);
  }

  if (Array.isArray(postData.params)) {
    const encoded = postData.params
      .filter((p): p is Record<string, unknown> => isRecord(p) && typeof p.name === "string")
      .map((p) => `${encodeURIComponent(p.name as string)}=${encodeURIComponent(typeof p.value === "string" ? p.value : "")}`)
      .join("&");
    if (encoded.length > 0) {
      return classifyText(encoded, mimeType ?? "application/x-www-form-urlencoded", null);
    }
  }

  return noneBody();
}

/**
 * Builds a response body from HAR `response.content`. Per the HAR spec,
 * `size` is the (possibly compressed-original) size the exporter reported —
 * unlike request bodies, we trust it over the local text length when
 * present, falling back to the text length only when `size` is absent.
 * `content.size > 0` with no `text` (some exporters drop bodies to keep HAR
 * files small) must still yield `kind: "none"`, never a fabricated empty
 * text body.
 */
function parseResponseBody(content: unknown): BodyPayload {
  if (!isRecord(content)) return noneBody();
  const mimeType = typeof content.mimeType === "string" ? content.mimeType : null;
  const text = typeof content.text === "string" ? content.text : "";
  const declaredSize = typeof content.size === "number" && Number.isFinite(content.size) && content.size >= 0 ? content.size : null;

  if (text.length === 0) {
    return { kind: "none", data: "", size: declaredSize ?? 0, truncated: false, encoding: null };
  }
  if (content.encoding === "base64") {
    return base64Body(text, declaredSize);
  }
  return classifyText(text, mimeType, declaredSize);
}

// ---- headers / query ----

function parseHeaderPairs(value: unknown): HeaderPair[] {
  if (!Array.isArray(value)) return [];
  const out: HeaderPair[] = [];
  for (const item of value) {
    if (!isRecord(item)) continue;
    const name = item.name;
    const val = item.value;
    if (typeof name !== "string") continue;
    if (typeof val === "string") out.push({ name, value: val });
    else if (typeof val === "number" || typeof val === "boolean") out.push({ name, value: String(val) });
  }
  return out;
}

/** Uses `request.queryString` when present (even empty — an empty array is a valid "no query params" answer); otherwise derives params from the URL. */
function parseQuery(request: Record<string, unknown>, urlStr: string): HeaderPair[] {
  if (Array.isArray(request.queryString)) {
    return parseHeaderPairs(request.queryString);
  }
  try {
    const u = new URL(urlStr);
    const out: HeaderPair[] = [];
    u.searchParams.forEach((value, name) => out.push({ name, value }));
    return out;
  } catch {
    return [];
  }
}

// ---- URL ----

function defaultPortForScheme(scheme: string): number {
  switch (scheme) {
    case "https":
    case "wss":
      return 443;
    case "http":
    case "ws":
      return 80;
    case "ftp":
      return 21;
    default:
      return 0;
  }
}

function parseUrlParts(urlStr: string): { scheme: string; host: string; port: number; path: string } {
  try {
    const u = new URL(urlStr);
    const scheme = u.protocol.replace(/:$/, "") || "http";
    const host = u.hostname;
    const port = u.port !== "" ? Number(u.port) : defaultPortForScheme(scheme);
    const path = u.pathname + u.search;
    return { scheme, host, port, path };
  } catch {
    return { scheme: "http", host: "", port: 0, path: "" };
  }
}

// ---- resource type (mirrors flow.rs's `ResourceType::infer`) ----

function inferResourceType(mime: string | null, path: string): ResourceType {
  const pathNoQuery = path.split(/[?#]/)[0] ?? path;
  const lastDot = pathNoQuery.lastIndexOf(".");
  const rawExt = lastDot >= 0 ? pathNoQuery.slice(lastDot + 1) : "";
  const ext = rawExt.includes("/") ? "" : rawExt.toLowerCase();

  if (mime !== null) {
    const m = mime.split(";")[0]?.trim().toLowerCase() ?? "";
    switch (m) {
      case "text/html":
      case "application/xhtml+xml":
        return "document";
      case "text/css":
        return "stylesheet";
      case "application/javascript":
      case "text/javascript":
      case "application/x-javascript":
      case "application/ecmascript":
        return "script";
      case "application/json":
      case "application/ld+json":
        return "json";
      default:
        break;
    }
    if (m.startsWith("image/")) return "image";
    if (m.startsWith("font/") || m === "application/font-woff" || m === "application/x-font-ttf" || m === "application/vnd.ms-fontobject") {
      return "font";
    }
    if (m.startsWith("audio/") || m.startsWith("video/")) return "media";
  }

  switch (ext) {
    case "html":
    case "htm":
      return "document";
    case "css":
      return "stylesheet";
    case "js":
    case "mjs":
    case "cjs":
      return "script";
    case "json":
      return "json";
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "webp":
    case "svg":
    case "ico":
    case "bmp":
    case "avif":
      return "image";
    case "woff":
    case "woff2":
    case "ttf":
    case "otf":
    case "eot":
      return "font";
    case "mp3":
    case "mp4":
    case "wav":
    case "ogg":
    case "webm":
    case "m4a":
    case "mov":
    case "avi":
      return "media";
    default:
      return "other";
  }
}

// ---- timings ----
//
// NOTE: `import_har` in har.rs currently never reads HAR `timings` at all
// (imported flows keep the all-zero `Timings::default()`). We deliberately
// go further here and parse them, since a HAR viewer without a working
// timing breakdown would be a regression relative to what real HAR tooling
// shows; -1 ("not applicable" per the HAR 1.2 spec) clamps to 0 like the
// rest of this parser's negative-duration handling.

function parseTimings(t: unknown): Flow["timings"] {
  const obj = isRecord(t) ? t : {};
  const pick = (k: string): number => {
    const v = obj[k];
    if (typeof v !== "number" || !Number.isFinite(v)) return 0;
    return v < 0 ? 0 : v;
  };
  return {
    blocked: pick("blocked"),
    dns: pick("dns"),
    connect: pick("connect"),
    ssl: pick("ssl"),
    send: pick("send"),
    wait: pick("wait"),
    receive: pick("receive"),
  };
}

// ---- misc entry-level fields ----

function parseIsoToMs(s: string): number | null {
  const t = Date.parse(s);
  return Number.isFinite(t) ? t : null;
}

function deriveServerAddr(entry: Record<string, unknown>): string | null {
  const ip = typeof entry.serverIPAddress === "string" && entry.serverIPAddress.length > 0 ? entry.serverIPAddress : null;
  if (ip === null) return null;
  const conn = entry.connection;
  const port = typeof conn === "string" && conn.length > 0 ? conn : typeof conn === "number" ? String(conn) : null;
  return port !== null ? `${ip}:${port}` : ip;
}

function deriveFromCache(entry: Record<string, unknown>): boolean {
  const cache = entry.cache;
  if (isRecord(cache) && isRecord(cache.afterRequest) && Object.keys(cache.afterRequest).length > 0) {
    return true;
  }
  return Boolean(entry._fromCache);
}

/** Content-Type response header wins (mirrors flow.rs's `mark_complete`, which derives `mime_type` from the header, not `content.mimeType` directly); `content.mimeType` is a fallback for foreign HARs whose `response.headers` omit it. */
function deriveMimeType(respHeaders: HeaderPair[], response: Record<string, unknown> | undefined): string | null {
  const headerVal = respHeaders.find((h) => h.name.toLowerCase() === "content-type")?.value;
  if (headerVal !== undefined) {
    const stripped = headerVal.split(";")[0]?.trim();
    if (stripped) return stripped;
  }
  if (response !== undefined && isRecord(response.content) && typeof response.content.mimeType === "string") {
    const stripped = response.content.mimeType.split(";")[0]?.trim();
    if (stripped) return stripped;
  }
  return null;
}

function normalizeOpcode(v: unknown): string {
  if (typeof v === "string" && v.length > 0) return v;
  if (typeof v === "number") {
    switch (v) {
      case 1:
        return "text";
      case 2:
        return "binary";
      case 8:
        return "close";
      case 9:
        return "ping";
      case 10:
        return "pong";
      default:
        return String(v);
    }
  }
  return "text";
}

/** Parses Chrome DevTools' `_webSocketMessages` extension (`{type: "send"|"receive", time, opcode, data}[]`). */
function parseWsMessages(entry: Record<string, unknown>, fallbackTimestamp: number): WsMessage[] {
  const raw = entry._webSocketMessages;
  if (!Array.isArray(raw)) return [];
  const out: WsMessage[] = [];
  for (const m of raw) {
    if (!isRecord(m)) continue;
    const direction: "send" | "recv" = m.type === "send" ? "send" : "recv";
    const data = typeof m.data === "string" ? m.data : "";
    const opcode = normalizeOpcode(m.opcode);
    const timeSec = typeof m.time === "number" && Number.isFinite(m.time) ? m.time : null;
    const timestamp = timeSec !== null ? Math.round(timeSec * 1000) : fallbackTimestamp;
    const size = opcode === "binary" ? base64DecodedByteLength(data) : utf8ByteLength(data);
    out.push({ direction, opcode, timestamp, data, size });
  }
  return out;
}

// ---- entry parsing ----

function parseEntry(entryValue: unknown, index: number): Flow | { skippedReason: string } {
  if (!isRecord(entryValue)) return { skippedReason: "entry is not an object" };
  const entry = entryValue;

  const request = isRecord(entry.request) ? entry.request : undefined;
  if (!request) return { skippedReason: "missing request object" };

  const method = typeof request.method === "string" ? request.method : undefined;
  const urlStr = typeof request.url === "string" ? request.url : undefined;
  if (!method || !urlStr) return { skippedReason: "missing request.method or request.url" };

  const response = isRecord(entry.response) ? entry.response : undefined;
  const hasResponse = response !== undefined;

  const httpVersion = typeof request.httpVersion === "string" ? request.httpVersion : "HTTP/1.1";
  const reqHeaders = parseHeaderPairs(request.headers);
  const query = parseQuery(request, urlStr);
  const reqBody = parseRequestBody(request.postData);

  const status = hasResponse && typeof response.status === "number" && Number.isFinite(response.status) ? response.status : 0;
  const statusText = hasResponse && typeof response.statusText === "string" ? response.statusText : "";
  const respHttpVersion = hasResponse && typeof response.httpVersion === "string" ? response.httpVersion : httpVersion;
  const respHeaders = hasResponse ? parseHeaderPairs(response.headers) : [];
  const respBody = hasResponse ? parseResponseBody(response.content) : noneBody();

  const { scheme, host, port, path } = parseUrlParts(urlStr);

  const ext = isRecord(entry._hamsy) ? entry._hamsy : undefined;
  const flowId = typeof ext?.flowId === "string" && isUuid(ext.flowId) ? ext.flowId : generateUuid();
  const matchedRules = Array.isArray(ext?.matchedRules) ? ext.matchedRules.filter((x): x is string => typeof x === "string") : [];
  const modified = typeof ext?.modified === "boolean" ? ext.modified : false;
  const extResourceType = typeof ext?.resourceType === "string" ? ext.resourceType : undefined;

  const startedAt = typeof entry.startedDateTime === "string" ? (parseIsoToMs(entry.startedDateTime) ?? 0) : 0;
  const rawDuration = typeof entry.time === "number" && Number.isFinite(entry.time) ? entry.time : null;
  const durationMs = rawDuration !== null ? Math.max(0, Math.round(rawDuration)) : null;

  const mimeType = deriveMimeType(respHeaders, response);

  const isWsEntry = entry._resourceType === "websocket" || Array.isArray(entry._webSocketMessages);
  const resourceType: ResourceType =
    extResourceType !== undefined && isResourceType(extResourceType)
      ? extResourceType
      : isWsEntry
        ? "webSocket"
        : inferResourceType(mimeType, path);
  const websocket = scheme === "ws" || scheme === "wss" || resourceType === "webSocket";

  const reqBodySizeDeclared = typeof request.bodySize === "number" && request.bodySize >= 0 ? request.bodySize : null;
  const requestSize = reqBodySizeDeclared ?? reqBody.size;
  const respBodySizeDeclared = hasResponse && typeof response.bodySize === "number" && response.bodySize >= 0 ? response.bodySize : null;
  const responseSize = respBodySizeDeclared ?? respBody.size;

  // HAR encodes a failed/aborted request as `response.status === 0` (Chrome,
  // Safari, and friends all do this); a genuinely complete flow always has a
  // response object with a real status.
  const stateComplete = hasResponse && status > 0;
  const state: FlowState = stateComplete ? "complete" : "error";
  let error: string | null = null;
  if (!stateComplete) {
    if (!hasResponse) {
      error = "No response recorded";
    } else {
      const chromeError =
        (typeof response._error === "string" && response._error) ||
        (typeof response._errorMessage === "string" && response._errorMessage) ||
        null;
      error = chromeError || "Request failed (status 0)";
    }
  }

  const flow: Flow = {
    id: flowId,
    seq: index,
    state,
    startedAt,
    durationMs,
    method,
    scheme,
    host,
    port,
    path,
    url: urlStr,
    httpVersion,
    status: hasResponse ? status : null,
    statusText: hasResponse ? statusText : null,
    mimeType,
    resourceType,
    requestSize,
    responseSize,
    clientAddr: "",
    matchedRules,
    modified,
    error,
    websocket,
    fromCache: deriveFromCache(entry),
    app: null,
    request: { method, url: urlStr, httpVersion, headers: reqHeaders, body: reqBody, query },
    response: hasResponse ? { status, statusText, httpVersion: respHttpVersion, headers: respHeaders, body: respBody } : null,
    originalRequest: null,
    originalResponse: null,
    timings: parseTimings(entry.timings),
    wsMessages: parseWsMessages(entry, startedAt),
    serverAddr: deriveServerAddr(entry),
    tls: null,
  };
  return flow;
}

function parsePageInfo(p: unknown): HarPageInfo | null {
  if (!isRecord(p)) return null;
  const id = typeof p.id === "string" ? p.id : "";
  const title = typeof p.title === "string" ? p.title : id;
  const startedDateTime = typeof p.startedDateTime === "string" ? p.startedDateTime : "";
  const timings = isRecord(p.pageTimings) ? p.pageTimings : undefined;
  const onContentLoad = typeof timings?.onContentLoad === "number" && timings.onContentLoad >= 0 ? timings.onContentLoad : null;
  const onLoad = typeof timings?.onLoad === "number" && timings.onLoad >= 0 ? timings.onLoad : null;
  return { id, title, startedDateTime, onContentLoad, onLoad };
}

// ---- public parse API ----

/** Parses an already-`JSON.parse`d HAR document. Throws only when `log.entries` is missing/not an array — everything else degrades per-entry into `skipped`. */
export function parseHar(doc: unknown): HarParseResult {
  const log = isRecord(doc) ? doc.log : undefined;
  if (!isRecord(log) || !Array.isArray(log.entries)) {
    throw new Error("Not a valid HAR file: missing log.entries");
  }

  const flows: Flow[] = [];
  const skipped: { index: number; reason: string }[] = [];
  log.entries.forEach((entry: unknown, i: number) => {
    try {
      const result = parseEntry(entry, i);
      if ("skippedReason" in result) {
        skipped.push({ index: i, reason: result.skippedReason });
      } else {
        flows.push(result);
      }
    } catch (err) {
      skipped.push({ index: i, reason: err instanceof Error ? err.message : "unknown parse error" });
    }
  });

  const pages = Array.isArray(log.pages)
    ? log.pages.map(parsePageInfo).filter((p): p is HarPageInfo => p !== null)
    : [];
  const creator = isRecord(log.creator) ? log.creator : undefined;
  const creatorName = typeof creator?.name === "string" ? creator.name : "";
  const creatorVersion = typeof creator?.version === "string" ? creator.version : "";
  const harVersion = typeof log.version === "string" ? log.version : "1.1";

  return { flows, pages, creatorName, creatorVersion, harVersion, skipped };
}

/** `JSON.parse` + `parseHar`; throws `Error` on invalid JSON as well as an invalid HAR shape. */
export function parseHarText(text: string): HarParseResult {
  let doc: unknown;
  try {
    doc = JSON.parse(text);
  } catch (err) {
    throw new Error(`Failed to parse HAR file: ${err instanceof Error ? err.message : "invalid JSON"}`);
  }
  return parseHar(doc);
}

// ---- export (mirrors har.rs's `export_har`/`export_entry`) ----

function headerToJson(h: HeaderPair): { name: string; value: string } {
  return { name: h.name, value: h.value };
}

function parseCookieHeader(value: string): { name: string; value: string }[] {
  const out: { name: string; value: string }[] = [];
  for (const rawPair of value.split(";")) {
    const pair = rawPair.trim();
    if (pair.length === 0) continue;
    const idx = pair.indexOf("=");
    if (idx < 0) continue;
    out.push({ name: pair.slice(0, idx).trim(), value: pair.slice(idx + 1).trim() });
  }
  return out;
}

function parseSetCookieHeader(value: string): Record<string, unknown> {
  const parts = value.split(";");
  const first = (parts[0] ?? "").trim();
  const eqIdx = first.indexOf("=");
  const name = eqIdx >= 0 ? first.slice(0, eqIdx) : first;
  const val = eqIdx >= 0 ? first.slice(eqIdx + 1) : "";

  let path: string | null = null;
  let domain: string | null = null;
  let expires: string | null = null;
  let httpOnly = false;
  let secure = false;

  for (const rawPart of parts.slice(1)) {
    const part = rawPart.trim();
    if (part.length === 0) continue;
    const idx = part.indexOf("=");
    if (idx >= 0) {
      const k = part.slice(0, idx).trim().toLowerCase();
      const v = part.slice(idx + 1).trim();
      if (k === "path") path = v;
      else if (k === "domain") domain = v;
      else if (k === "expires") expires = v;
    } else {
      const lower = part.toLowerCase();
      if (lower === "httponly") httpOnly = true;
      else if (lower === "secure") secure = true;
    }
  }

  return { name: name.trim(), value: val.trim(), path, domain, expires, httpOnly, secure };
}

function formatStartTime(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "1970-01-01T00:00:00.000Z";
  return date.toISOString();
}

function splitServerAddr(addr: string | null): [string | null, string | null] {
  if (addr === null) return [null, null];
  const idx = addr.lastIndexOf(":");
  if (idx < 0) return [addr, null];
  return [addr.slice(0, idx), addr.slice(idx + 1)];
}

function exportEntry(flow: Flow): unknown {
  const started = formatStartTime(flow.startedAt);
  const timeMs = flow.durationMs ?? 0;

  const request = flow.request;
  const response = flow.response;

  const reqHeaders = request ? request.headers.map(headerToJson) : [];
  const cookieHeader = request?.headers.find((h) => h.name.toLowerCase() === "cookie");
  const reqCookies = cookieHeader ? parseCookieHeader(cookieHeader.value) : [];
  const queryString = request ? request.query.map(headerToJson) : [];

  const requestJson: Record<string, unknown> = {
    method: flow.method,
    url: flow.url,
    httpVersion: flow.httpVersion,
    cookies: reqCookies,
    headers: reqHeaders,
    queryString,
    headersSize: -1,
    bodySize: request?.body.size ?? 0,
  };
  if (request && request.body.kind !== ("none" satisfies BodyKind)) {
    const mime = request.headers.find((h) => h.name.toLowerCase() === "content-type")?.value ?? "application/octet-stream";
    requestJson.postData = { mimeType: mime, text: request.body.data };
  }

  const respHeaders = response ? response.headers.map(headerToJson) : [];
  const respCookies = response
    ? response.headers.filter((h) => h.name.toLowerCase() === "set-cookie").map((h) => parseSetCookieHeader(h.value))
    : [];
  const redirectUrl = response?.headers.find((h) => h.name.toLowerCase() === "location")?.value ?? "";

  const content: Record<string, unknown> = {
    size: response?.body.size ?? 0,
    mimeType: flow.mimeType ?? "application/octet-stream",
    // Only the decoded (logical) body size is tracked, not the original
    // wire-compressed size, so real bytes-saved can't be computed here —
    // mirrors export_entry's identical comment/choice in har.rs.
    compression: 0,
  };
  if (response && response.body.kind !== ("none" satisfies BodyKind)) {
    content.text = response.body.data;
    if (response.body.kind === ("base64" satisfies BodyKind)) content.encoding = "base64";
  }

  const responseJson = {
    status: response?.status ?? 0,
    statusText: response?.statusText ?? "",
    httpVersion: response?.httpVersion ?? flow.httpVersion,
    cookies: respCookies,
    headers: respHeaders,
    content,
    redirectURL: redirectUrl,
    headersSize: -1,
    bodySize: response?.body.size ?? 0,
  };

  const [serverIp, connection] = splitServerAddr(flow.serverAddr);

  return {
    startedDateTime: started,
    time: timeMs,
    request: requestJson,
    response: responseJson,
    cache: {},
    timings: {
      blocked: flow.timings.blocked,
      dns: flow.timings.dns,
      connect: flow.timings.connect,
      ssl: flow.timings.ssl,
      send: flow.timings.send,
      wait: flow.timings.wait,
      receive: flow.timings.receive,
    },
    serverIPAddress: serverIp,
    connection,
    _hamsy: {
      matchedRules: flow.matchedRules,
      modified: flow.modified,
      flowId: flow.id,
      resourceType: flow.resourceType,
    },
  };
}

/** Exports `flows` as a HAR 1.2 document, mirroring `export_har`/`export_entry` in har.rs field-for-field (including the `_hamsy` extension). */
export function flowsToHar(flows: Flow[], creatorVersion = "0.1.0"): unknown {
  return {
    log: {
      version: "1.2",
      creator: { name: "hamsy-proxy", version: creatorVersion },
      browser: { name: "hamsy-proxy", version: creatorVersion },
      pages: [],
      entries: flows.map(exportEntry),
    },
  };
}
