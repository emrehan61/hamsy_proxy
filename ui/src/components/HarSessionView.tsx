// Read-only viewer for a single imported HAR session — the HAR-tab
// counterpart to Traffic.tsx's live view. Traffic.tsx mounts this keyed on
// `sessionId` (`<Show when={activeSessionId()} keyed>`), so every signal
// below is fresh per HAR tab; only flow selection persists across a remount
// between two HAR tabs, because it lives in the harSessions store's
// `harSelectedFlowId`/`selectHarFlow`, not local component state.
//
// A session's flows are lazy-loaded (see stores/harSessions.ts's
// `ensureSessionLoaded`), so `session()` can resolve to an unloaded stub
// (`loaded: false`, empty `flows`) briefly after this view mounts, before
// the real data arrives from IndexedDB — `loadedSession` below gates on
// that so the flows-dependent memos never run against an empty stub.

import type { Component } from "solid-js";
import { Show, createMemo, createSignal } from "solid-js";
import type { Flow } from "../lib/types";
import { flowsToHar } from "../lib/har";
import { filterFlows } from "../lib/filter";
import type { HarSearchMatch } from "../lib/harSearch";
import { triggerDownload } from "../lib/download";
import { closeSession, harSelectedFlowId, selectHarFlow, sessions } from "../stores/harSessions";
import FilterBar from "./FilterBar";
import FlowTable from "./FlowTable";
import FlowDetail from "./FlowDetail";
import SplitPane from "./SplitPane";
import EmptyState from "./EmptyState";
import Button from "./Button";
import HarSearch from "./HarSearch";

export interface HarSessionViewProps {
  sessionId: string;
}

