// Wire contract shared with the Rust backend. All JSON is camelCase.
// This file is the single source of truth for types imported across the app.
// Keep it in sync with the backend's serde models — do not diverge.

export type FlowState = "pending" | "requesting" | "responding" | "complete" | "error";

export type ResourceType =
  | "document"
  | "stylesheet"
  | "script"
  | "image"
  | "font"
  | "xhr"
  | "json"
  | "media"
  | "webSocket"
  | "other";

export interface FlowSummary {
  id: string;
  seq: number;
  state: FlowState;
  startedAt: number; // ms epoch
  durationMs: number | null;
  method: string;
  scheme: string;
  host: string;
  port: number;
  path: string;
  url: string;
  httpVersion: string;
  status: number | null;
  statusText: string | null;
  mimeType: string | null;
  resourceType: ResourceType;
  requestSize: number;
  responseSize: number;
  clientAddr: string;
  matchedRules: string[];
  modified: boolean;
  error: string | null;
  websocket: boolean;
  fromCache: boolean;
  app: string | null;
}

export interface HeaderPair {
  name: string;
  value: string;
}

export type BodyKind = "text" | "base64" | "none" | "truncated";

export interface BodyPayload {
  kind: BodyKind;
  data: string;
  size: number;
  truncated: boolean;
  encoding: string | null;
}

export interface RequestRecord {
  method: string;
  url: string;
  httpVersion: string;
  headers: HeaderPair[];
  body: BodyPayload;
  query: HeaderPair[];
}

export interface ResponseRecord {
  status: number;
  statusText: string;
  httpVersion: string;
  headers: HeaderPair[];
  body: BodyPayload;
}

export interface Timings {
  blocked: number;
  dns: number;
  connect: number;
  ssl: number;
  send: number;
  wait: number;
  receive: number;
}

export interface WsMessage {
  direction: "send" | "recv";
  opcode: string;
  timestamp: number;
  data: string;
  size: number;
}

export interface TlsInfo {
  version: string | null;
  cipherSuite: string | null;
  alpn: string | null;
  sni: string | null;
  peerCertSubject: string | null;
  peerCertIssuer: string | null;
  notBefore: number | null;
  notAfter: number | null;
}

export interface Flow extends FlowSummary {
  request: RequestRecord | null;
  response: ResponseRecord | null;
  originalRequest: RequestRecord | null;
  originalResponse: ResponseRecord | null;
  timings: Timings;
  wsMessages: WsMessage[];
  serverAddr: string | null;
  tls: TlsInfo | null;
}

export type UrlOp = "any" | "contains" | "equals" | "startsWith" | "endsWith" | "regex" | "wildcard";
export type HeaderOp = "exists" | "absent" | "equals" | "contains" | "regex";

export interface HeaderCond {
  name: string;
  op: HeaderOp;
  value: string | null;
}

export interface BodyCond {
  op: "contains" | "regex" | "equals";
  value: string;
}

export interface Matcher {
  urlOp: UrlOp;
  urlValue: string;
  methods: string[];
  hostPorts: string[];
  statusCodes: string[];
  resourceTypes: ResourceType[];
  requestHeaders: HeaderCond[];
  responseHeaders: HeaderCond[];
  requestBody: BodyCond | null;
  responseBody: BodyCond | null;
}

export type PayloadEncoding = "text" | "base64";

export interface JsonOp {
  op: "set" | "remove" | "merge" | "append";
  path: string;
  value?: unknown;
}

export type Action =
  | { type: "redirect"; to: string }
  | { type: "rewriteUrl"; find: string; replace: string; regex: boolean }
  | { type: "setQueryParam"; name: string; value: string }
  | { type: "removeQueryParam"; name: string }
  | { type: "setRequestHeader"; name: string; value: string }
  | { type: "removeRequestHeader"; name: string }
  | { type: "setResponseHeader"; name: string; value: string }
  | { type: "removeResponseHeader"; name: string }
  | { type: "setRequestBody"; body: string; encoding: PayloadEncoding; contentType: string | null }
  | { type: "setResponseBody"; body: string; encoding: PayloadEncoding; contentType: string | null }
  | { type: "replaceInRequestBody"; find: string; replace: string; regex: boolean }
  | { type: "replaceInResponseBody"; find: string; replace: string; regex: boolean }
  | { type: "jsonPatchRequest"; ops: JsonOp[] }
  | { type: "jsonPatchResponse"; ops: JsonOp[] }
  | { type: "mockResponse"; status: number; headers: HeaderPair[]; body: string; encoding: PayloadEncoding; delayMs: number }
  | { type: "setStatus"; status: number }
  | { type: "block"; reason: string }
  | { type: "delay"; ms: number }
  | { type: "throttle"; bytesPerSec: number }
  | { type: "setMethod"; method: string };

export interface Rule {
  id: string;
  name: string;
  enabled: boolean;
  priority: number;
  group: string | null;
  notes: string | null;
  match: Matcher;
  actions: Action[];
}

export interface Settings {
  proxyPort: number;
  uiPort: number;
  bindAddr: string;
  maxFlows: number;
  maxBodyBytes: number;
  interceptHttps: boolean;
  passthroughHosts: string[];
  /** Names of built-in preset groups (see `PassthroughPreset`) whose hosts are unioned with `passthroughHosts`. */
  passthroughPresets: string[];
  captureIncludeHosts: string[];
  captureExcludeHosts: string[];
  manualProxy: boolean;
  captureWebsockets: boolean;
  theme: string;
  upstreamProxy: string | null;
  paused: boolean;
}

/** One entry of `GET /api/presets/passthrough`. */
export interface PassthroughPreset {
  name: string;
  label: string;
  description: string;
  hosts: string[];
}

export interface ApiState {
  version: string;
  proxyPort: number;
  uiPort: number;
  capturing: boolean;
  paused: boolean;
  flowCount: number;
  caFingerprint: string;
  uptimeSecs: number;
  systemProxy: { enabled: boolean; platform: string; supported: boolean };
}

export interface SetupInfo {
  proxyHost: string;
  proxyPort: number;
  lanAddresses: string[];
  certUrl: string;
  caFingerprint: string;
  qrSvg: string;
}

export type WsServerMessage =
  | { type: "flow"; flow: FlowSummary }
  | { type: "flows"; flows: FlowSummary[] }
  | { type: "flowDetail"; flow: Flow }
  | { type: "wsMessage"; flowId: string; message: WsMessage }
  | { type: "cleared" }
  | { type: "state"; state: unknown }
  | { type: "rulesChanged" }
  | { type: "settingsChanged"; settings: Settings }
  | { type: "notice"; level: string; message: string };

export type WsClientMessage =
  | { type: "pause"; paused: boolean }
  | { type: "clear" }
  | { type: "subscribe"; filter?: string }
  | { type: "ping" };

export interface FlowListParams {
  limit?: number;
  afterSeq?: number;
  q?: string;
  methods?: string[];
  statusClass?: string[];
  resourceTypes?: string[];
  host?: string;
  onlyModified?: boolean;
  app?: string;
}
