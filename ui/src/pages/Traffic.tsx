// Main traffic page: owns flow filtering, selection, the split layout,
// toolbar action implementations (HAR export, replay, copy-as-cURL,
// pause/clear), keyboard shortcuts, and the empty states.

import type { Component } from "solid-js";
import { Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { useSearchParams } from "@solidjs/router";
import "../styles/har.css";
import type { Flow, FlowSummary } from "../lib/types";
import type { HarSearchMatch } from "../lib/harSearch";
import { clearFlows as clearFlowsApi, getHar, listFlows, replayFlow } from "../lib/api";
import { triggerDownload } from "../lib/download";
import { pushToast } from "../stores/ui";
import { settings, setSettings } from "../stores/settings";
import { wsClient } from "../lib/ws";
import {
  allFlowIds,
  clearFlows as clearFlowsStore,
  flowCount,
  getFlow as getFlowSummary,
  getFlowDetail,
  ingestFlows,
  seenApps,
  seenHosts,
  selectFlow,
  selectedId,
  setFlowDetail,
} from "../stores/flows";
import { activeSessionId, loadSessionFromDb, restoreSessionsFromDb, setActiveSession } from "../stores/harSessions";
import { filterFlows } from "../lib/filter";
import FilterBar from "../components/FilterBar";
import FlowTable, { type FlowTableApi } from "../components/FlowTable";
import FlowDetail from "../components/FlowDetail";
import SplitPane from "../components/SplitPane";
import Toolbar from "../components/Toolbar";
import EmptyState, { type EmptyStateProps } from "../components/EmptyState";
import SessionTabs, { importHarFileList } from "../components/SessionTabs";
import HarSessionView from "../components/HarSessionView";
import SessionSearch from "../components/SessionSearch";
import Icon from "../components/Icon";
import { apiState } from "../stores/systemProxy";

// ---- cURL export ----
//
// Hop-by-hop headers never make sense to replay verbatim in a standalone
// curl invocation (they describe the proxy<->client connection, not the
// resource request), and HTTP/2 pseudo-headers (":authority" etc.) aren't
// valid header syntax for curl at all.
const HOP_BY_HOP_HEADERS = new Set([
  "connection",
  "keep-alive",
  "proxy-connection",
  "transfer-encoding",
  "upgrade",
  "te",
  "trailer",
]);

/** POSIX-safe single-quote wrapping: close, escaped quote, reopen. */
function shQuote(value: string): string {
  return `'${value.split("'").join("'\\''")}'`;
}

function decodeBase64Text(data: string): string | undefined {
  try {
    const binary = atob(data);
    const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return undefined;
  }
}

function flowToCurl(flow: Flow): string {
  const req = flow.request;
  if (!req) {
    throw new Error("Flow detail not loaded yet");
  }

  const parts: string[] = ["curl", "-X", shQuote(req.method), shQuote(req.url)];
  for (const h of req.headers) {
    const name = h.name.trim();
    if (name.startsWith(":")) continue;
    if (HOP_BY_HOP_HEADERS.has(name.toLowerCase())) continue;
    parts.push("-H", shQuote(`${h.name}: ${h.value}`));
  }

  let binaryOmitted = false;
  const body = req.body;
  if (body.kind === "text") {
    parts.push("--data-raw", shQuote(body.data));
  } else if (body.kind === "base64") {
    const decoded = decodeBase64Text(body.data);
    if (decoded !== undefined) {
      parts.push("--data-raw", shQuote(decoded));
    } else {
      binaryOmitted = true;
    }
  }
  // "none"/"truncated" bodies: nothing meaningful to replay, omit silently.

  const command = parts.join(" ");
  return binaryOmitted ? `${command}\n# binary body omitted` : command;
}

function isTextInputTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;
}

