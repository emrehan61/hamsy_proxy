import type { Component, JSX } from "solid-js";
import { Show, createEffect, onCleanup } from "solid-js";
import { Portal } from "solid-js/web";
import Button from "./Button";

export interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children: JSX.Element;
}

const Modal: Component<ModalProps> = (props) => {
  let containerRef: HTMLDivElement | undefined;
  let previouslyFocused: HTMLElement | null = null;

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") props.onClose();
  };

  createEffect(() => {
    if (props.open) {
      previouslyFocused = document.activeElement as HTMLElement | null;
      document.addEventListener("keydown", handleKeyDown);
      containerRef?.focus();
    } else {
      document.removeEventListener("keydown", handleKeyDown);
      previouslyFocused?.focus();
      previouslyFocused = null;
    }
  });

  onCleanup(() => {
    document.removeEventListener("keydown", handleKeyDown);
  });

  return (
    <Show when={props.open}>
      <Portal>
        <div class="modal__backdrop" onClick={() => props.onClose()}>
          <div
            ref={containerRef}
            class="modal"
            role="dialog"
            aria-modal="true"
            aria-label={props.title}
            tabIndex={-1}
            onClick={(e) => e.stopPropagation()}
          >
            <Show when={props.title}>
              <div class="modal__header">
                <h2 class="modal__title">{props.title}</h2>
                <Button
                  variant="ghost"
                  size="sm"
                  icon="close"
                  aria-label="Close"
                  onClick={() => props.onClose()}
                />
              </div>
            </Show>
            <div class="modal__body">{props.children}</div>
          </div>
        </div>
      </Portal>
    </Show>
  );
};

export default Modal;
