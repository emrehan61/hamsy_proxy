// Settings store: a Solid resource wrapping GET /api/settings, plus the
// dark/light/system theme preference (persisted to localStorage, applied to
// the DOM synchronously at module load to avoid a flash of the wrong theme).

import { createResource, createSignal } from "solid-js";
import { getSettings, updateSettings as apiUpdateSettings } from "../lib/api";
import type { Settings } from "../lib/types";
import { wsClient } from "../lib/ws";

// ---- settings resource ----

async function fetchSettings(): Promise<Settings> {
  return getSettings();
}

const [settingsResource, { refetch: refetchSettingsResource, mutate: mutateSettingsResource }] =
  createResource(fetchSettings);

/** Current settings, or `undefined` until the initial fetch resolves. */
export function settings(): Settings | undefined {
  return settingsResource();
}

export function refetchSettings(): Settings | Promise<Settings | undefined> | undefined | null {
  return refetchSettingsResource();
}

export function settingsLoadError(): unknown {
  return settingsResource.error as unknown;
}

// Set when the server reports `restartRequired: true` on a settings PUT
// (e.g. changing proxyPort/uiPort/bindAddr). Settings.tsx shows a persistent
// banner while this is true; it stays true across further edits until
// explicitly dismissed, since the underlying restart is still pending.
const [restartRequiredSignal, setRestartRequiredSignal] = createSignal(false);

export function settingsRestartRequired(): boolean {
  return restartRequiredSignal();
}

export function dismissRestartRequired(): void {
  setRestartRequiredSignal(false);
}

/**
 * Applies `patch` optimistically to the local resource, then persists ONLY
 * `patch` via PUT /api/settings (the server accepts a partial object — never
 * send the whole settings object blindly). On success the server's
 * (possibly normalized) response replaces the local value. On failure the
 * optimistic patch is rolled back and the error is rethrown for the caller
 * to surface (e.g. via the ui store's toast queue).
 */
export async function setSettings(patch: Partial<Settings>): Promise<Settings> {
  const previous = settingsResource();
  const merged = previous ? { ...previous, ...patch } : undefined;
  if (merged) mutateSettingsResource(merged);
  try {
    const saved = await apiUpdateSettings(patch);
    mutateSettingsResource(saved);
    if (saved.restartRequired) setRestartRequiredSignal(true);
    return saved;
  } catch (err) {
    if (previous) mutateSettingsResource(previous);
    throw err;
  }
}

/**
 * Wires the settings resource to `settingsChanged` WS pushes so other
 * clients' edits stay in sync. Lazy — call once from App.tsx (phase 2).
 */
let settingsSyncInitialized = false;

export function initSettingsSync(): void {
  if (settingsSyncInitialized) return;
  settingsSyncInitialized = true;
  wsClient.onMessage((msg) => {
    if (msg.type === "settingsChanged") {
      mutateSettingsResource(msg.settings);
    }
  });
}

// ---- theme ----

export type ThemeChoice = "dark" | "light" | "system";

const THEME_STORAGE_KEY = "rdproxy.theme";

function resolveInitialThemeChoice(): ThemeChoice {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY);
    if (stored === "dark" || stored === "light" || stored === "system") return stored;
  } catch {
    // localStorage unavailable (privacy mode, SSR, etc.) — fall through.
  }
  return "system";
}

function resolveEffectiveTheme(choice: ThemeChoice): "dark" | "light" {
  if (choice === "system") {
    const prefersLight = typeof window !== "undefined" && window.matchMedia?.("(prefers-color-scheme: light)").matches;
    return prefersLight ? "light" : "dark";
  }
  return choice;
}

function applyThemeToDom(choice: ThemeChoice): void {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.theme = resolveEffectiveTheme(choice);
}

const [themeChoiceSignal, setThemeChoiceSignal] = createSignal<ThemeChoice>(resolveInitialThemeChoice());

// Applied directly (not inside a component/effect) so the correct theme is on
// the DOM before Solid's first render, avoiding a flash of the wrong theme.
applyThemeToDom(themeChoiceSignal());

// Keep the DOM in sync if the OS-level preference changes while "system" is
// selected. Plain DOM listener rather than a Solid effect since this module
// runs outside any component/root.
if (typeof window !== "undefined" && window.matchMedia) {
  const media = window.matchMedia("(prefers-color-scheme: light)");
  const handleChange = () => {
    if (themeChoiceSignal() === "system") applyThemeToDom("system");
  };
  media.addEventListener?.("change", handleChange);
}

export function theme(): ThemeChoice {
  return themeChoiceSignal();
}

export function setTheme(t: ThemeChoice): void {
  setThemeChoiceSignal(t);
  try {
    localStorage.setItem(THEME_STORAGE_KEY, t);
  } catch {
    // Ignore persistence failures (e.g. storage disabled).
  }
  applyThemeToDom(t);
}
