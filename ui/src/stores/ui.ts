// General UI state: toast queue + generic localStorage-backed persistence
// helpers used for split-pane sizes, column widths, etc.

import { createSignal } from "solid-js";

// ---- toasts ----

export type ToastLevel = "info" | "success" | "error" | "warning";

export interface Toast {
  id: string;
  level: ToastLevel;
  message: string;
  ttlMs?: number;
}

const DEFAULT_TOAST_TTL_MS = 4000;

const [toastsSignal, setToastsSignal] = createSignal<Toast[]>([]);

export function toasts(): Toast[] {
  return toastsSignal();
}

function nextToastId(): string {
  return `toast-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

export function pushToast(t: Omit<Toast, "id">): string {
  const id = nextToastId();
  const toast: Toast = { ttlMs: DEFAULT_TOAST_TTL_MS, ...t, id };
  setToastsSignal((prev) => [...prev, toast]);
  const ttl = toast.ttlMs ?? DEFAULT_TOAST_TTL_MS;
  if (ttl > 0) {
    setTimeout(() => dismissToast(id), ttl);
  }
  return id;
}

export function dismissToast(id: string): void {
  setToastsSignal((prev) => prev.filter((t) => t.id !== id));
}

// ---- generic persisted values ----

function storageKey(key: string): string {
  return `flproxy.${key}`;
}

export function getPersisted<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(storageKey(key));
    if (raw === null) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

export function setPersisted<T>(key: string, value: T): void {
  try {
    localStorage.setItem(storageKey(key), JSON.stringify(value));
  } catch {
    // Ignore persistence failures (e.g. storage disabled/full).
  }
}

// ---- split pane sizes (convenience wrappers over getPersisted/setPersisted) ----

export function getSplitSize(key: string, fallback: number): number {
  return getPersisted<number>(`split.${key}`, fallback);
}

export function setSplitSize(key: string, value: number): void {
  setPersisted<number>(`split.${key}`, value);
}
