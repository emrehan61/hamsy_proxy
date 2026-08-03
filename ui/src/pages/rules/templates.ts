// One-click starting points for "New rule". Each builds a full RuleInput
// (matcher + one action) ready to POST as-is; the editor opens immediately
// afterward so the user can fill in the blanks.

import type { RuleInput } from "../../lib/api";
import type { Matcher, UrlOp } from "../../lib/types";

function emptyMatcher(urlOp: UrlOp = "contains"): Matcher {
  return {
    urlOp,
    urlValue: "",
    methods: [],
    hostPorts: [],
    statusCodes: [],
    resourceTypes: [],
    requestHeaders: [],
    responseHeaders: [],
    requestBody: null,
    responseBody: null,
  };
}

function rule(name: string, match: Matcher, actions: RuleInput["actions"]): RuleInput {
  return { name, enabled: true, priority: 0, group: null, notes: null, match, actions };
}

export interface RuleTemplate {
  id: string;
  label: string;
  description: string;
  build: () => RuleInput;
}

export const RULE_TEMPLATES: RuleTemplate[] = [
  {
    id: "redirect",
    label: "Redirect URL",
    description: "Send matching requests to a different URL.",
    build: () => rule("Redirect URL", emptyMatcher(), [{ type: "redirect", to: "" }]),
  },
  {
    id: "mock",
    label: "Mock response",
    description: "Short-circuit matching requests with a canned response.",
    build: () =>
      rule("Mock response", emptyMatcher(), [
        { type: "mockResponse", status: 200, headers: [{ name: "Content-Type", value: "application/json" }], body: "{}", encoding: "text", delayMs: 0 },
      ]),
  },
  {
    id: "req-headers",
    label: "Modify request headers",
    description: "Set a header on the outgoing request.",
    build: () => rule("Modify request headers", emptyMatcher(), [{ type: "setRequestHeader", name: "X-Debug", value: "1" }]),
  },
  {
    id: "res-headers",
    label: "Modify response headers",
    description: "Set a header on the incoming response.",
    build: () => rule("Modify response headers", emptyMatcher(), [{ type: "setResponseHeader", name: "Cache-Control", value: "no-store" }]),
  },
  {
    id: "req-body",
    label: "Modify request body",
    description: "Replace the outgoing request body.",
    build: () =>
      rule("Modify request body", emptyMatcher(), [{ type: "setRequestBody", body: "{}", encoding: "text", contentType: "application/json" }]),
  },
  {
    id: "res-body",
    label: "Modify response body",
    description: "Replace the incoming response body.",
    build: () =>
      rule("Modify response body", emptyMatcher(), [{ type: "setResponseBody", body: "{}", encoding: "text", contentType: "application/json" }]),
  },
  {
    id: "block",
    label: "Block request",
    description: "Refuse matching requests outright.",
    build: () => rule("Block request", emptyMatcher(), [{ type: "block", reason: "Blocked by rule" }]),
  },
  {
    id: "delay",
    label: "Add delay",
    description: "Slow down matching requests.",
    build: () => rule("Add delay", emptyMatcher(), [{ type: "delay", ms: 1000 }]),
  },
  {
    id: "throttle",
    label: "Throttle bandwidth",
    description: "Cap transfer speed for matching requests.",
    build: () => rule("Throttle bandwidth", emptyMatcher(), [{ type: "throttle", bytesPerSec: 51_200 }]),
  },
  {
    id: "rewrite-json",
    label: "Rewrite response JSON",
    description: "Patch fields in a JSON response body.",
    build: () => rule("Rewrite response JSON", emptyMatcher(), [{ type: "jsonPatchResponse", ops: [{ op: "set", path: "", value: null }] }]),
  },
  {
    id: "status",
    label: "Change status code",
    description: "Force a different response status code.",
    build: () => rule("Change status code", emptyMatcher(), [{ type: "setStatus", status: 200 }]),
  },
  {
    id: "blank",
    label: "Blank rule",
    description: "Start from scratch with no conditions or actions.",
    build: () => rule("New rule", emptyMatcher("any"), []),
  },
];
