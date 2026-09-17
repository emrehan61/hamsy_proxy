// Filter controls above the flow table: debounced text search, chip
// multi-selects for method/status-class/resource-type, a modified-only
// toggle, and host inclusion/exclusion controls.

import type { Component } from "solid-js";
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { ResourceType } from "../lib/types";
import TextInput from "./TextInput";
import Toggle from "./Toggle";
import Select from "./Select";
import Icon from "./Icon";

export interface FilterBarProps {
  query: string;
  onQueryChange: (v: string) => void;
  methods: string[];
  onMethodsChange: (v: string[]) => void;
  statusClasses: string[];
  onStatusClassesChange: (v: string[]) => void;
  resourceTypes: string[];
  onResourceTypesChange: (v: string[]) => void;
  onlyModified: boolean;
  onOnlyModifiedChange: (v: boolean) => void;
  host: string;
  onHostChange: (v: string) => void;
  hosts: string[];
  excludedHosts: string[];
  onExcludedHostsChange: (v: string[]) => void;
  apps: string[];
  selectedApps: string[];
  onSelectedAppsChange: (v: string[]) => void;
  searchInputRef?: (el: HTMLInputElement) => void;
}

const DEBOUNCE_MS = 120;

const METHOD_OPTIONS = ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"];
const STATUS_CLASS_OPTIONS = ["2xx", "3xx", "4xx", "5xx", "err"];
const RESOURCE_TYPE_OPTIONS: ResourceType[] = [
  "document",
  "stylesheet",
  "script",
  "image",
  "font",
  "xhr",
  "json",
  "media",
  "webSocket",
  "other",
];

function toggleValue<T>(list: T[], value: T): T[] {
  return list.includes(value) ? list.filter((v) => v !== value) : [...list, value];
}

// Options are pre-sorted by the caller to keep their order stable.
const MultiSelectFilterDropdown: Component<{
  options: string[];
  selected: string[];
  onChange: (v: string[]) => void;
  label: string;
  selectedLabel: (selected: string[]) => string;
  clearLabel: string;
  ariaLabel: string;
}> = (props) => {
  const [open, setOpen] = createSignal(false);
  let containerRef: HTMLDivElement | undefined;

  const close = () => setOpen(false);

  const onDocClick = (e: MouseEvent) => {
    if (containerRef && !containerRef.contains(e.target as Node)) close();
  };
  const onDocKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };

  onMount(() => {
    document.addEventListener("click", onDocClick);
    document.addEventListener("keydown", onDocKeyDown);
    onCleanup(() => {
      document.removeEventListener("click", onDocClick);
      document.removeEventListener("keydown", onDocKeyDown);
    });
  });

  const label = () => props.selected.length === 0 ? props.label : props.selectedLabel(props.selected);

  return (
    <div class="filter-bar__app-dropdown" ref={containerRef}>
      <button
        type="button"
        class={`filter-bar__app-trigger${props.selected.length > 0 ? " filter-bar__app-trigger--active" : ""}`}
        aria-haspopup="listbox"
        aria-expanded={open()}
        aria-label={props.ariaLabel}
        onClick={() => setOpen((v) => !v)}
      >
        <span class="filter-bar__app-trigger-label">{label()}</span>
        <Icon name="chevron-down" size={14} class="filter-bar__app-trigger-icon" />
      </button>
      <Show when={open()}>
        <div class="filter-bar__app-panel" role="listbox" aria-multiselectable="true" aria-label={props.ariaLabel}>
          <button
            type="button"
            role="option"
            aria-selected={props.selected.length === 0}
            class={`filter-bar__app-option${props.selected.length === 0 ? " filter-bar__app-option--selected" : ""}`}
            onClick={() => props.onChange([])}
          >
            <span class="filter-bar__app-option-check" aria-hidden="true">
              <Show when={props.selected.length === 0}>
                <Icon name="check" size={13} />
              </Show>
            </span>
            <span class="filter-bar__app-option-label">{props.clearLabel}</span>
          </button>
          <For each={props.options}>
            {(option) => {
              const checked = () => props.selected.includes(option);
              return (
                <button
                  type="button"
                  role="option"
                  aria-selected={checked()}
                  class={`filter-bar__app-option${checked() ? " filter-bar__app-option--selected" : ""}`}
                  title={option}
                  onClick={() => props.onChange(toggleValue(props.selected, option))}
                >
                  <span class="filter-bar__app-option-check" aria-hidden="true">
                    <Show when={checked()}>
                      <Icon name="check" size={13} />
                    </Show>
                  </span>
                  <span class="filter-bar__app-option-label">{option}</span>
                </button>
              );
            }}
          </For>
        </div>
      </Show>
    </div>
  );
};

