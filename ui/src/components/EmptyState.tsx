import type { Component } from "solid-js";
import { Show } from "solid-js";
// @solidjs/router is already a project dependency (see package.json), so the
// href case uses its `A` component for proper client-side SPA navigation
// instead of a plain <a> that would force a full page reload.
import { A } from "@solidjs/router";
import Icon, { type IconName } from "./Icon";
import Button from "./Button";

export interface EmptyStateAction {
  label: string;
  href?: string;
  onClick?: () => void;
}

export interface EmptyStateProps {
  icon?: IconName;
  title: string;
  description?: string;
  action?: EmptyStateAction;
}

const EmptyState: Component<EmptyStateProps> = (props) => {
  return (
    <div class="empty-state">
      <Show when={props.icon}>
        {(name) => <Icon name={name()} size={32} class="empty-state__icon" />}
      </Show>
      <p class="empty-state__title">{props.title}</p>
      <Show when={props.description}>
        <p class="empty-state__description">{props.description}</p>
      </Show>
      <Show when={props.action}>
        {(action) => (
          <Show
            when={action().href}
            fallback={
              <Button variant="default" size="sm" onClick={() => action().onClick?.()}>
                {action().label}
              </Button>
            }
          >
            {(href) => (
              <A href={href()} class="btn btn--default btn--sm empty-state__link">
                {action().label}
              </A>
            )}
          </Show>
        )}
      </Show>
    </div>
  );
};

export default EmptyState;
