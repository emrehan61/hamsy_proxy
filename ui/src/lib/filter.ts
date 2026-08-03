// Shared flow-filtering predicate. Used by both the live Traffic page (over
// the WS-fed flows store) and the read-only HAR session viewer (over a
// static, already-loaded `Flow[]`) — one implementation keeps their filter
// semantics (search query, method/status/resource-type chips, "modified
// only", host) from ever silently drifting apart between the two views.

import type { FlowSummary } from "./types";
import { statusClassOf } from "./format";

export interface FlowFilterState {
  query: string;
  methods: string[];
  statusClasses: string[];
  resourceTypes: string[];
  onlyModified: boolean;
  host: string;
  apps: string[];
}

// Single pass over `flows` for every filter — do not chain .filter()/.map()
// here, this must stay one loop for 50k-row perf.
export function filterFlows<T extends FlowSummary>(flows: T[], f: FlowFilterState): T[] {
  const q = f.query.trim().toLowerCase();
  const result: T[] = [];
  for (const flow of flows) {
    if (f.methods.length > 0 && !f.methods.includes(flow.method)) continue;
    if (f.statusClasses.length > 0) {
      // "err" is a FilterBar-only chip for network-level errors that
      // statusClassOf (pure HTTP-status mapper) has no concept of.
      const cls = flow.error !== null ? "err" : statusClassOf(flow.status);
      if (!f.statusClasses.includes(cls)) continue;
    }
    if (f.resourceTypes.length > 0 && !f.resourceTypes.includes(flow.resourceType)) continue;
    if (f.onlyModified && !flow.modified) continue;
    if (f.host && flow.host !== f.host) continue;
    if (f.apps.length > 0 && !f.apps.includes(flow.app ?? "Unknown")) continue;
    if (q) {
      const haystack = `${flow.url} ${flow.host} ${flow.method} ${flow.status ?? ""}`.toLowerCase();
      if (!haystack.includes(q)) continue;
    }
    result.push(flow);
  }
  return result;
}
