import type { Component } from "solid-js";
import { For, Show, createMemo, createSignal } from "solid-js";
import type { HeaderPair } from "../lib/types";
import { pushToast } from "../stores/ui";
import TextInput from "./TextInput";
import Button from "./Button";

export interface HeadersTableProps {
  headers: HeaderPair[];
  title?: string;
}

const COPIED_RESET_MS = 1000;

const HeadersTable: Component<HeadersTableProps> = (props) => {
  const [filter, setFilter] = createSignal("");
  const [copiedIndex, setCopiedIndex] = createSignal<number | null>(null);

  const filtered = createMemo(() => {
    const q = filter().trim().toLowerCase();
    if (!q) return props.headers;
    return props.headers.filter(
      (h) => h.name.toLowerCase().includes(q) || h.value.toLowerCase().includes(q),
    );
  });

  const copyValue = async (header: HeaderPair, index: number) => {
    try {
      await navigator.clipboard.writeText(header.value);
      setCopiedIndex(index);
      pushToast({ level: "success", message: `Copied "${header.name}"` });
      setTimeout(() => {
        setCopiedIndex((current) => (current === index ? null : current));
      }, COPIED_RESET_MS);
    } catch {
      pushToast({ level: "error", message: `Failed to copy "${header.name}"` });
    }
  };

  return (
    <div class="headers-table">
      <Show when={props.title}>
        <div class="headers-table__title">{props.title}</div>
      </Show>
      <TextInput
        value={filter()}
        onInput={setFilter}
        placeholder="Filter headers"
        icon="search"
        class="headers-table__filter"
      />
      <Show when={filtered().length > 0} fallback={<div class="headers-table__empty">No headers</div>}>
        <div class="headers-table__grid">
          <For each={filtered()}>
            {(header, index) => (
              <div class="headers-table__row">
                <div class="headers-table__name mono">{header.name}</div>
                <div class="headers-table__value mono">{header.value}</div>
                <Button
                  variant="ghost"
                  size="sm"
                  icon={copiedIndex() === index() ? "check" : "copy"}
                  aria-label={`Copy value of ${header.name}`}
                  onClick={() => copyValue(header, index())}
                />
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
};

export default HeadersTable;
