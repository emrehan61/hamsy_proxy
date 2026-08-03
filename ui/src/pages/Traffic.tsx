// Main traffic page: owns flow filtering, selection, the split layout,
// toolbar action implementations (HAR export, replay, copy-as-cURL,
// pause/clear), keyboard shortcuts, and the empty states.

import type { Component } from "solid-js";
import { Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { Flow, FlowSummary } from "../lib/types";
import { statusClassOf } from "../lib/format";
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
  seenHosts,
  selectFlow,
  selectedId,
} from "../stores/flows";
import FilterBar from "../components/FilterBar";
import FlowTable, { type FlowTableApi } from "../components/FlowTable";
import FlowDetail from "../components/FlowDetail";
import SplitPane from "../components/SplitPane";
import Toolbar from "../components/Toolbar";
import EmptyState from "../components/EmptyState";

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

  // Single pass over the full flow list for every filter — do not chain
  // .filter()/.map() here, this must stay one loop for 50k-row perf.
  const filteredFlows = createMemo<FlowSummary[]>(() => {
    const flows = allFlows();
    const q = query().trim().toLowerCase();
    const methodsList = methods();
    const statusClassList = statusClasses();
    const resourceTypeList = resourceTypes();
    const onlyMod = onlyModified();
    const hostFilter = host();

    const result: FlowSummary[] = [];
    for (const flow of flows) {
      if (methodsList.length > 0 && !methodsList.includes(flow.method)) continue;
      if (statusClassList.length > 0) {
        // FilterBar's status-class chips include "err" for network-level
        // errors, which statusClassOf (a pure HTTP-status mapper) has no
        // concept of — handle it as a special case here instead.
        const cls = flow.error !== null ? "err" : statusClassOf(flow.status);
        if (!statusClassList.includes(cls)) continue;
      }
      if (resourceTypeList.length > 0 && !resourceTypeList.includes(flow.resourceType)) continue;
      if (onlyMod && !flow.modified) continue;
      if (hostFilter && flow.host !== hostFilter) continue;
      if (q) {
        const haystack = `${flow.url} ${flow.host} ${flow.method} ${flow.status ?? ""}`.toLowerCase();
        if (!haystack.includes(q)) continue;
      }
      result.push(flow);
    }
    return result;
  });

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
      e.preventDefault();
      onExportHarFiltered();
      return;
    }

    if (e.key === " " && !inTextInput) {
      e.preventDefault();
      onTogglePause();
      return;
    }

    if (e.key === "Delete" && !inTextInput) {
      if (confirm("Clear all captured flows? This cannot be undone.")) {
        doClear();
      }
      return;
    }

    if (inTextInput) return;

    if (e.key === "j") {
      flowTableApi?.moveSelection(1);
    } else if (e.key === "k") {
      flowTableApi?.moveSelection(-1);
    }
  };

  onMount(() => {
    // The WS-pushed store (initFlowsSync, wired in App.tsx) only carries
    // live updates going forward — hydrate history once via REST.
    listFlows()
      .then((res) => ingestFlows(res.flows))
      .catch((err) => {
        console.error("flproxy: failed to load initial flows", err);
      });

    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  return (
    <div class="traffic-page">
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
                  searchInputRef={handleSearchInputRef}
                />
              </Show>
              <div class="traffic-page__table">
                <Show
                  when={flowCount() > 0}
                  fallback={
                    <EmptyState
                      icon="plug-off"
                      title="No flows captured yet"
                      description="Set your system proxy to 127.0.0.1:9080 and install the CA cert."
                      action={{ label: "Go to Setup", href: "/setup" }}
                    />
                  }
                >
                  <Show
                    when={filteredFlows().length > 0}
                    fallback={<div class="traffic-page__no-match">No flows match your filters.</div>}
                  >
                    <FlowTable
                      flows={filteredFlows()}
                      selectedId={selectedId()}
                      onSelect={selectFlow}
                      onReady={handleFlowTableReady}
                    />
                  </Show>
                </Show>
              </div>
            </div>
          }
          second={<FlowDetail flow={selectedFlowDetail()} />}
        />
      </div>
    </div>
  );
};

export default Traffic;
