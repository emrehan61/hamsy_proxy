import { apiState } from "./systemProxy";

let lastKnownViewerMode: boolean | undefined;

function currentMode(): boolean | undefined {
  try {
    const state = apiState();
    if (state) lastKnownViewerMode = state.viewerOnly === true;
  } catch {
    // Keep imported HAR tabs usable if a background state refresh fails.
    // A disconnected viewer must never turn into a capture UI.
  }
  return lastKnownViewerMode;
}

/** Shared server mode; the CLI HAR viewer never exposes capture controls. */
export function viewerOnly(): boolean {
  return currentMode() === true;
}

export function appModeReady(): boolean {
  return currentMode() !== undefined;
}
