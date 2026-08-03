// Imported HAR session store. Each session is a read-only, client-side-only
// snapshot of an imported `.har` file (Traffic page's per-tab data) — never
// touches the live captured-flow store (./flows.ts) or any backend API.
//
// Reactivity: session `flows` arrays can hold tens of thousands of entries,
// and flow content never mutates after import (sessions are read-only), so
// the ordered session list lives in a plain `createSignal<HarSession[]>`
// rather than a `createStore`. `createSignal` just stores whatever reference
// is set — it does NOT deep-proxy nested arrays/objects — so the `flows`
// arrays inside each session stay plain, with no per-element reactivity
// overhead from constructing/diffing a deep Proxy over 10k+ objects. The
// signal is the reactive surface for "which sessions exist / their order";
// alongside it we keep a plain (non-reactive) `Map<string, HarSession>` for
// O(1) `getHarSession`/`sessionFlows` lookups, kept in sync on every
// add/remove — mirrors flows.ts's `idToIndex` plain-map-beside-a-store
// pattern. Per-session flow selection is fine-grained per-tab UI state (a
// string id or null, not bulk data), so it's fine and idiomatic to use a
// `createStore` for it (mirrors flows.ts's `detailStore`).

import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type { Flow } from "../lib/types";
import type { HarPageInfo } from "../lib/har";
import { parseHarText } from "../lib/har";
import type { StoredHarSession } from "../lib/harDb";
import { putSession, getSession, listSessionMetas, deleteSession, clearAllSessions } from "../lib/harDb";

export interface HarSession {
  id: string;
  name: string;
  importedAt: number;
  creatorName: string;
  creatorVersion: string;
  pages: HarPageInfo[];
  flows: Flow[];
}

// ---- ids ----
// Duplicated verbatim from har.ts's private `generateUuid` — that file is
// done and not exported, so this is a byte-for-byte copy, not a divergent
// reimplementation.

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

// ---- session list (signal, not store — see file-top comment) ----

const [sessionList, setSessionList] = createSignal<HarSession[]>([]);

// Plain (non-reactive) index for O(1) lookups. Kept in sync alongside the
// signal; never read reactively by components.
const sessionById = new Map<string, HarSession>();

/** Inserts `s` into `list` keeping ascending `importedAt` order (session counts are small, so a linear scan is fine). */
function insertSorted(list: HarSession[], s: HarSession): HarSession[] {
  const next = list.slice();
  let i = next.length;
  while (i > 0 && (next[i - 1]?.importedAt ?? 0) > s.importedAt) i -= 1;
  next.splice(i, 0, s);
  return next;
}

export function addSession(session: HarSession): void {
  sessionById.set(session.id, session);
  setSessionList((list) => insertSorted(list, session));
}

export function sessions(): HarSession[] {
  return sessionList();
}

export function getHarSession(id: string): HarSession | undefined {
  return sessionById.get(id);
}

export function sessionFlows(id: string): Flow[] {
  return sessionById.get(id)?.flows ?? [];
}

// ---- active tab (null === Live) ----

const [activeSessionId, setActiveSessionIdSignal] = createSignal<string | null>(null);

export { activeSessionId };

/** Tolerates ids not present in `sessions()` yet (e.g. a session still loading). */
export function setActiveSession(id: string | null): void {
  setActiveSessionIdSignal(id);
}

// ---- per-session flow selection (fine-grained UI state -> createStore) ----

const [selectionStore, setSelectionStore] = createStore<Record<string, string | null>>({});

export function harSelectedFlowId(sessionId: string): string | null {
  return selectionStore[sessionId] ?? null;
}

export function selectHarFlow(sessionId: string, flowId: string | null): void {
  setSelectionStore(sessionId, flowId);
}

// ---- naming ----

/** Strips a trailing `.har` extension (case-insensitive). */
function stripHarExtension(fileName: string): string {
  return /\.har$/i.test(fileName) ? fileName.slice(0, -4) : fileName;
}