const Traffic: Component = () => {
  // ---- filter state ----
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

  const openSearchResult = (match: HarSearchMatch, flow: Flow) => {
    setFlowDetail(flow.id, flow);
    selectFlow(flow.id);
    setSearchSelection(match);
    setSearchOpen(false);
  };

  const onQueryChange = (v: string) => setQuery(v);

  // ---- data ----
  // FlowTable wants a plain, parent-filtered array, not the store's raw
  // id list — rebuild it here from the store's fine-grained byId map.
  const allFlows = createMemo<FlowSummary[]>(() => {
    const ids = allFlowIds();
    const result: FlowSummary[] = [];
    for (const id of ids) {
      const flow = getFlowSummary(id);
      if (flow) result.push(flow);
    }
    return result;
  });

  // Single pass over the full flow list for every filter — see
  // ../lib/filter.ts (shared with the read-only HAR view).
  const filteredFlows = createMemo<FlowSummary[]>(() =>
    filterFlows(allFlows(), {
      query: query(),
      methods: methods(),
      statusClasses: statusClasses(),
      resourceTypes: resourceTypes(),
      onlyModified: onlyModified(),
      host: host(),
      excludedHosts: excludedHosts(),
      apps: apps(),
    }),
  );

  // ---- selection ----
  // `selectFlow` (store action) already fetches + caches the full Flow
  // detail on first selection; we just read the cache reactively here.
  const selectedFlowDetail = createMemo<Flow | undefined>(() => {
    const id = selectedId();
    return id === null ? undefined : getFlowDetail(id);
  });

  // ---- pause (settings is the single source of truth; WS is the live
  // control channel, REST persists it for other clients/restarts) ----
  const paused = () => settings()?.paused ?? false;

  // ---- system proxy off (drives the empty-state copy below so a quiet
  // flow list reads as "proxy is off" rather than "hamsy is broken") ----
  const systemProxyOff = () => {
    const sp = apiState()?.systemProxy;
    return sp !== undefined && sp.supported && !sp.enabled;
  };

  const noFlowsEmptyState = createMemo<EmptyStateProps>(() => {
    if (systemProxyOff()) {
      return {
        icon: "plug-off",
        title: "System proxy is off",
        description: `Hamsy isn't receiving new traffic — use the "System proxy" button above to turn it back on, or point a client at 127.0.0.1:${apiState()?.proxyPort ?? "?"} directly.`,
      };
    }
    return {
      icon: "list",
      title: "No flows captured yet",
      description: `Set your system proxy to 127.0.0.1:${apiState()?.proxyPort ?? "9080"} and install the CA cert.`,
      action: { label: "Go to Setup", href: "/setup" },
    };
  });

  const onTogglePause = () => {
    const next = !paused();
    wsClient.send({ type: "pause", paused: next });
    setSettings({ paused: next }).catch(() => {
      pushToast({ level: "error", message: "Failed to persist pause state" });
    });
  };

  // ---- clear ----
  const doClear = () => {
    clearFlowsStore();
    clearFlowsApi().catch(() => {
      pushToast({ level: "error", message: "Failed to clear flows on the server" });
    });
  };

  // ---- HAR export ----
  async function exportHar(ids: string[], fallbackFilename: string): Promise<void> {
    try {
      const { blob, filename } = await getHar(ids);
      triggerDownload(blob, filename ?? fallbackFilename);
    } catch {
      pushToast({ level: "error", message: "Failed to export HAR" });
    }
  }

  const onExportHarAll = () => {
    const ids = allFlowIds();
    if (ids.length === 0) {
      pushToast({ level: "warning", message: "No flows to export" });
      return;
    }
    void exportHar(ids, "flows-all.har");
  };

  const onExportHarSelected = () => {
    const id = selectedId();
    if (!id) {
      pushToast({ level: "warning", message: "No flow selected" });
      return;
    }
    void exportHar([id], "flow.har");
  };

  const onExportHarFiltered = () => {
    const ids = filteredFlows().map((f) => f.id);
    if (ids.length === 0) {
      pushToast({ level: "warning", message: "No flows match the current filters" });
      return;
    }
    void exportHar(ids, "flows-filtered.har");
  };

  // ---- replay ----
  const onReplaySelected = () => {
    const id = selectedId();
    if (!id) return;
    replayFlow(id).then(
      () => pushToast({ level: "success", message: "Replay started" }),
      () => pushToast({ level: "error", message: "Replay failed" }),
    );
  };

  // ---- copy as cURL ----
  const onCopyCurl = () => {
    const id = selectedId();
    if (!id) return;
    const flow = getFlowDetail(id);
    if (!flow) {
      pushToast({ level: "error", message: "Flow detail not loaded yet" });
      return;
    }
    let curl: string;
    try {
      curl = flowToCurl(flow);
    } catch (err) {
      pushToast({ level: "error", message: err instanceof Error ? err.message : "Failed to build cURL command" });
      return;
    }
    navigator.clipboard.writeText(curl).then(
      () => pushToast({ level: "success", message: "Copied as cURL" }),
      () => pushToast({ level: "error", message: "Failed to copy to clipboard" }),
    );
  };

  // ---- keyboard shortcuts ----
  let searchInputEl: HTMLInputElement | undefined;
  const handleSearchInputRef = (el: HTMLInputElement) => {
    searchInputEl = el;
  };

  let flowTableApi: FlowTableApi | undefined;
  const handleFlowTableReady = (api: FlowTableApi) => {
    flowTableApi = api;
  };

  const onKeyDown = (e: KeyboardEvent) => {
    // If FlowTable's own onKeyDown (attached directly to its focused scroll
    // container) already handled this event — it preventDefaults for
    // Arrow/j/k — skip it here rather than double-moving the selection.
    if (e.defaultPrevented) return;

    const inTextInput = isTextInputTarget(e.target);

    if (e.key === "/" && !inTextInput) {
      e.preventDefault();
      searchInputEl?.focus();
      return;
    }

    if (e.key === "Escape") {
      if (document.activeElement === searchInputEl && query() !== "") {
        onQueryChange("");
      }
      searchInputEl?.blur();
      return;
    }

    // Minimal, deliberate interpretation of "quick filter": no command
    // palette this phase, just jump to the search box (same as "/").
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      searchInputEl?.focus();
      return;
    }

    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "e") {
      if (activeSessionId() !== null) return;
      e.preventDefault();
      onExportHarFiltered();
      return;
    }

    if (e.key === " " && !inTextInput) {
      if (activeSessionId() !== null) return;
      e.preventDefault();
      onTogglePause();
      return;
    }

    if (e.key === "Delete" && !inTextInput) {
      if (activeSessionId() !== null) return;
      if (confirm("Clear all captured flows? This cannot be undone.")) {
        doClear();
      }
      return;
    }

    if (inTextInput || activeSessionId() !== null || searchOpen()) return;

    if (e.key === "j") {
      flowTableApi?.moveSelection(1);
    } else if (e.key === "k") {
      flowTableApi?.moveSelection(-1);
    }
  };

  // ---- HAR import (toolbar button) ----
  let harToolbarFileInputEl: HTMLInputElement | undefined;
  const onImportHarClick = () => harToolbarFileInputEl?.click();
  const onHarFileInputChange = (e: Event) => {
    const input = e.currentTarget as HTMLInputElement;
    // `input.files` is cleared in place by the `value = ""` reset below
    // (Blink mutates the existing FileList rather than replacing it), so
    // snapshot into an array first. The reset itself is what lets the user
    // re-pick the same file and still get a change event.
    const files = Array.from(input.files ?? []);
    input.value = "";
    if (files.length > 0) void importHarFileList(files);
  };

  // ---- HAR import (drag & drop anywhere on the page) ----
  const [isDraggingFile, setIsDraggingFile] = createSignal(false);
  let dragDepth = 0;
  const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes("Files");
  const onDragEnter = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    dragDepth += 1;
    setIsDraggingFile(true);
  };
  const onDragOver = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
  };
  const onDragLeave = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) setIsDraggingFile(false);
  };
  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    dragDepth = 0;
    setIsDraggingFile(false);
    const files = Array.from(e.dataTransfer?.files ?? []).filter((f) => /\.har$/i.test(f.name));
    if (files.length === 0) {
      pushToast({ level: "warning", message: "No .har files found in drop" });
      return;
    }
    void importHarFileList(files);
  };

  // ---- HAR deep link (?harSession=<id>) ----
  const [searchParams, setSearchParams] = useSearchParams();
  const initialHarSession = searchParams.harSession;
  const hasDeepLink = typeof initialHarSession === "string" && initialHarSession.length > 0;
  const [deepLinkPending, setDeepLinkPending] = createSignal(hasDeepLink);

  // Mirrors activeSessionId() into the URL (replace, not push, so switching
  // tabs never floods browser history). Suppressed while a deep-linked
  // session is still hydrating from IndexedDB, so the async load doesn't
  // race with this effect and momentarily clear the ?harSession= param
  // before setActiveSession(id) has a chance to run.
  createEffect(() => {
    const id = activeSessionId();
    if (id === null && deepLinkPending()) return;
    setSearchParams({ harSession: id ?? undefined }, { replace: true });
  });

  onMount(() => {
    // The WS-pushed store (initFlowsSync, wired in App.tsx) only carries
    // live updates going forward — hydrate history once via REST.
    listFlows()
      .then((res) => ingestFlows(res.flows))
      .catch((err) => {
        console.error("hamsy-proxy: failed to load initial flows", err);
      });

    void restoreSessionsFromDb();
    if (hasDeepLink) {
      loadSessionFromDb(initialHarSession as string)
        .then((session) => {
          if (session) setActiveSession(session.id);
          else pushToast({ level: "error", message: "HAR session not found" });
        })
        .finally(() => setDeepLinkPending(false));
    }

    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  return (
    <div class="traffic-page" onDragEnter={onDragEnter} onDragOver={onDragOver} onDragLeave={onDragLeave} onDrop={onDrop}>
      <SessionTabs />
      <Show when={isDraggingFile()}>
        <div class="traffic-page__drop-overlay">
          <Icon name="download" size={32} />
          <p>Drop .har files to import</p>
        </div>
      </Show>
      <Show
        when={activeSessionId() === null}
        fallback={
          <Show when={activeSessionId()} keyed>
            {(id) => <HarSessionView sessionId={id} />}
          </Show>
        }
      >
        <Toolbar
          paused={paused()}
          onTogglePause={onTogglePause}
          onClear={doClear}
          onExportHarAll={onExportHarAll}
          onExportHarSelected={onExportHarSelected}
          onExportHarFiltered={onExportHarFiltered}
          onReplaySelected={onReplaySelected}
          onCopyCurl={onCopyCurl}
          flowCount={flowCount()}
          hasSelection={selectedId() !== null}
          onImportHar={onImportHarClick}
          searchOpen={searchOpen()}
          onToggleSearch={() => setSearchOpen((open) => !open)}
        />
        <div class="traffic-page__body">
          <SplitPane
            direction="horizontal"
            sizeKey="traffic.split"
            min={320}
            initial={720}
            first={
              <div class="traffic-page__list">
                <Show when={flowCount() > 0}>
                  <FilterBar
                    query={query()}
                    onQueryChange={onQueryChange}
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
                    hosts={seenHosts()}
                    excludedHosts={excludedHosts()}
                    onExcludedHostsChange={setExcludedHosts}
                    apps={seenApps()}
                    selectedApps={apps()}
                    onSelectedAppsChange={setApps}
                    searchInputRef={handleSearchInputRef}
                  />
                </Show>
                <div class="traffic-page__table">
                  <SessionSearch flows={allFlows()} filteredFlows={filteredFlows()} active={searchOpen()} onSelect={openSearchResult} />
                  <Show when={!searchOpen()}>
                    <Show when={flowCount() > 0} fallback={<EmptyState {...noFlowsEmptyState()} />}>
                      <Show
                        when={filteredFlows().length > 0}
                        fallback={<div class="traffic-page__no-match">No flows match your filters.</div>}
                      >
                        <FlowTable
                          flows={filteredFlows()}
                          selectedId={selectedId()}
                          onSelect={selectFlow}
                          onReady={handleFlowTableReady}
                          revealSelected
                        />
                      </Show>
                    </Show>
                  </Show>
                </div>
              </div>
            }
            second={<FlowDetail flow={selectedFlowDetail()} revealTab={searchSelection()} />}
          />
        </div>
      </Show>
      <input
        ref={(el) => (harToolbarFileInputEl = el)}
        type="file"
        accept=".har,application/json"
        multiple
        class="traffic-page__file-input"
        onChange={onHarFileInputChange}
      />
    </div>
  );
};

export default Traffic;
