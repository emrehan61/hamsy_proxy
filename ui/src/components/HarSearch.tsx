import type { Component } from "solid-js";
import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import type { Flow } from "../lib/types";
import type { HarSearchMatch, HarSearchResult } from "../lib/harSearch";
import TextInput from "./TextInput";
import Toggle from "./Toggle";

const HarSearch: Component<{
  flows: Flow[];
  filteredFlows: Flow[];
  active: boolean;
  onSelect: (match: HarSearchMatch) => void;
}> = (props) => {
  const [query, setQuery] = createSignal("");
  const [regex, setRegex] = createSignal(false);
  const [caseSensitive, setCaseSensitive] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [result, setResult] = createSignal<HarSearchResult>({ matches: [], requestCount: 0, error: null });
  const flowById = createMemo(() => new Map(props.flows.map((flow) => [flow.id, flow])));
  let input: HTMLInputElement | undefined;
  let scroll: HTMLDivElement | undefined;
  let worker: Worker | undefined;
  let workerFlows: Flow[] | undefined;
  let searchId = 0;

  const stopWorker = () => {
    worker?.terminate();
    worker = undefined;
    workerFlows = undefined;
  };
  onCleanup(stopWorker);
  createEffect(() => {
    if (props.active) input?.focus();
  });

  createEffect(() => {
    const active = props.active;
    const text = query();
    const useRegex = regex();
    const matchCase = caseSensitive();
    const flows = props.flows;
    const ids = props.filteredFlows.map((flow) => flow.id);
    const id = ++searchId;
    setResult({ matches: [], requestCount: 0, error: null });
    setBusy(active && text.length > 0);
    if (!active || text.length === 0) return;

    let inFlight = false;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const timer = setTimeout(() => {
      try {
        if (!worker || workerFlows !== flows) {
          stopWorker();
          worker = new Worker(new URL("../lib/harSearch.worker.ts", import.meta.url), { type: "module" });
          worker.postMessage({ type: "init", flows });
          workerFlows = flows;
        }
        const fail = (error: string) => {
          if (id !== searchId) return;
          clearTimeout(timeout);
          inFlight = false;
          stopWorker();
          setBusy(false);
          setResult({ matches: [], requestCount: 0, error });
        };
        worker.onmessage = (event: MessageEvent<HarSearchResult & { id: number }>) => {
          if (event.data.id !== searchId) return;
          clearTimeout(timeout);
          inFlight = false;
          setResult(event.data);
          setBusy(false);
          if (scroll) scroll.scrollTop = 0;
        };
        worker.onerror = () => fail("Search failed. Try again with a more specific query.");
        inFlight = true;
        timeout = setTimeout(() => fail("Search took too long. Try a more specific query or a simpler regular expression."), 8000);
        worker.postMessage({ type: "search", id, query: text, regex: useRegex, caseSensitive: matchCase, ids });
      } catch {
        inFlight = false;
        clearTimeout(timeout);
        stopWorker();
        setBusy(false);
        setResult({ matches: [], requestCount: 0, error: "Search could not start. Try again." });
      }
    }, 180);
    onCleanup(() => {
      clearTimeout(timer);
      clearTimeout(timeout);
      // A pathological regex cannot block the UI or hold up the next query.
      if (inFlight) stopWorker();
    });
  });

  const virtualizer = createVirtualizer<HTMLDivElement, HTMLButtonElement>({
    get count() { return result().matches.length; },
    getScrollElement: () => scroll ?? null,
    estimateSize: () => 88,
    overscan: 5,
  });

  return (
    <section class="har-search" aria-label="Search HAR contents" style={{ display: props.active ? "flex" : "none" }}>
      <div class="har-search__controls">
        <TextInput value={query()} onInput={setQuery} placeholder="Search all request and response contents…" aria-label="Search HAR contents" icon="search" ref={(el) => { input = el; }} />
        <Toggle checked={regex()} onChange={setRegex} label="Regex" />
        <Toggle checked={caseSensitive()} onChange={setCaseSensitive} label="Match case" />
      </div>
      <div class="har-search__status" role="status" aria-live="polite">
        {busy() ? "Searching…" : result().error ? result().error : query().length === 0
          ? "Search URLs, headers, query parameters, text bodies, and WebSocket messages. Filters above apply."
          : `${result().matches.length} matching ${result().matches.length === 1 ? "field" : "fields"} in ${result().requestCount} ${result().requestCount === 1 ? "request" : "requests"}. First match shown per field. Click to open the request.`}
      </div>
      <div class="har-search__results" ref={scroll}>
        <Show when={!busy() && !result().error && query().length > 0 && result().matches.length === 0}>
          <div class="har-session-view__no-match">No matches found.</div>
        </Show>
        <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
          <For each={virtualizer.getVirtualItems()}>
            {(item) => (
              <Show when={result().matches[item.index]}>
                {(match) => (
                  <button type="button" class="har-search__result" style={{ transform: `translateY(${item.start}px)` }} onClick={() => props.onSelect(match())}>
                    <span class="har-search__url mono" title={flowById().get(match().flowId)?.url}>
                      {flowById().get(match().flowId)?.method} {flowById().get(match().flowId)?.url}
                    </span>
                    <span class="har-search__field">{match().field}</span>
                    <span class="har-search__snippet mono">{match().before}<mark>{match().match || "∣"}</mark>{match().after}</span>
                  </button>
                )}
              </Show>
            )}
          </For>
        </div>
      </div>
    </section>
  );
};

export default HarSearch;