/** Suffixes ` (2)`, ` (3)`, … until unique among currently open session names. */
function dedupeName(base: string): string {
  const openNames = new Set(Array.from(sessionById.values(), (s) => s.name));
  if (!openNames.has(base)) return base;
  let n = 2;
  while (openNames.has(`${base} (${n})`)) n += 1;
  return `${base} (${n})`;
}

// ---- StoredHarSession <-> HarSession conversion ----

function toStored(s: HarSession): StoredHarSession {
  return {
    id: s.id,
    name: s.name,
    importedAt: s.importedAt,
    flowCount: s.flows.length,
    flows: s.flows,
    pages: s.pages,
    creatorName: s.creatorName,
    creatorVersion: s.creatorVersion,
  };
}

function fromStored(s: StoredHarSession): HarSession {
  return {
    id: s.id,
    name: s.name,
    importedAt: s.importedAt,
    creatorName: s.creatorName,
    creatorVersion: s.creatorVersion,
    pages: s.pages,
    flows: s.flows,
  };
}

// ---- import ----

/** Parses `text` as a HAR document, adds the resulting session in-memory, and fire-and-forget persists it. Throws on parse failure (propagated from `parseHarText`). */
export function importHarText(name: string, text: string): HarSession {
  const parsed = parseHarText(text);
  const baseName = stripHarExtension(name);
  const session: HarSession = {
    id: generateUuid(),
    name: dedupeName(baseName),
    importedAt: Date.now(),
    creatorName: parsed.creatorName,
    creatorVersion: parsed.creatorVersion,
    pages: parsed.pages,
    flows: parsed.flows,
  };
  addSession(session);
  void putSession(toStored(session));
  return session;
}

/** Reads `file` as text and delegates to `importHarText`. */
export async function importHarFile(file: File): Promise<HarSession> {
  const text = await file.text();
  return importHarText(file.name, text);
}

// ---- close ----

export function closeSession(id: string): void {
  const list = sessionList();
  const index = list.findIndex((s) => s.id === id);
  if (index === -1) return;

  const wasActive = activeSessionId() === id;

  sessionById.delete(id);
  const next = list.slice();
  next.splice(index, 1);
  setSessionList(next);

  setSelectionStore(
    produce((s) => {
      delete s[id];
    }),
  );

  if (wasActive) {
    const neighbor = next[index] ?? next[index - 1] ?? null;
    setActiveSessionIdSignal(neighbor ? neighbor.id : null);
  }

  void deleteSession(id);
}

export function closeAllSessions(): void {
  sessionById.clear();
  setSessionList([]);
  setSelectionStore(
    produce((s) => {
      for (const key of Object.keys(s)) delete s[key];
    }),
  );
  setActiveSessionIdSignal(null);
  void clearAllSessions();
}

// ---- restore from IndexedDB ----

let harSessionsRestoreInitialized = false;

/** Hydrates persisted sessions into memory. Idempotent — a second call is a no-op. */
export async function restoreSessionsFromDb(): Promise<void> {
  if (harSessionsRestoreInitialized) return;
  harSessionsRestoreInitialized = true;

  const metas = await listSessionMetas();
  for (const meta of metas) {
    if (sessionById.has(meta.id)) continue;
    const stored = await getSession(meta.id);
    if (stored) addSession(fromStored(stored));
  }
}

/** For a deep-linked window that only has a session id: returns the in-memory session if present, otherwise loads it from IndexedDB (adding it to memory, without re-persisting). Returns `undefined` if not found anywhere. */
export async function loadSessionFromDb(id: string): Promise<HarSession | undefined> {
  const existing = sessionById.get(id);
  if (existing) return existing;

  const stored = await getSession(id);
  if (!stored) return undefined;

  const session = fromStored(stored);
  addSession(session);
  return session;
}
