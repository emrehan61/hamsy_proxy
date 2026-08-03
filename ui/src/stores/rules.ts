// Rules store: a Solid resource wrapping GET /api/rules, plus mutation
// helpers that call the REST endpoints and keep the local resource in sync.
// Mirrors stores/settings.ts's shape (lazy WS wiring via initRulesSync,
// called once from App.tsx so `rulesChanged` pushes trigger a refetch
// regardless of which page is mounted).

import { createResource } from "solid-js";
import * as api from "../lib/api";
import type { RuleInput } from "../lib/api";
import type { Rule } from "../lib/types";
import { wsClient } from "../lib/ws";

async function fetchRules(): Promise<Rule[]> {
  const res = await api.listRules();
  return res.rules;
}

const [rulesResource, { refetch: refetchRulesResource, mutate: mutateRulesResource }] = createResource(fetchRules);

/** Current rules in server order, or `[]` until the initial fetch resolves. */
export function rules(): Rule[] {
  return rulesResource() ?? [];
}

export function rulesLoading(): boolean {
  return rulesResource.loading;
}

export function rulesLoadError(): unknown {
  return rulesResource.error as unknown;
}

export function refetchRules(): Promise<Rule[] | undefined> | Rule[] | undefined | null {
  return refetchRulesResource();
}

export async function createRule(input: RuleInput): Promise<Rule> {
  const created = await api.createRule(input);
  mutateRulesResource((prev) => [...(prev ?? []), created]);
  return created;
}

export async function updateRule(id: string, input: RuleInput): Promise<Rule> {
  const saved = await api.updateRule(id, input);
  mutateRulesResource((prev) => (prev ?? []).map((r) => (r.id === id ? saved : r)));
  return saved;
}

export async function deleteRule(id: string): Promise<void> {
  await api.deleteRule(id);
  mutateRulesResource((prev) => (prev ?? []).filter((r) => r.id !== id));
}

/** Optimistic enable/disable flip, rolled back on failure. */
export async function toggleRule(id: string): Promise<Rule> {
  const previous = rulesResource();
  mutateRulesResource((prev) => (prev ?? []).map((r) => (r.id === id ? { ...r, enabled: !r.enabled } : r)));
  try {
    const saved = await api.toggleRule(id);
    mutateRulesResource((prev) => (prev ?? []).map((r) => (r.id === id ? saved : r)));
    return saved;
  } catch (err) {
    if (previous) mutateRulesResource(previous);
    throw err;
  }
}

/** Optimistic reorder to `orderedIds`, rolled back on failure. */
export async function reorderRules(orderedIds: string[]): Promise<void> {
  const previous = rulesResource();
  const byId = new Map((previous ?? []).map((r) => [r.id, r] as const));
  const reordered = orderedIds.map((id) => byId.get(id)).filter((r): r is Rule => r !== undefined);
  mutateRulesResource(reordered);
  try {
    await api.reorderRules(orderedIds);
  } catch (err) {
    if (previous) mutateRulesResource(previous);
    throw err;
  }
}

export async function importRules(input: RuleInput[], replace: boolean): Promise<{ imported: number }> {
  const result = await api.importRules(input, replace);
  await refetchRulesResource();
  return result;
}

export function exportRules(): Promise<{ rules: Rule[] }> {
  return api.exportRules();
}

let rulesSyncInitialized = false;

/** Wires the rules resource to `rulesChanged` WS pushes. Call once from App.tsx. */
export function initRulesSync(): void {
  if (rulesSyncInitialized) return;
  rulesSyncInitialized = true;
  wsClient.onMessage((msg) => {
    if (msg.type === "rulesChanged") {
      void refetchRulesResource();
    }
  });
}
