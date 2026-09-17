// Browser-style session tab strip for the Traffic page: a permanent "Live"
// tab (the always-capturing WS-fed flow store) plus one tab per imported
// HAR session, in `sessions()` order. Selecting a tab flips
// `activeSessionId()` in the shared harSessions store; Traffic.tsx reacts to
// that to swap between the live SplitPane layout and a read-only
// HarSessionView.
//
// Tabs are ARIA `role="tab"` <div>s rather than real <button>s: each HAR
// tab needs its own nested close <button>, and a <button> cannot contain
// another <button>. Roving tabIndex + Left/Right navigation is managed by
// hand here (mirroring Tabs.tsx's focusTabAt/handleKeyDown, adapted to a
// plain ref-map instead of a querySelectorAll scan).

import type { Component } from "solid-js";
import { For, Show, createEffect, onCleanup, onMount } from "solid-js";
import type { HarSession } from "../stores/harSessions";
import { activeSessionId, closeSession, importHarFile, sessions, setActiveSession } from "../stores/harSessions";
import { flowCount } from "../stores/flows";
import { pushToast } from "../stores/ui";
import Icon from "./Icon";
import { viewerOnly } from "../stores/appMode";

/**
 * Imports every file in `files`, toasting one success/failure message per
 * file, then activates the last successfully-imported session. Exported
 * from here — rather than added to the finished, do-not-modify
 * harSessions.ts data layer — because it's UI glue (toasts + tab
 * activation) shared by two call sites: Traffic.tsx's live-toolbar
 * "Import HAR" button, and Traffic.tsx's drag&drop handler. One
 * implementation keeps "import N files -> N toasts -> activate the last
 * one" identical across both.
 */
export async function importHarFileList(files: FileList | File[]): Promise<void> {
  const list = Array.from(files);
  let lastSession: HarSession | undefined;
  for (const file of list) {
    try {
      const session = await importHarFile(file);
      lastSession = session;
      pushToast({ level: "success", message: `Imported ${session.name} — ${session.flowCount} flows` });
    } catch (err) {
      pushToast({ level: "error", message: `Failed to import ${file.name}: ${err instanceof Error ? err.message : "unknown error"}` });
    }
  }
  if (lastSession) setActiveSession(lastSession.id);
}

// Duplicated verbatim from Traffic.tsx's private `isTextInputTarget` rather
// than imported — same "tiny helper, copy don't couple" precedent
// harSessions.ts's header comment already sets for `generateUuid`.
function isTextInputTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;
}

const SessionTabs: Component = () => {
  // Plain (non-reactive) ref map, `null` sentinel for the Live tab — kept in
  // sync as tabs mount/unmount via each tab's `ref` callback.
  const tabRefs = new Map<string | null, HTMLElement>();

  const orderedIds = (): (string | null)[] => [null, ...sessions().map((s) => s.id)];

  const activateAndFocus = (id: string | null) => {
    setActiveSession(id);
    tabRefs.get(id)?.focus();
  };

  const onTabKeyDown = (e: KeyboardEvent, id: string | null) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      setActiveSession(id);
      return;
    }
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      e.preventDefault();
      const ids = orderedIds();
      const idx = ids.indexOf(id);
      if (idx === -1) return;
      const delta = e.key === "ArrowRight" ? 1 : -1;
      const next = ids[(idx + delta + ids.length) % ids.length];
      if (next !== undefined) activateAndFocus(next);
    }
  };

  // Ctrl/Cmd+W closes the active HAR tab (never Live). Best-effort only:
  // most browsers reserve Ctrl/Cmd+W for closing the browser tab itself and
  // don't let page scripts intercept it — this only helps in contexts that
  // do allow it (e.g. some embedded/kiosk webviews).
  const onWindowKeyDown = (e: KeyboardEvent) => {
    if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "w") return;
    if (isTextInputTarget(e.target)) return;
    const id = activeSessionId();
    if (id === null) return;
    e.preventDefault();
    closeSession(id);
  };

  onMount(() => {
    window.addEventListener("keydown", onWindowKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onWindowKeyDown));
  });

  // Keep the active tab scrolled into view within the strip whenever it
  // changes (keyboard nav, programmatic activation after import, etc).
  // `scrollIntoView({block:"nearest"})` is a no-op if already visible.
  createEffect(() => {
    const id = activeSessionId();
    tabRefs.get(id)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });

  return (
    <div class="session-tabs">
      <div class="session-tabs__strip" role="tablist">
        <div
          ref={(el) => tabRefs.set(null, el)}
          role="tab"
          class={`session-tabs__tab${activeSessionId() === null ? " session-tabs__tab--active" : ""}`}
          aria-selected={activeSessionId() === null}
          tabIndex={activeSessionId() === null ? 0 : -1}
          onClick={() => setActiveSession(null)}
          onKeyDown={(e) => onTabKeyDown(e, null)}
        >
          <Icon name={viewerOnly() ? "file" : "zap"} size={14} class="session-tabs__tab-icon" />
          <span class="session-tabs__tab-name">{viewerOnly() ? "Open HAR" : "Live"}</span>
          <Show when={!viewerOnly()}><span class="session-tabs__tab-count mono">{flowCount()}</span></Show>
        </div>

        <For each={sessions()}>
          {(session) => (
            <div
              ref={(el) => tabRefs.set(session.id, el)}
              role="tab"
              class={`session-tabs__tab${activeSessionId() === session.id ? " session-tabs__tab--active" : ""}`}
              aria-selected={activeSessionId() === session.id}
              tabIndex={activeSessionId() === session.id ? 0 : -1}
              title={session.name}
              onClick={() => setActiveSession(session.id)}
              onKeyDown={(e) => onTabKeyDown(e, session.id)}
              onAuxClick={(e) => {
                if (e.button === 1) {
                  e.preventDefault();
                  closeSession(session.id);
                }
              }}
            >
              <Icon name="file" size={14} class="session-tabs__tab-icon" />
              <span class="session-tabs__tab-name">{session.name}</span>
              <span class="session-tabs__tab-count mono">{session.flowCount}</span>
              <button
                type="button"
                class="session-tabs__tab-close"
                aria-label={`Close ${session.name}`}
                onClick={(e) => {
                  e.stopPropagation();
                  closeSession(session.id);
                }}
              >
                <Icon name="close" size={12} />
              </button>
            </div>
          )}
        </For>
      </div>
    </div>
  );
};

export default SessionTabs;
