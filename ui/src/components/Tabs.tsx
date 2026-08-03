import type { Component, JSX } from "solid-js";
import { For, Show } from "solid-js";

export interface TabItem {
  id: string;
  label: string;
  badge?: number;
}

export interface TabsProps {
  tabs: TabItem[];
  active: string;
  onChange: (id: string) => void;
  /**
   * Panel content, rendered as-is below the strip. Tabs only owns the
   * `role="tablist"` strip and tab-switching interactions — it does NOT
   * render panels itself. Callers pick which panel to show (via their own
   * `<Show>`/`<Switch>` keyed on `active`) and pass the result here.
   */
  children: JSX.Element;
}

const Tabs: Component<TabsProps> = (props) => {
  let stripRef: HTMLDivElement | undefined;

  const focusTabAt = (index: number) => {
    const buttons = stripRef?.querySelectorAll<HTMLButtonElement>('[role="tab"]');
    if (!buttons || buttons.length === 0) return;
    const clamped = (index + buttons.length) % buttons.length;
    buttons[clamped]?.focus();
    const id = props.tabs[clamped]?.id;
    if (id) props.onChange(id);
  };

  const handleKeyDown = (e: KeyboardEvent, index: number) => {
    if (e.key === "ArrowRight") {
      e.preventDefault();
      focusTabAt(index + 1);
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      focusTabAt(index - 1);
    }
  };

  return (
    <div class="tabs">
      <div class="tabs__strip" role="tablist" ref={stripRef}>
        <For each={props.tabs}>
          {(tab, index) => (
            <button
              type="button"
              role="tab"
              class={`tabs__tab${tab.id === props.active ? " tabs__tab--active" : ""}`}
              aria-selected={tab.id === props.active}
              tabIndex={tab.id === props.active ? 0 : -1}
              onClick={() => props.onChange(tab.id)}
              onKeyDown={(e) => handleKeyDown(e, index())}
            >
              <span class="tabs__tab-label">{tab.label}</span>
              <Show when={tab.badge !== undefined}>
                <span class="tabs__tab-badge">{tab.badge}</span>
              </Show>
            </button>
          )}
        </For>
      </div>
      <div class="tabs__panels">{props.children}</div>
    </div>
  );
};

export default Tabs;
