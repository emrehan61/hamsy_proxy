// Virtualized flow list — the most perf-critical component in the app.
//
// Design choice: FlowTable takes `flows: FlowSummary[]` as a plain prop
// (parent-owned, already filtered/sorted) rather than reading
// `allFlowIds()`/the flows store directly. This keeps FlowTable a dumb,
// reusable "render this array virtualized" view and leaves all
// filtering/derivation to the page that owns the flows store (a later
// phase's Traffic page).
//
// Only `<For each={virtualizer.getVirtualItems()}>` ever renders rows —
// the full `flows` array is never mapped into JSX.

import type { Component } from "solid-js";
import { For, Show, createEffect, createSignal, onMount } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import type { FlowSummary } from "../lib/types";
import { getPersisted, setPersisted } from "../stores/ui";
import FlowRow from "./FlowRow";

export interface FlowColumn {
  key: "seq" | "method" | "status" | "host" | "path" | "type" | "size" | "time" | "duration";
  label: string;
  defaultWidth: number;
}

// Shared column definition — imported by FlowRow so header cells and row
// cells never drift out of sync. Do not duplicate this list elsewhere.
export const FLOW_COLUMNS: FlowColumn[] = [
  { key: "seq", label: "#", defaultWidth: 56 },
  { key: "method", label: "Method", defaultWidth: 72 },
  { key: "status", label: "Status", defaultWidth: 64 },
  { key: "host", label: "Host", defaultWidth: 180 },
  { key: "path", label: "Path", defaultWidth: 320 },
  { key: "type", label: "Type", defaultWidth: 90 },
  { key: "size", label: "Size", defaultWidth: 84 },
  { key: "time", label: "Time", defaultWidth: 96 },
  { key: "duration", label: "Duration", defaultWidth: 84 },
];

const COLUMN_WIDTHS_PERSIST_KEY = "columns.flowTable"; // -> localStorage "flproxy.columns.flowTable"
const ROW_HEIGHT = 28;
const MIN_COLUMN_WIDTH = 40;
// Distance (px) from the bottom of the scroll area within which we still
// consider the user "at the bottom" for follow-mode purposes.
const FOLLOW_THRESHOLD_PX = 48;

export interface FlowTableApi {
  moveSelection: (delta: number) => void;
}

export interface FlowTableProps {
  flows: FlowSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onReady?: (api: FlowTableApi) => void;
}

function defaultColumnWidths(): Record<string, number> {
  const widths: Record<string, number> = {};
  for (const col of FLOW_COLUMNS) widths[col.key] = col.defaultWidth;
  return widths;
}

