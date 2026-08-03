import type { Component, JSX } from "solid-js";
import { Show } from "solid-js";
import Icon, { type IconName } from "./Icon";

export type ButtonVariant = "default" | "primary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md";

export interface ButtonProps {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: IconName;
  disabled?: boolean;
  onClick?: (e: MouseEvent) => void;
  "aria-label"?: string;
  children?: JSX.Element;
  title?: string;
  type?: "button" | "submit";
}

const Button: Component<ButtonProps> = (props) => {
  const variant = () => props.variant ?? "default";
  const size = () => props.size ?? "md";
  const iconOnly = () => Boolean(props.icon) && !props.children;

  return (
    <button
      type={props.type ?? "button"}
      class={`btn btn--${variant()} btn--${size()}${iconOnly() ? " btn--icon-only" : ""}`}
      disabled={props.disabled}
      onClick={props.onClick}
      title={props.title}
      aria-label={props["aria-label"]}
    >
      <Show when={props.icon}>
        {(name) => <Icon name={name()} size={size() === "sm" ? 14 : 16} class="btn__icon" />}
      </Show>
      <Show when={props.children}>
        <span class="btn__label">{props.children}</span>
      </Show>
    </button>
  );
};

export default Button;