const HarSessionView: Component<HarSessionViewProps> = (props) => {
  // Reactive lookup (NOT the store's non-reactive getHarSession()) so this
  // view updates if the session only finishes loading from IndexedDB after
  // mount — the deep-linked-window case where Traffic.tsx calls
  // setActiveSession(id) optimistically before loadSessionFromDb() resolves.
  const session = createMemo(() => sessions().find((s) => s.id === props.sessionId));
  const loadedSession = createMemo(() => {
    const s = session();
    return s && s.loaded ? s : undefined;
  });

  // ---- local filter state (same shape as Traffic.tsx's) ----
  const [query, setQuery] = createSignal("");
  const [methods, setMethods] = createSignal<string[]>([]);
  const [statusClasses, setStatusClasses] = createSignal<string[]>([]);
  const [resourceTypes, setResourceTypes] = createSignal<string[]>([]);
  const [onlyModified, setOnlyModified] = createSignal(false);
  const [host, setHost] = createSignal("");
  const [excludedHosts, setExcludedHosts] = createSignal<string[]>([]);
  const [apps, setApps] = createSignal<string[]>([]);
  const [searchOpen, setSearchOpen] = createSignal(false);
  const [searchSelection, setSearchSelection] = createSignal<HarSearchMatch>();

  const openSearchResult = (match: HarSearchMatch) => {
    selectHarFlow(props.sessionId, match.flowId);
    setSearchSelection(match);
    setSearchOpen(false);
  };

  const filteredFlows = createMemo<Flow[]>(() => {
    const s = loadedSession();
    if (!s) return [];
    return filterFlows(s.flows, {
      query: query(),
      methods: methods(),
      statusClasses: statusClasses(),
      resourceTypes: resourceTypes(),
      onlyModified: onlyModified(),
      host: host(),
      excludedHosts: excludedHosts(),
      apps: apps(),
    });
  });

  const filtersActive = (): boolean =>
    query().trim() !== "" ||
    methods().length > 0 ||
    statusClasses().length > 0 ||
    resourceTypes().length > 0 ||
    onlyModified() ||
    host() !== "" ||
    excludedHosts().length > 0 ||
    apps().length > 0;

  // Unique hosts across the session's flows, for the FilterBar host <Select>.
  const hosts = createMemo<string[]>(() => {
    const s = loadedSession();
    if (!s) return [];
    const seen = new Set<string>();
    for (const flow of s.flows) seen.add(flow.host);
    return Array.from(seen).sort();
  });

  // Unique apps across the session's flows, for the FilterBar app chips.
  // Imported HAR flows have no live-ingest "seen apps" set (that's the live
  // Traffic view's stores/flows.ts pattern), so this view derives its own
  // list straight from the already-loaded, static Flow[] instead.
  const sessionApps = createMemo<string[]>(() => {
    const s = loadedSession();
    if (!s) return [];
    const seen = new Set<string>();
    for (const flow of s.flows) seen.add(flow.app ?? "Unknown");
    return Array.from(seen).sort();
  });

  // O(1) selected-flow lookup, mirroring harSessions.ts's own
  // plain-map-beside-reactive-list pattern.
  const flowById = createMemo(() => {
    const s = loadedSession();
    const map = new Map<string, Flow>();
    if (s) for (const f of s.flows) map.set(f.id, f);
    return map;
  });
  const selectedFlow = createMemo<Flow | undefined>(() => {
    const id = harSelectedFlowId(props.sessionId);
    return id ? flowById().get(id) : undefined;
  });

  // ---- toolbar actions ----
  // Pure client-side HAR export (never calls the backend getHar API — that
  // requires flow ids the backend knows about, which imported HAR flows
  // never have).
  const onExportHar = () => {
    const s = loadedSession();
    if (!s) return;
    const filtered = filtersActive();
    const flows = filtered ? filteredFlows() : s.flows;
    const doc = flowsToHar(flows);
    const blob = new Blob([JSON.stringify(doc, null, 2)], { type: "application/json" });
    triggerDownload(blob, filtered ? `${s.name}-filtered.har` : `${s.name}.har`);
  };

  const onOpenInNewWindow = () => {
    const s = session();
    if (!s) return;
    window.open(`${window.location.pathname}?harSession=${encodeURIComponent(s.id)}`, "_blank", "noopener");
  };

  const onCloseSession = () => closeSession(props.sessionId);

  return (
    <Show when={loadedSession()} fallback={<div class="har-session-view__loading">Loading session…</div>}>
      {(s) => (
        <div class="har-session-view">
          <div class="har-session-view__toolbar">
            <span class="har-session-view__name" title={s().name}>
              {s().name}
            </span>
            <span class="har-session-view__count mono">{s().flows.length} flows</span>
            <div class="toolbar__spacer" />
            <Button variant={searchOpen() ? "primary" : "ghost"} size="sm" icon="search" onClick={() => setSearchOpen((open) => !open)}>
              {searchOpen() ? "Show requests" : "Search HAR"}
            </Button>
            <Button variant="ghost" size="sm" icon="download" onClick={onExportHar}>
              Export HAR
            </Button>
            <Button variant="ghost" size="sm" icon="external-link" onClick={onOpenInNewWindow}>
              Open in new window
            </Button>
            <Button variant="ghost" size="sm" icon="close" onClick={onCloseSession}>
              Close
            </Button>
          </div>

          <Show
            when={s().flows.length > 0}
            fallback={<EmptyState icon="file" title="No entries in this HAR" description="This HAR file parsed to zero flows." />}
          >
            <div class="har-session-view__body">
              <SplitPane
                direction="horizontal"
                sizeKey="har.split"
                min={320}
                initial={720}
                first={
                  <div class="har-session-view__list">
                    <FilterBar
                      query={query()}
                      onQueryChange={setQuery}
                      methods={methods()}
                      onMethodsChange={setMethods}
                      statusClasses={statusClasses()}
                      onStatusClassesChange={setStatusClasses}
                      resourceTypes={resourceTypes()}
                      onResourceTypesChange={setResourceTypes}
                      onlyModified={onlyModified()}
                      onOnlyModifiedChange={setOnlyModified}
                      host={host()}
                      onHostChange={setHost}
                      excludedHosts={excludedHosts()}
                      onExcludedHostsChange={setExcludedHosts}
                      hosts={hosts()}
                      apps={sessionApps()}
                      selectedApps={apps()}
                      onSelectedAppsChange={setApps}
                    />
                    <div class="har-session-view__table">
                      <HarSearch flows={s().flows} filteredFlows={filteredFlows()} active={searchOpen()} onSelect={openSearchResult} />
                      <Show when={!searchOpen()}>
                        <Show
                          when={filteredFlows().length > 0}
                          fallback={<div class="har-session-view__no-match">No flows match your filters.</div>}
                        >
                          <FlowTable
                            flows={filteredFlows()}
                            selectedId={harSelectedFlowId(props.sessionId)}
                            onSelect={(id) => selectHarFlow(props.sessionId, id)}
                            revealSelected
                          />
                        </Show>
                      </Show>
                    </div>
                  </div>
                }
                second={<FlowDetail flow={selectedFlow()} revealTab={searchSelection()} />}
              />
            </div>
          </Show>
        </div>
      )}
    </Show>
  );
};

export default HarSessionView;
