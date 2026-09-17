import type { Component } from "solid-js";
import { createEffect, createMemo, createSignal, onCleanup, untrack } from "solid-js";
import type { Flow, FlowSummary } from "../lib/types";
import type { HarSearchMatch } from "../lib/harSearch";
import { loadSearchSnapshot } from "../lib/sessionSearch";
import Button from "./Button";
import HarSearch from "./HarSearch";

const SessionSearch: Component<{
  flows: FlowSummary[];
  filteredFlows: FlowSummary[];
  active: boolean;
  onSelect: (match: HarSearchMatch, flow: Flow) => void;
}> = (props) => {
  const [snapshot, setSnapshot] = createSignal<Flow[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const [refresh, setRefresh] = createSignal(0);
  // Track clearing the session, but keep results stable as traffic arrives.
  const empty = createMemo(() => props.flows.length === 0);
  const filteredSnapshot = createMemo(() => {
    const ids = new Set(props.filteredFlows.map((flow) => flow.id));
    return snapshot().filter((flow) => ids.has(flow.id));
  }, undefined, { equals: (previous, next) => previous.length === next.length && previous.every((flow, index) => flow === next[index]) });

  createEffect(() => {
    const active = props.active;
    const isEmpty = empty();
    refresh();
    setSnapshot([]);
    setError(undefined);
    setLoading(false);
    if (!active || isEmpty) return;
    const ids = untrack(() => props.flows.map((flow) => flow.id));
    const controller = new AbortController();
    setLoading(true);
    // A stalled connection should also leave a retryable state.
    const timeout = setTimeout(() => {
      controller.abort();
      setLoading(false);
      setError("Loading took too long. Refresh to try again.");
    }, 60_000);
    void loadSearchSnapshot(ids, controller.signal).then(
      (flows) => {
        if (controller.signal.aborted) return;
        setSnapshot(flows);
        setLoading(false);
      },
      () => {
        if (controller.signal.aborted) return;
        setError("Could not load session contents. Refresh to try again.");
        setLoading(false);
      },
    ).finally(() => clearTimeout(timeout));
    onCleanup(() => {
      clearTimeout(timeout);
      controller.abort();
    });
  });

  return (
    <div class="session-search" style={{ display: props.active ? "flex" : "none" }}>
      <div class="session-search__snapshot">
        <span role="status" aria-live="polite">
          {loading() ? "Loading session contents…" : error() ?? `Searching a snapshot of ${snapshot().length} ${snapshot().length === 1 ? "request" : "requests"}. Refresh to include the latest traffic and WebSocket messages.`}
        </span>
        <Button size="sm" variant="ghost" onClick={() => setRefresh((value) => value + 1)}>Refresh</Button>
      </div>
      <HarSearch
        flows={snapshot()}
        filteredFlows={filteredSnapshot()}
        active={props.active && !loading() && !error()}
        label="Search current session contents"
        onSelect={(match) => {
          const flow = snapshot().find((flow) => flow.id === match.flowId);
          if (flow) props.onSelect(match, flow);
        }}
      />
    </div>
  );
};

export default SessionSearch;
