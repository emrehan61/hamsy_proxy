// App shell: sidebar + offline banner + routed page content + toast host.
//
// Deviation from the phase spec: @solidjs/router 0.15 does not export an
// `<Outlet/>` component (checked node_modules/@solidjs/router/dist — no
// such export exists in this version). Instead, a router's `root` prop
// takes a `Component<RouteSectionProps>` that receives the matched route
// tree as `props.children` and decides where to render it. App is written
// to that contract: it renders `props.children` where the spec describes
// `<Outlet/>`, and a future phase's router setup wires it up via
// `<Router root={App}>...</Router>`.

import "./styles/global.css";
import type { Component, JSX } from "solid-js";
import { ErrorBoundary, Show, createSignal, onCleanup, onMount } from "solid-js";
import { getState } from "./lib/api";
import { initSettingsSync } from "./stores/settings";
import { initFlowsSync } from "./stores/flows";
import { initRulesSync } from "./stores/rules";
import { initSystemProxySync } from "./stores/systemProxy";
import { wsClient } from "./lib/ws";
import Sidebar from "./components/Sidebar";
import ToastHost from "./components/Toast";

export interface AppProps {
  children?: JSX.Element;
}

// How long the WS status has to stay in a non-open state before the
// offline banner shows up for that reason too (avoids flashing the banner
// during the normal brief "connecting" window on page load).
const WS_OFFLINE_GRACE_MS = 4000;

const App: Component<AppProps> = (props) => {
  const [backendReachable, setBackendReachable] = createSignal(true);
  const [wsSustainedDown, setWsSustainedDown] = createSignal(false);

  let wsDownTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    initSettingsSync();
    initFlowsSync();
    initRulesSync();
    initSystemProxySync();

    getState()
      .then(() => setBackendReachable(true))
      .catch(() => setBackendReachable(false));

    const unsubscribe = wsClient.onStatusChange((status) => {
      if (status === "open") {
        if (wsDownTimer !== undefined) {
          clearTimeout(wsDownTimer);
          wsDownTimer = undefined;
        }
        setWsSustainedDown(false);
        setBackendReachable(true);
        return;
      }
      if (wsDownTimer === undefined) {
        wsDownTimer = setTimeout(() => setWsSustainedDown(true), WS_OFFLINE_GRACE_MS);
      }
    });

    onCleanup(() => {
      unsubscribe();
      if (wsDownTimer !== undefined) clearTimeout(wsDownTimer);
    });
  });

  return (
    <div class="app-shell">
      <Sidebar />
      <div class="app-shell__main">
        <Show when={!backendReachable() || wsSustainedDown()}>
          <div class="app-shell__offline-banner" role="status">
            Backend not reachable — retrying…
          </div>
        </Show>
        <div class="app-shell__content">
          <ErrorBoundary
            fallback={(err, reset) => (
              <div class="app-shell__error">
                <p class="app-shell__error-message">Something went wrong: {String(err instanceof Error ? err.message : err)}</p>
                <button type="button" class="btn btn--default btn--sm" onClick={() => reset()}>
                  Reload
                </button>
              </div>
            )}
          >
            {props.children}
          </ErrorBoundary>
        </div>
      </div>
      <ToastHost />
    </div>
  );
};

export default App;