const FilterBar: Component<FilterBarProps> = (props) => {
  // Local echo of `query` so the input reflects every keystroke instantly;
  // the upward notification (onQueryChange) is debounced by DEBOUNCE_MS.
  // Without this local signal, a controlled <input> bound straight to
  // `props.query` would visibly "lose" keystrokes on any unrelated parent
  // re-render that happens before the debounced update lands.
  const [localQuery, setLocalQuery] = createSignal(props.query);

  // Stay in sync when `query` changes from outside this component (e.g. a
  // "clear filters" action elsewhere).
  createEffect(() => setLocalQuery(props.query));

  let debounceTimer: ReturnType<typeof setTimeout> | undefined;

  const handleQueryInput = (v: string) => {
    setLocalQuery(v);
    if (debounceTimer !== undefined) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      debounceTimer = undefined;
      props.onQueryChange(v);
    }, DEBOUNCE_MS);
  };

  onCleanup(() => {
    if (debounceTimer !== undefined) clearTimeout(debounceTimer);
  });

  const hostOptions = () => [{ value: "", label: "All hosts" }, ...props.hosts.map((h) => ({ value: h, label: h }))];

  // Sorted for stable chip ordering — `props.apps` grows incrementally as
  // new apps are detected, so without sorting the row would reshuffle
  // under the user's cursor every time an unseen app shows up.
  const sortedApps = createMemo(() => [...props.apps].sort((a, b) => a.localeCompare(b)));
  const sortedHosts = createMemo(() => [...new Set([...props.hosts, ...props.excludedHosts])].sort((a, b) => a.localeCompare(b)));

  return (
    <div class="filter-bar">
      <TextInput
        value={localQuery()}
        onInput={handleQueryInput}
        placeholder="Search flows…"
        icon="search"
        class="filter-bar__search"
        ref={props.searchInputRef}
      />

      <div class="filter-bar__chips" role="group" aria-label="Filter by method">
        <For each={METHOD_OPTIONS}>
          {(method) => (
            <button
              type="button"
              class={`filter-bar__chip${props.methods.includes(method) ? " filter-bar__chip--active" : ""}`}
              aria-pressed={props.methods.includes(method)}
              onClick={() => props.onMethodsChange(toggleValue(props.methods, method))}
            >
              {method}
            </button>
          )}
        </For>
      </div>

      <div class="filter-bar__chips" role="group" aria-label="Filter by status class">
        <For each={STATUS_CLASS_OPTIONS}>
          {(cls) => (
            <button
              type="button"
              class={`filter-bar__chip${props.statusClasses.includes(cls) ? " filter-bar__chip--active" : ""}`}
              aria-pressed={props.statusClasses.includes(cls)}
              onClick={() => props.onStatusClassesChange(toggleValue(props.statusClasses, cls))}
            >
              {cls}
            </button>
          )}
        </For>
      </div>

      <div class="filter-bar__chips" role="group" aria-label="Filter by resource type">
        <For each={RESOURCE_TYPE_OPTIONS}>
          {(rt) => (
            <button
              type="button"
              class={`filter-bar__chip${props.resourceTypes.includes(rt) ? " filter-bar__chip--active" : ""}`}
              aria-pressed={props.resourceTypes.includes(rt)}
              onClick={() => props.onResourceTypesChange(toggleValue(props.resourceTypes, rt))}
            >
              {rt}
            </button>
          )}
        </For>
      </div>

      <Show when={sortedApps().length > 0}>
        <MultiSelectFilterDropdown
          options={sortedApps()}
          selected={props.selectedApps}
          onChange={props.onSelectedAppsChange}
          label="All apps"
          selectedLabel={(selected) => selected.length === 1 ? selected[0]! : `${selected.length} apps`}
          clearLabel="All apps"
          ariaLabel="Filter by app"
        />
      </Show>

      <Toggle checked={props.onlyModified} onChange={props.onOnlyModifiedChange} label="Modified only" />

      <Select value={props.host} onChange={props.onHostChange} options={hostOptions()} class="filter-bar__host" />
      <MultiSelectFilterDropdown
        options={sortedHosts()}
        selected={props.excludedHosts}
        onChange={props.onExcludedHostsChange}
        label="Exclude hosts"
        selectedLabel={(selected) => `Exclude hosts (${selected.length})`}
        clearLabel="No excluded hosts"
        ariaLabel="Exclude hosts"
      />
    </div>
  );
};

export default FilterBar;
