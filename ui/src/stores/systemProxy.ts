// System proxy state store: the one place that fetches GET /api/state and
// drives POST /api/system-proxy. The Toolbar's always-visible control and
// the Settings page's toggle both read/write through here so they can never
// disagree — see Toolbar.tsx's SystemProxyControl and Settings.tsx's
// "System proxy" section.
//
// The real state can change with nothing telling this tab: `hamsy proxy off`
// from the CLI, another browser tab, or hamsy shutting down. Since there's
// no WS push for it, this resource is re-fetched on window focus and on a
// modest interval (in addition to right after every toggle) so the UI drifts
// back to reality instead of going stale.

import { createResource, createSignal } from "solid-js";
import { getState, setSystemProxy as apiSetSystemProxy } from "../lib/api";
import type { ApiState } from "../lib/types";
import { pushToast } from "./ui";

const [apiStateResource, { refetch: refetchApiStateResource }] = createResource(getState);

/** Full `GET /api/state` snapshot, or `undefined` until the first fetch resolves. */
export function apiState(): ApiState | undefined {
  return apiStateResource();
}

export function refetchApiState(): void {
  void refetchApiStateResource();
}

const [systemProxyBusySignal, setSystemProxyBusySignal] = createSignal(false);

/** True while a `POST /api/system-proxy` request is in flight — it shells out to OS commands and can take a moment. */
export function systemProxyBusy(): boolean {
  return systemProxyBusySignal();
}

/**
 * Flips the OS system proxy on/off. Never applies an optimistic value —
 * callers read `apiState().systemProxy` for display, which only ever
 * reflects a confirmed server response, so a failed request can't leave the
 * control showing the wrong state.
 */
export async function toggleSystemProxy(next: boolean): Promise<void> {
  setSystemProxyBusySignal(true);
  try {
    await apiSetSystemProxy(next);
  } catch {
    pushToast({ level: "error", message: `Failed to turn ${next ? "on" : "off"} the system proxy` });
  } finally {
    // Re-sync from the server either way: on success this picks up the
    // confirmed value, and on failure it guards against the OS command
    // having partially applied despite the 502.
    await refetchApiStateResource();
    setSystemProxyBusySignal(false);
  }
}

// ---- background refresh (focus + modest interval) ----

const REFRESH_INTERVAL_MS = 20_000;
let refreshInitialized = false;

/** Call once (from App.tsx) to keep this resource from going stale while the tab sits open. */
export function initSystemProxySync(): void {
  if (refreshInitialized) return;
  refreshInitialized = true;
  if (typeof window === "undefined") return;
  window.addEventListener("focus", () => refetchApiState());
  setInterval(() => refetchApiState(), REFRESH_INTERVAL_MS);
}
