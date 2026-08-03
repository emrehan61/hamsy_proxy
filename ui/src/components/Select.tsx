import type { Component } from "solid-js";
import { For, Show } from "solid-js";

export interface SelectOption {
  value: string;
  label: string;
}

export interface SelectProps {
  value: string;
  onChange: (v: string) => void;
  options: SelectOption[];
  placeholder?: string;
  class?: string;
  disabled?: boolean;
}

const Select: Component<SelectProps> = (props) => {
  return (
    <select
      class={`select${props.class ? ` ${props.class}` : ""}`}
      value={props.value}
      disabled={props.disabled}
      onChange={(e) => props.onChange(e.currentTarget.value)}
    >
      <Show when={props.placeholder}>
        <option value="" disabled>
          {props.placeholder}
        </option>
      </Show>
      <For each={props.options}>{(opt) => <option value={opt.value}>{opt.label}</option>}</For>
    </select>
  );
};

export default Select;
