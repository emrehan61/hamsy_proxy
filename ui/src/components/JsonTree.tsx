// Collapsible JSON tree viewer with a raw/pretty toggle, copy-all, and a
// simple substring search that auto-expands matching branches.

import type { Component, JSX } from "solid-js";
import { For, Show, createMemo, createSignal } from "solid-js";
import { highlightJson } from "../lib/format";
import { pushToast } from "../stores/ui";
import Icon from "./Icon";
import TextInput from "./TextInput";
import Button from "./Button";

export interface JsonTreeProps {
  data: unknown;
  defaultDepth?: number;
}

type JsonValueKind = "object" | "array" | "string" | "number" | "boolean" | "null";

function kindOf(value: unknown): JsonValueKind {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  if (typeof value === "object") return "object";
  if (typeof value === "string") return "string";
  if (typeof value === "number") return "number";
  if (typeof value === "boolean") return "boolean";
  return "null";
}

// Pragmatic cap for very wide objects/arrays: render only the first
// MAX_VISIBLE_ENTRIES children plus a "+N more…" row that reveals the rest
// in one batch on click. This is NOT windowed virtualization — it's a
// simple cap+show-more. Real windowed virtualization (mounting/unmounting
// rows as they scroll in/out of view) belongs in FlowTable via
// @tanstack/solid-virtual; a JSON tree's nested, variable-height rows make
// that a poor fit here, and a few hundred eagerly-rendered DOM nodes per
// level is cheap in practice.
const MAX_VISIBLE_ENTRIES = 200;

function matchesSearch(text: string, query: string): boolean {
  return text.toLowerCase().includes(query.toLowerCase());
}

interface EntryRow {
  key: string;
  value: unknown;
}

function entriesOf(value: unknown): EntryRow[] {
  if (Array.isArray(value)) return value.map((v, i) => ({ key: String(i), value: v }));
  if (value !== null && typeof value === "object") {
    return Object.entries(value as Record<string, unknown>).map(([key, v]) => ({ key, value: v }));
  }
  return [];
}

function highlightMatch(text: string, query: string): JSX.Element {
  if (!query) return <>{text}</>;
  const idx = text.toLowerCase().indexOf(query.toLowerCase());
  if (idx === -1) return <>{text}</>;
  return (
    <>
      {text.slice(0, idx)}
      <mark class="jsontree__match">{text.slice(idx, idx + query.length)}</mark>
      {text.slice(idx + query.length)}
    </>
  );
}

interface JsonNodeProps {
  keyLabel?: string;
  value: unknown;
  depth: number;
  defaultDepth: number;
  search: () => string;
}

