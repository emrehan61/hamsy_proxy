// Shared "list of strings" editor: host/port globs, status-code patterns,
// include/exclude host globs, etc. Each row is a text input with a remove
// button; a trailing "+ Add" button appends a blank editable row. Blank rows
// are kept in `values` as-is — callers are expected to filter empty strings
// out at save time, not on every keystroke (so a row mid-edit never
// vanishes).

import type { Component } from "solid-js";
import { For, Show } from "solid-js";
import TextInput from "./TextInput";
import Button from "./Button";

export interface RepeatableInputListProps {
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
  mono?: boolean;
  addLabel?: string;
  "aria-label"?: string;
  /** Returns an error message for a non-empty value, or null if it's valid. */
  validate?: (value: string) => string | null;
}

const RepeatableInputList: Component<RepeatableInputListProps> = (props) => {
  const setAt = (index: number, value: string) => {
    const next = props.values.slice();
    next[index] = value;
    props.onChange(next);
  };

  const removeAt = (index: number) => {
    props.onChange(props.values.filter((_, i) => i !== index));
  };

  const add = () => {
    props.onChange([...props.values, ""]);
  };

  return (
    <div class="repeatable-list" role="group" aria-label={props["aria-label"]}>
      <For each={props.values}>
        {(value, index) => {
          const error = () => (value.trim() && props.validate ? props.validate(value) : null);
          return (
            <div class="repeatable-list__row">
              <TextInput
                value={value}
                onInput={(v) => setAt(index(), v)}
                placeholder={props.placeholder}
                mono={props.mono}
                aria-label={props["aria-label"] ? `${props["aria-label"]} ${index() + 1}` : undefined}
              />
              <Button
                variant="ghost"
                size="sm"
                icon="close"
                aria-label="Remove"
                onClick={() => removeAt(index())}
              />
              <Show when={error()}>{(msg) => <span class="repeatable-list__error">{msg()}</span>}</Show>
            </div>
          );
        }}
      </For>
      <Button variant="ghost" size="sm" icon="plus" onClick={add}>
        {props.addLabel ?? "Add"}
      </Button>
    </div>
  );
};

export default RepeatableInputList;
