// Per-action-type metadata: display labels, the canonical ordered list of
// types (drives the type <Select> in ActionsEditor), and a sensible default
// value for each type (used both when a card's type is switched and by
// templates.ts).

import type { Action } from "../../lib/types";

export const ACTION_TYPE_LABELS: Record<Action["type"], string> = {
  redirect: "Redirect URL",
  rewriteUrl: "Rewrite URL",
  setQueryParam: "Set query param",
  removeQueryParam: "Remove query param",
  setRequestHeader: "Set request header",
  removeRequestHeader: "Remove request header",
  setResponseHeader: "Set response header",
  removeResponseHeader: "Remove response header",
  setRequestBody: "Set request body",
  setResponseBody: "Set response body",
  replaceInRequestBody: "Replace in request body",
  replaceInResponseBody: "Replace in response body",
  jsonPatchRequest: "JSON patch request",
  jsonPatchResponse: "JSON patch response",
  mockResponse: "Mock response",
  setStatus: "Set status code",
  block: "Block request",
  delay: "Add delay",
  throttle: "Throttle bandwidth",
  setMethod: "Set method",
};

export const ACTION_TYPES = Object.keys(ACTION_TYPE_LABELS) as Action["type"][];

export function defaultActionFor(type: Action["type"]): Action {
  switch (type) {
    case "redirect":
      return { type, to: "" };
    case "rewriteUrl":
      return { type, find: "", replace: "", regex: false };
    case "setQueryParam":
      return { type, name: "", value: "" };
    case "removeQueryParam":
      return { type, name: "" };
    case "setRequestHeader":
      return { type, name: "", value: "" };
    case "removeRequestHeader":
      return { type, name: "" };
    case "setResponseHeader":
      return { type, name: "", value: "" };
    case "removeResponseHeader":
      return { type, name: "" };
    case "setRequestBody":
      return { type, body: "", encoding: "text", contentType: null };
    case "setResponseBody":
      return { type, body: "", encoding: "text", contentType: null };
    case "replaceInRequestBody":
      return { type, find: "", replace: "", regex: false };
    case "replaceInResponseBody":
      return { type, find: "", replace: "", regex: false };
    case "jsonPatchRequest":
      return { type, ops: [] };
    case "jsonPatchResponse":
      return { type, ops: [] };
    case "mockResponse":
      return { type, status: 200, headers: [], body: "", encoding: "text", delayMs: 0 };
    case "setStatus":
      return { type, status: 200 };
    case "block":
      return { type, reason: "Blocked by rule" };
    case "delay":
      return { type, ms: 1000 };
    case "throttle":
      return { type, bytesPerSec: 51200 };
    case "setMethod":
      return { type, method: "GET" };
    default: {
      const exhaustive: never = type;
      throw new Error(`Unknown action type: ${String(exhaustive)}`);
    }
  }
}

export const COMMON_HEADER_NAMES = [
  "Accept",
  "Accept-Encoding",
  "Accept-Language",
  "Authorization",
  "Cache-Control",
  "Content-Encoding",
  "Content-Length",
  "Content-Type",
  "Cookie",
  "Origin",
  "Referer",
  "Set-Cookie",
  "User-Agent",
  "X-Forwarded-For",
  "X-Requested-With",
];

export const HTTP_METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT"];

export const THROTTLE_PRESETS: { label: string; bytesPerSec: number }[] = [
  { label: "3G (400 kbps)", bytesPerSec: 50_000 },
  { label: "4G (4 Mbps)", bytesPerSec: 500_000 },
  { label: "DSL (2 Mbps)", bytesPerSec: 250_000 },
];
