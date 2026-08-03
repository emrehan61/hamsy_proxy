// Top toolbar: capture controls, HAR export, replay/curl actions, live
// connection indicator, flow count, and the theme toggle.

import type { Component } from "solid-js";
import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { wsClient, type WsStatus } from "../lib/ws";
import { setTheme, theme } from "../stores/settings";
import Button from "./Button";

export interface ToolbarProps {
  paused: boolean;
  onTogglePause: () => void;
  onClear: () => void;
  onExportHarAll: () => void;
  onExportHarSelected: () => void;
  onExportHarFiltered: () => void;
  onReplaySelected: () => void;
  onCopyCurl: () => void;
  flowCount: number;
  hasSelection: boolean;
  /** Optional so existing `<Toolbar/>` call sites without HAR import stay unaffected — the button only renders when this is provided. */
  onImportHar?: () => void;
}

function statusLabel(status: WsStatus): string {
  switch (status) {
    case "open":
      return "Live — connected";
    case "connecting":
      return "Connecting…";
    case "reconnecting":
      return "Reconnecting…";
    case "closed":
      return "Disconnected";
  }
}

const ConnectionStatusDot: Component = () => {
  const [status, setStatus] = createSignal<WsStatus>("connecting");
  onMount(() => {
    const unsubscribe = wsClient.onStatusChange(setStatus);
    onCleanup(unsubscribe);
  });
  return (
    <span class={`toolbar__status-dot toolbar__status-dot--${status()}`} role="status" aria-label={statusLabel(status())} title={statusLabel(status())} />
  );
};

const ExportHarMenu: Component<{
  onAll: () => void;
  onSelected: () => void;
  onFiltered: () => void;
  hasSelection: boolean;
}> = (props) => {
  const [open, setOpen] = createSignal(false);
  let containerRef: HTMLDivElement | undefined;

  const close = () => setOpen(false);

  const onDocClick = (e: MouseEvent) => {
    if (containerRef && !containerRef.contains(e.target as Node)) close();
  };
  const onDocKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };

  onMount(() => {
    document.addEventListener("click", onDocClick);
    document.addEventListener("keydown", onDocKeyDown);
    onCleanup(() => {
      document.removeEventListener("click", onDocClick);
      document.removeEventListener("keydown", onDocKeyDown);
    });
  });

  const pick = (fn: () => void) => {
    fn();
    close();
  };

  return (
    <div class="toolbar__export-menu" ref={containerRef}>
      <Button variant="default" size="sm" icon="download" onClick={() => setOpen((v) => !v)}>
        Export HAR
      </Button>
      <Show when={open()}>
        <div class="toolbar__export-menu-list" role="menu">
          <button type="button" role="menuitem" class="toolbar__export-menu-item" onClick={() => pick(props.onAll)}>
            All flows
          </button>
          <button
            type="button"
            role="menuitem"
            class="toolbar__export-menu-item"
            disabled={!props.hasSelection}
            onClick={() => pick(props.onSelected)}
          >
            Selected flow
          </button>
          <button type="button" role="menuitem" class="toolbar__export-menu-item" onClick={() => pick(props.onFiltered)}>
            Filtered flows
          </button>
        </div>
      </Show>
    </div>
  );
};

const Toolbar: Component<ToolbarProps> = (props) => {
  const onClearClick = () => {
    if (confirm("Clear all captured flows? This cannot be undone.")) {
      props.onClear();
    }
  };

  return (
    <div class="toolbar">
      <Button
        variant="default"
        size="sm"
        icon={props.paused ? "play" : "pause"}
        onClick={props.onTogglePause}
        aria-label={props.paused ? "Resume capture" : "Pause capture"}
      >
        {props.paused ? "Resume" : "Pause"}
      </Button>
      <Button variant="ghost" size="sm" icon="trash" onClick={onClearClick} aria-label="Clear flows">
        Clear
      </Button>

      <div class="toolbar__divider" aria-hidden="true" />

      <ExportHarMenu
        onAll={props.onExportHarAll}
        onSelected={props.onExportHarSelected}
        onFiltered={props.onExportHarFiltered}
        hasSelection={props.hasSelection}
      />
      <Show when={props.onImportHar}>
        <Button variant="ghost" size="sm" icon="arrow-up" onClick={() => props.onImportHar?.()}>
          Import HAR
        </Button>
      </Show>
      <Button variant="ghost" size="sm" icon="replay" disabled={!props.hasSelection} onClick={props.onReplaySelected}>
        Replay
      </Button>
      <Button variant="ghost" size="sm" icon="code" disabled={!props.hasSelection} onClick={props.onCopyCurl}>
        Copy as cURL
      </Button>

      <div class="toolbar__spacer" />

      <ConnectionStatusDot />
      <span class="toolbar__flow-count mono">{props.flowCount} flows</span>

      {/* ThemeChoice also has a "system" value; the toggle only ever flips
          between the two concrete choices, so "system" is treated like
          "dark" for icon/toggle purposes here. */}
      <Button
        variant="ghost"
        size="sm"
        icon={theme() === "light" ? "sun" : "moon"}
        aria-label="Toggle theme"
        title="Toggle theme"
        onClick={() => setTheme(theme() === "light" ? "dark" : "light")}
      />
    </div>
  );
};

export default Toolbar;
