import type { Component } from "solid-js";
import { For } from "solid-js";
import { dismissToast, toasts, type Toast as ToastData, type ToastLevel } from "../stores/ui";
import Icon, { type IconName } from "./Icon";
import Button from "./Button";

function iconForLevel(level: ToastLevel): IconName {
  switch (level) {
    case "success":
      return "check";
    case "error":
      return "alert-triangle";
    case "warning":
      return "alert-triangle";
    case "info":
    default:
      return "info";
  }
}

const ToastItem: Component<{ toast: ToastData }> = (props) => {
  return (
    <div class={`toast toast--${props.toast.level}`} role="status">
      <Icon name={iconForLevel(props.toast.level)} size={16} class="toast__icon" />
      <span class="toast__message">{props.toast.message}</span>
      <Button
        variant="ghost"
        size="sm"
        icon="close"
        aria-label="Dismiss notification"
        onClick={() => dismissToast(props.toast.id)}
      />
    </div>
  );
};

export const ToastHost: Component = () => {
  return (
    <div class="toast-host">
      <For each={toasts()}>{(t) => <ToastItem toast={t} />}</For>
    </div>
  );
};

export default ToastHost;