const FlowTable: Component<FlowTableProps> = (props) => {
  let scrollRef: HTMLDivElement | undefined;

  const [columnWidths, setColumnWidths] = createSignal<Record<string, number>>({
    ...defaultColumnWidths(),
    ...getPersisted<Record<string, number>>(COLUMN_WIDTHS_PERSIST_KEY, {}),
  });

  // ---- column resize (pointer-events drag, mirrors SplitPane's divider) ----
  let resizing: { key: string; startX: number; startWidth: number } | null = null;

  const beginResize = (key: string) => (e: PointerEvent) => {
    resizing = { key, startX: e.clientX, startWidth: columnWidths()[key] ?? MIN_COLUMN_WIDTH };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };
  const onResizeMove = (e: PointerEvent) => {
    if (!resizing) return;
    const r = resizing;
    const next = Math.max(MIN_COLUMN_WIDTH, r.startWidth + (e.clientX - r.startX));
    setColumnWidths((prev) => ({ ...prev, [r.key]: next }));
  };
  const endResize = (e: PointerEvent) => {
    if (!resizing) return;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    resizing = null;
    setPersisted(COLUMN_WIDTHS_PERSIST_KEY, columnWidths());
  };

  // ---- virtualizer ----
  // `count` is a getter (reactive-props pattern) so @tanstack/solid-virtual's
  // internal createComputed re-subscribes to `props.flows.length` whenever
  // the parent supplies a new (filtered/grown) array. Explicit generics
  // avoid TypeScript inference falling back to `unknown` for TItemElement,
  // which has nothing else in the options object to infer it from.
  const virtualizer = createVirtualizer<HTMLDivElement, HTMLDivElement>({
    get count() {
      return props.flows.length;
    },
    getScrollElement: () => scrollRef ?? null,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
  });

  // ---- follow mode ----
  // `following` starts true (tail the live feed). It flips to false the
  // moment the user scrolls away from the bottom, and back to true either
  // by manually scrolling back to the bottom or via the "Jump to latest"
  // pill. A plain scroll-position check (not scroll *direction*) covers
  // both transitions with one code path.
  const [following, setFollowing] = createSignal(true);

  const isNearBottom = (): boolean => {
    const el = scrollRef;
    if (!el) return true;
    return el.scrollTop + el.clientHeight >= el.scrollHeight - FOLLOW_THRESHOLD_PX;
  };

  const onScroll = () => setFollowing(isNearBottom());

  // Previous-length tracker (plain variable, not a signal — we only need
  // its value at effect-run time, not reactivity on it) so the
  // scroll-to-bottom effect fires only when `flows` *grows* (new flows
  // arriving) and not when a filter change shrinks or reorders it.
  let prevLength = props.flows.length;

  createEffect(() => {
    const len = props.flows.length;
    if (following() && len > prevLength) {
      virtualizer.scrollToIndex(len - 1, { align: "end" });
    }
    prevLength = len;
  });

  const jumpToLatest = () => {
    if (props.flows.length === 0) return;
    virtualizer.scrollToIndex(props.flows.length - 1, { align: "end" });
    setFollowing(true);
  };

  // ---- selection / keyboard navigation ----
  const indexOfSelected = (): number => {
    if (props.selectedId === null) return -1;
    return props.flows.findIndex((f) => f.id === props.selectedId);
  };

  const moveSelection = (delta: number): void => {
    if (props.flows.length === 0) return;
    const current = indexOfSelected();
    const base = current === -1 ? (delta > 0 ? -1 : 0) : current;
    const next = Math.min(Math.max(base + delta, 0), props.flows.length - 1);
    const flow = props.flows[next];
    if (!flow) return;
    props.onSelect(flow.id);
    virtualizer.scrollToIndex(next, { align: "auto" });
  };

  onMount(() => {
    props.onReady?.({ moveSelection });
  });

  const onKeyDown = (e: KeyboardEvent) => {
    if ((e.key === "a" || e.key === "A") && (e.metaKey || e.ctrlKey)) {
      // Explicit no-op: no "select all rows" feature. Don't preventDefault
      // — let native text selection behave as the browser normally would.
      return;
    }
    if (e.key === "ArrowDown" || e.key === "j") {
      e.preventDefault();
      moveSelection(1);
    } else if (e.key === "ArrowUp" || e.key === "k") {
      e.preventDefault();
      moveSelection(-1);
    }
  };

  return (
    <div class="flow-table">
      <div class="flow-table__header" role="row">
        <For each={FLOW_COLUMNS}>
          {(col) => (
            <div
              class={`flow-table__header-cell flow-table__header-cell--${col.key}`}
              role="columnheader"
              style={{ width: `${columnWidths()[col.key]}px`, "flex-basis": `${columnWidths()[col.key]}px` }}
            >
              <span class="flow-table__header-label">{col.label}</span>
              <div
                class="flow-table__resize-handle"
                onPointerDown={beginResize(col.key)}
                onPointerMove={onResizeMove}
                onPointerUp={endResize}
              />
            </div>
          )}
        </For>
      </div>
      <div
        class="flow-table__scroll"
        role="grid"
        aria-rowcount={props.flows.length + 1}
        aria-colcount={FLOW_COLUMNS.length}
        tabIndex={0}
        ref={scrollRef}
        onScroll={onScroll}
        onKeyDown={onKeyDown}
      >
        {/* Spacer sized to the virtualizer's total height gives the scroll
            container a correct native scrollbar; each row is absolutely
            positioned within it at its virtual `start` offset. */}
        <div class="flow-table__spacer" style={{ height: `${virtualizer.getTotalSize()}px` }}>
          <For each={virtualizer.getVirtualItems()}>
            {(item) => (
              <Show when={props.flows[item.index]}>
                {(flow) => (
                  <FlowRow
                    flow={flow()}
                    selected={flow().id === props.selectedId}
                    columnWidths={columnWidths()}
                    onClick={() => props.onSelect(flow().id)}
                    style={{
                      position: "absolute",
                      top: "0",
                      left: "0",
                      width: "100%",
                      transform: `translateY(${item.start}px)`,
                    }}
                  />
                )}
              </Show>
            )}
          </For>
        </div>
      </div>
      <Show when={!following()}>
        <button type="button" class="flow-table__jump-pill" onClick={jumpToLatest}>
          Jump to latest
        </button>
      </Show>
    </div>
  );
};

export default FlowTable;
