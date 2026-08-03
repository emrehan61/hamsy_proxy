// Flow list store. Performance-critical: must stay smooth with tens of
// thousands of rows, so updates are fine-grained (per-row) and WS ingestion
// is batched/coalesced rather than triggering a reactive update per message.
//
// Only imports from ../lib/api, ../lib/ws, ../lib/types, ./settings — no
// components/pages exist yet for this phase to depend on.

import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { getFlow as apiGetFlow } from "../lib/api";
import { wsClient } from "../lib/ws";
import type { Flow, FlowSummary, WsMessage } from "../lib/types";
import { settings } from "./settings";

const DEFAULT_MAX_FLOWS = 5000;

interface FlowsState {
  order: string[]; // flow ids in seq order
  byId: Record<string, FlowSummary>;
}

const [store, setStore] = createStore<FlowsState>({ order: [], byId: {} });

// Plain (non-reactive) index for O(1) existence/position lookups. Kept in
// sync alongside the store; never read reactively by components.
const idToIndex = new Map<string, number>();

// ---- ingestion ----

/** Upserts a single flow. Delegates to the batch path (still one produce call). */
export function ingestFlow(flow: FlowSummary): void {
  ingestFlows([flow]);
}

/**
 * Upserts many flows in a single `produce` transaction (coalesced — never
 * call `setStore` once per flow). New ids are appended to `order`; existing
 * ids are updated in place in `byId` so only that row's fine-grained signal
 * fires, not the whole list.
 */
export function ingestFlows(incoming: FlowSummary[]): void {
  if (incoming.length === 0) return;

  const maxFlows = settings()?.maxFlows ?? DEFAULT_MAX_FLOWS;
  let evictedCount = 0;

  setStore(
    produce((s) => {
      for (const flow of incoming) {
        noteHost(flow.host);
        const existingIndex = idToIndex.get(flow.id);
        if (existingIndex !== undefined) {
          // Fine-grained in-place update — only this row's consumers re-run.
          s.byId[flow.id] = flow;
        } else {
          const index = s.order.length;
          s.order.push(flow.id);
          s.byId[flow.id] = flow;
          idToIndex.set(flow.id, index);
        }
      }

      if (s.order.length > maxFlows) {
        evictedCount = s.order.length - maxFlows;
        const evictedIds = s.order.splice(0, evictedCount);
        for (const id of evictedIds) {
          delete s.byId[id];
          idToIndex.delete(id);
        }
      }
    }),
  );

  if (evictedCount > 0) {
    // Eviction shifts every remaining index — cheaper to rebuild the map
    // once here than to keep per-row indices correct during the splice.
    idToIndex.clear();
    store.order.forEach((id, i) => idToIndex.set(id, i));
  }
}

export function clearFlows(): void {
  setStore(
    produce((s) => {
      s.order.splice(0, s.order.length);
      for (const key of Object.keys(s.byId)) {
        delete s.byId[key];
      }
    }),
  );
  idToIndex.clear();
  seenHostsSet.clear();
  setSeenHostsVersion((v) => v + 1);
  setSelectedId(null);
  setDetailStore(
    produce((d) => {
      for (const key of Object.keys(d)) {
        delete d[key];
      }
    }),
  );
}

export function getFlow(id: string): FlowSummary | undefined {
  return store.byId[id];
}

export function allFlowIds(): string[] {
  return store.order;
}

export function flowCount(): number {
  return store.order.length;
}

// ---- seen hosts (incremental, versioned so it stays cheaply reactive) ----

const seenHostsSet = new Set<string>();
const [seenHostsVersion, setSeenHostsVersion] = createSignal(0);

function noteHost(host: string): void {
  if (!seenHostsSet.has(host)) {
    seenHostsSet.add(host);
    setSeenHostsVersion((v) => v + 1);
  }
}

/** Unique hosts seen so far. Reactive via a version counter bumped on ingest. */
export function seenHosts(): string[] {
  seenHostsVersion();
  return Array.from(seenHostsSet);
}

// ---- selection ----

const [selectedId, setSelectedId] = createSignal<string | null>(null);
export { selectedId };

export function selectFlow(id: string | null): void {
  setSelectedId(id);
  if (id !== null && !detailStore[id]) {
    void loadFlowDetail(id);
  }
}

async function loadFlowDetail(id: string): Promise<void> {
  try {
    const flow = await apiGetFlow(id);
    setFlowDetail(id, flow);
  } catch (err) {
    // Swallow here; a later phase can surface this via the ui store's toasts.
    console.error(`flproxy: failed to load flow detail for ${id}`, err);
  }
}

// ---- flow detail cache ----

const [detailStore, setDetailStore] = createStore<Record<string, Flow>>({});

export function getFlowDetail(id: string): Flow | undefined {
  return detailStore[id];
}

export function setFlowDetail(id: string, flow: Flow): void {
  setDetailStore(id, flow);
}

function appendWsMessageToDetail(flowId: string, message: WsMessage): void {
  if (!detailStore[flowId]) return;
  setDetailStore(
    produce((d) => {
      const flow = d[flowId];
      if (flow) flow.wsMessages.push(message);
    }),
  );
}

// ---- WS ingestion buffering, flushed on requestAnimationFrame ----
//
// `flow`/`flows` WS messages push into a plain (non-reactive) module-level
// buffer instead of calling ingestFlows directly. An rAF is scheduled only on
// the transition from empty -> non-empty (i.e. the first push after the
// previous flush drained the buffer), so we never spin rAF while idle. The
// callback drains whatever has accumulated since it was scheduled into a
// single `ingestFlows` call, keeping WS message volume decoupled from
// reactive-update volume (crucial at high flow throughput).

let wsBuffer: FlowSummary[] = [];

function flushWsBuffer(): void {
  if (wsBuffer.length === 0) return;
  const batch = wsBuffer;
  wsBuffer = [];
  ingestFlows(batch);
}

function bufferIncomingFlows(flows: FlowSummary[]): void {
  if (flows.length === 0) return;
  const wasEmpty = wsBuffer.length === 0;
  for (const flow of flows) wsBuffer.push(flow);
  if (wasEmpty) {
    requestAnimationFrame(flushWsBuffer);
  }
}

// ---- lazy WS wiring ----

let flowsSyncInitialized = false;

/** Wires WS subscriptions + the rAF flush loop. Call once from App.tsx. */
export function initFlowsSync(): void {
  if (flowsSyncInitialized) return;
  flowsSyncInitialized = true;

  wsClient.onMessage((msg) => {
    switch (msg.type) {
      case "flow":
        bufferIncomingFlows([msg.flow]);
        break;
      case "flows":
        bufferIncomingFlows(msg.flows);
        break;
      case "cleared":
        clearFlows();
        break;
      case "flowDetail":
        setFlowDetail(msg.flow.id, msg.flow);
        break;
      case "wsMessage":
        appendWsMessageToDetail(msg.flowId, msg.message);
        break;
      default:
        break;
    }
  });
}
