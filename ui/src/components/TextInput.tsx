import type { Component } from "solid-js";
import { Show } from "solid-js";
import Icon, { type IconName } from "./Icon";

export interface TextInputProps {
  value: string;
  onInput: (v: string) => void;
  placeholder?: string;
  type?: string;
  class?: string;
  icon?: IconName;
  mono?: boolean;
  disabled?: boolean;
  /** Wires to the native `list` attribute for a `<datalist>` of suggestions. */
  list?: string;
  "aria-label"?: string;
  id?: string;
  onKeyDown?: (e: KeyboardEvent) => void;
  ref?: (el: HTMLInputElement) => void;
}

const TextInput: Component<TextInputProps> = (props) => {
  return (
    <div
      class={`text-input${props.icon ? " text-input--with-icon" : ""}${
        props.class ? ` ${props.class}` : ""
      }`}
    >
      <Show when={props.icon}>
        {(name) => <Icon name={name()} size={14} class="text-input__icon" />}
      </Show>
      <input
        ref={props.ref}
        id={props.id}
        class={`text-input__field${props.mono ? " mono" : ""}`}
        type={props.type ?? "text"}
        value={props.value}
        placeholder={props.placeholder}
        disabled={props.disabled}
        list={props.list}
        aria-label={props["aria-label"]}
        onInput={(e) => props.onInput(e.currentTarget.value)}
        onKeyDown={props.onKeyDown}
      />
    </div>
  );
};

export default TextInput;
