import type { Flow } from "./types";
import { harSearchFields } from "./harSearch";

export interface AgentSearchParams {
  query: string;
  regex?: boolean;
  caseSensitive?: boolean;
  limit?: number | null;
  afterSeq?: number | null;
  host?: string | null;
  excludedHosts?: string[];
  methods?: string | null;
  statusClass?: number | null;
}
export function searchSession(flows: Flow[], q: AgentSearchParams) {
  if (typeof q.query !== "string" || !q.query.length || new TextEncoder().encode(q.query).byteLength > 1024) throw new Error("query must contain 1–1024 UTF-8 bytes");
  const limit = q.limit ?? 50;
  if (!Number.isInteger(limit) || limit < 1 || limit > 200) throw new Error("limit must be 1–200");
  if (q.afterSeq != null && (!Number.isSafeInteger(q.afterSeq) || q.afterSeq < 0)) throw new Error("Invalid afterSeq");
  if (q.statusClass != null && (!Number.isInteger(q.statusClass) || q.statusClass < 1 || q.statusClass > 5)) throw new Error("Invalid statusClass");
  if (q.excludedHosts && (!Array.isArray(q.excludedHosts) || q.excludedHosts.length > 100 || q.excludedHosts.some(h => typeof h !== "string" || h.length > 255))) throw new Error("Invalid excludedHosts");
  const pattern = new RegExp(q.regex ? q.query : q.query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), q.caseSensitive ? "m" : "im");
  const methods = (q.methods ?? "").split(",").map(m => m.trim().toUpperCase()).filter(Boolean);
  const excluded = new Set((q.excludedHosts ?? []).map(h => h.toLowerCase()));
  const matches: { flowId: string; seq: number; fields: string[] }[] = [];
  let scanned = 0, hasMore = false;
  let nextAfterSeq = q.afterSeq ?? null;
  for (const flow of flows) {
    if (q.afterSeq != null && flow.seq <= q.afterSeq) continue;
    if (matches.length >= limit || scanned >= 2000) { hasMore = true; break; }
    scanned++;
    nextAfterSeq = flow.seq;
    if (q.host && flow.host.toLowerCase() !== q.host.toLowerCase()) continue;
    if (excluded.has(flow.host.toLowerCase())) continue;
    if (methods.length && !methods.includes(flow.method.toUpperCase())) continue;
    if (q.statusClass != null && (flow.status == null || Math.floor(flow.status / 100) !== q.statusClass)) continue;
    const fields = new Set<string>();
    for (const [field, text] of harSearchFields(flow, true)) {
      if (text && pattern.test(text)) fields.add(field.startsWith("WebSocket message") ? "WebSocket message" : field);
    }
    if (fields.size) matches.push({ flowId: flow.id, seq: flow.seq, fields: [...fields] });
  }
  return { matches, scanned, nextAfterSeq, hasMore, contentsOmitted: true };
}
