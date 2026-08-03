import type { Component } from "solid-js";
import { Show } from "solid-js";

export interface ToggleProps {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: string;
  disabled?: boolean;
  /** Accessible name for toggles rendered without a visible `label` (e.g. compact list rows). */
  "aria-label"?: string;
}

const Toggle: Component<ToggleProps> = (props) => {
  return (
    <label class={`toggle${props.disabled ? " toggle--disabled" : ""}`}>
      <input
        type="checkbox"
        role="switch"
        class="toggle__input"
        checked={props.checked}
        disabled={props.disabled}
        aria-checked={props.checked}
        aria-label={props.label ? undefined : props["aria-label"]}
        onChange={(e) => props.onChange(e.currentTarget.checked)}
      />
      <span class="toggle__track" aria-hidden="true">
        <span class="toggle__thumb" />
      </span>
      <Show when={props.label}>
        <span class="toggle__label">{props.label}</span>
      </Show>
    </label>
  );
};

export default Toggle;