const JsonNode: Component<JsonNodeProps> = (props) => {
  const kind = createMemo(() => kindOf(props.value));
  const isContainer = createMemo(() => kind() === "object" || kind() === "array");

  const [manuallyExpanded, setManuallyExpanded] = createSignal(props.depth < props.defaultDepth);
  const [revealAll, setRevealAll] = createSignal(false);

  // Subtree text used only to decide whether a search query matches
  // somewhere below this node, to drive auto-expand. Plain substring match
  // against the JSON-stringified subtree — no fuzzy search, per spec.
  const subtreeText = createMemo(() => {
    if (!props.search()) return "";
    try {
      return JSON.stringify(props.value);
    } catch {
      return "";
    }
  });

  const hasSearchMatch = createMemo(() => {
    const q = props.search();
    if (!q) return false;
    if (matchesSearch(props.keyLabel ?? "", q)) return true;
    return matchesSearch(subtreeText(), q);
  });

  // Manual toggle OR an active, matching search both count as "expanded";
  // clearing the search reverts to whatever the user last toggled manually.
  const expanded = createMemo(() => manuallyExpanded() || hasSearchMatch());

  const entries = createMemo(() => entriesOf(props.value));
  const visibleEntries = createMemo(() => {
    const all = entries();
    if (revealAll() || all.length <= MAX_VISIBLE_ENTRIES) return all;
    return all.slice(0, MAX_VISIBLE_ENTRIES);
  });

  const summary = createMemo(() => {
    const k = kind();
    if (k === "array") return `[${entries().length} items]`;
    if (k === "object") return `{${entries().length} keys}`;
    return "";
  });

  const renderPrimitive = (): JSX.Element => {
    const k = kind();
    const q = props.search();
    if (k === "string") return <span class="jsontree__str">{highlightMatch(`"${props.value as string}"`, q)}</span>;
    if (k === "number") return <span class="jsontree__num">{highlightMatch(String(props.value), q)}</span>;
    if (k === "boolean") return <span class="jsontree__bool">{highlightMatch(String(props.value), q)}</span>;
    return <span class="jsontree__null">null</span>;
  };

  return (
    <div class="jsontree__node" style={{ "padding-left": `${props.depth * 14}px` }}>
      <div class="jsontree__line">
        <Show when={isContainer()} fallback={<span class="jsontree__chevron-spacer" aria-hidden="true" />}>
          <button
            type="button"
            class="jsontree__toggle"
            aria-label={expanded() ? "Collapse" : "Expand"}
            aria-expanded={expanded()}
            onClick={() => setManuallyExpanded((v) => !v)}
          >
            <Icon name={expanded() ? "chevron-down" : "chevron-right"} size={12} />
          </button>
        </Show>
        <Show when={props.keyLabel !== undefined}>
          <span class="jsontree__key">{highlightMatch(`${props.keyLabel}:`, props.search())}</span>
        </Show>
        <Show when={isContainer()} fallback={renderPrimitive()}>
          <span class="jsontree__summary">{summary()}</span>
        </Show>
      </div>
      {/* Collapsed containers render NO child JSX at all (this <Show>, not
          just a CSS display:none) — the perf-critical part for large
          payloads. */}
      <Show when={isContainer() && expanded()}>
        <div class="jsontree__children">
          <For each={visibleEntries()}>
            {(entry) => (
              <JsonNode
                keyLabel={entry.key}
                value={entry.value}
                depth={props.depth + 1}
                defaultDepth={props.defaultDepth}
                search={props.search}
              />
            )}
          </For>
          <Show when={!revealAll() && entries().length > MAX_VISIBLE_ENTRIES}>
            <button
              type="button"
              class="jsontree__more"
              style={{ "padding-left": `${(props.depth + 1) * 14}px` }}
              onClick={() => setRevealAll(true)}
            >
              +{entries().length - MAX_VISIBLE_ENTRIES} more…
            </button>
          </Show>
        </div>
      </Show>
    </div>
  );
};

export const JsonTree: Component<JsonTreeProps> = (props) => {
  const defaultDepth = () => props.defaultDepth ?? 2;
  const [mode, setMode] = createSignal<"pretty" | "raw">("pretty");
  const [search, setSearch] = createSignal("");

  const rawText = createMemo(() => {
    try {
      return JSON.stringify(props.data, null, 2);
    } catch {
      return String(props.data);
    }
  });

  const highlighted = createMemo(() => highlightJson(rawText()));

  const copyAll = async () => {
    try {
      await navigator.clipboard.writeText(rawText());
      pushToast({ level: "success", message: "Copied JSON to clipboard" });
    } catch {
      pushToast({ level: "error", message: "Failed to copy JSON" });
    }
  };

  return (
    <div class="jsontree">
      <div class="jsontree__toolbar">
        <div class="jsontree__mode-switch" role="group" aria-label="View mode">
          <button
            type="button"
            class={`jsontree__mode-btn${mode() === "pretty" ? " jsontree__mode-btn--active" : ""}`}
            onClick={() => setMode("pretty")}
          >
            Pretty
          </button>
          <button
            type="button"
            class={`jsontree__mode-btn${mode() === "raw" ? " jsontree__mode-btn--active" : ""}`}
            onClick={() => setMode("raw")}
          >
            Raw
          </button>
        </div>
        <TextInput value={search()} onInput={setSearch} placeholder="Search…" icon="search" class="jsontree__search" />
        <Button variant="ghost" size="sm" icon="copy" onClick={copyAll}>
          Copy
        </Button>
      </div>
      <Show
        when={mode() === "pretty"}
        fallback={<pre class="jsontree__raw mono" innerHTML={highlighted()} />}
      >
        <div class="jsontree__root">
          <JsonNode value={props.data} depth={0} defaultDepth={defaultDepth()} search={search} />
        </div>
      </Show>
    </div>
  );
};

export default JsonTree;
