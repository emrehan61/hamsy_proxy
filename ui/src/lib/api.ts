// Typed fetch client for the hamsy-proxy REST API. One function per endpoint.
// All requests are same-origin (dev server proxies /api to the Rust backend).

import type {
  ApiState,
  Flow,
  FlowListParams,
  FlowSummary,
  PassthroughPreset,
  RequestRecord,
  Rule,
  Settings,
  SetupInfo,
} from "./types";

const BASE = "/api";

export class ApiError extends Error {
  status: number;
  body: unknown;

  constructor(status: number, message: string, body?: unknown) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

/** Rule payload accepted for create: full Rule minus id, id optional (server assigns if absent). */
export type RuleInput = Omit<Rule, "id"> & { id?: string };

function buildQuery(params: Record<string, string | number | boolean | string[] | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null) continue;
    if (Array.isArray(value)) {
      if (value.length === 0) continue;
      search.set(key, value.join(","));
    } else if (typeof value === "boolean") {
      if (value) search.set(key, "true");
    } else if (value === "") {
      continue;
    } else {
      search.set(key, String(value));
    }
  }
  const qs = search.toString();
  return qs ? `?${qs}` : "";
}

async function parseErrorBody(res: Response): Promise<unknown> {
  const text = await res.text().catch(() => "");
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    credentials: "same-origin",
    ...init,
    headers: {
      ...(init?.body ? { "Content-Type": "application/json" } : {}),
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    const body = await parseErrorBody(res);
    throw new ApiError(res.status, `${init?.method ?? "GET"} ${path} failed with ${res.status}`, body);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}

// ---- state ----

export function getState(): Promise<ApiState> {
  return request<ApiState>("/state");
}

// ---- flows ----

export function listFlows(params: FlowListParams = {}): Promise<{ flows: FlowSummary[] }> {
  const qs = buildQuery({
    limit: params.limit,
    afterSeq: params.afterSeq,
    q: params.q,
    methods: params.methods,
    statusClass: params.statusClass,
    resourceTypes: params.resourceTypes,
    host: params.host,
    onlyModified: params.onlyModified,
  });
  return request<{ flows: FlowSummary[] }>(`/flows${qs}`);
}

export function getFlow(id: string): Promise<Flow> {
  return request<Flow>(`/flows/${encodeURIComponent(id)}`);
}

export function clearFlows(): Promise<void> {
  return request<void>("/flows", { method: "DELETE" });
}

export function replayFlow(id: string, edited?: RequestRecord): Promise<{ id: string }> {
  return request<{ id: string }>(`/flows/${encodeURIComponent(id)}/replay`, {
    method: "POST",
    body: edited ? JSON.stringify(edited) : undefined,
  });
}

// ---- rules ----

export function listRules(): Promise<{ rules: Rule[] }> {
  return request<{ rules: Rule[] }>("/rules");
}

export function createRule(rule: RuleInput): Promise<Rule> {
  return request<Rule>("/rules", { method: "POST", body: JSON.stringify(rule) });
}

export function updateRule(id: string, rule: RuleInput): Promise<Rule> {
  return request<Rule>(`/rules/${encodeURIComponent(id)}`, { method: "PUT", body: JSON.stringify(rule) });
}

export function deleteRule(id: string): Promise<void> {
  return request<void>(`/rules/${encodeURIComponent(id)}`, { method: "DELETE" });
}

export function toggleRule(id: string): Promise<Rule> {
  return request<Rule>(`/rules/${encodeURIComponent(id)}/toggle`, { method: "POST" });
}

export function reorderRules(ids: string[]): Promise<void> {
  return request<void>("/rules/reorder", { method: "POST", body: JSON.stringify({ ids }) });
}

export function importRules(rules: RuleInput[], replace: boolean): Promise<{ imported: number }> {
  return request<{ imported: number }>("/rules/import", { method: "POST", body: JSON.stringify({ rules, replace }) });
}

export function exportRules(): Promise<{ rules: Rule[] }> {
  return request<{ rules: Rule[] }>("/rules/export");
}

// ---- settings ----

export function getSettings(): Promise<Settings> {
  return request<Settings>("/settings");
}

/** PUT /api/settings accepts a PARTIAL patch — never send the whole object blindly. */
export function updateSettings(patch: Partial<Settings>): Promise<Settings & { restartRequired: boolean }> {
  return request<Settings & { restartRequired: boolean }>("/settings", { method: "PUT", body: JSON.stringify(patch) });
}

// ---- passthrough presets ----

export function getPassthroughPresets(): Promise<PassthroughPreset[]> {
  return request<PassthroughPreset[]>("/presets/passthrough");
}

// ---- system proxy ----

export function setSystemProxy(enabled: boolean): Promise<{ enabled: boolean }> {
  return request<{ enabled: boolean }>("/system-proxy", { method: "POST", body: JSON.stringify({ enabled }) });
}

// ---- setup ----

export function getSetupInfo(): Promise<SetupInfo> {
  return request<SetupInfo>("/setup");
}

// ---- har export ----

function parseFilenameFromContentDisposition(header: string | null): string | undefined {
  if (!header) return undefined;
  const starMatch = /filename\*=(?:UTF-8'')?([^;]+)/i.exec(header);
  if (starMatch?.[1]) {
    try {
      return decodeURIComponent(starMatch[1].trim().replace(/^"|"$/g, ""));
    } catch {
      return starMatch[1].trim().replace(/^"|"$/g, "");
    }
  }
  const plainMatch = /filename="?([^";]+)"?/i.exec(header);
  return plainMatch?.[1]?.trim();
}

export async function getHar(ids: string[]): Promise<{ blob: Blob; filename?: string }> {
  const qs = buildQuery({ ids });
  const res = await fetch(`${BASE}/har${qs}`, { credentials: "same-origin" });
  if (!res.ok) {
    const body = await parseErrorBody(res);
    throw new ApiError(res.status, `GET /har failed with ${res.status}`, body);
  }
  const blob = await res.blob();
  const filename = parseFilenameFromContentDisposition(res.headers.get("Content-Disposition"));
  return { blob, filename };
}

/** POST /api/har/import — body is a raw HAR 1.2 document (not wrapped). */
export function importHar(har: unknown): Promise<{ imported: number }> {
  return request<{ imported: number }>("/har/import", { method: "POST", body: JSON.stringify(har) });
}

const api = {
  getState,
  listFlows,
  getFlow,
  clearFlows,
  replayFlow,
  listRules,
  createRule,
  updateRule,
  deleteRule,
  toggleRule,
  reorderRules,
  importRules,
  exportRules,
  getSettings,
  updateSettings,
  getPassthroughPresets,
  setSystemProxy,
  getSetupInfo,
  getHar,
  importHar,
};

export default api;
