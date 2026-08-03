// Pure, DOM-free mirror of the Rust matcher semantics (flproxy-core) used by
// the Rules page's live match tester and its inline validators. Every export
// here is a plain function over plain data — safe to unit test directly, and
// safe to call from components without pulling in Solid reactivity.

import type { Action, Matcher, UrlOp } from "./types";

// ---- wildcard globs (used for hostPorts and urlOp:"wildcard") ----

const REGEX_SPECIAL = /[.+^${}()|[\]\\]/;

function escapeRegexChar(ch: string): string {
  return REGEX_SPECIAL.test(ch) ? `\\${ch}` : ch;
}

/** Translates a `*`/`?` glob into an anchored, case-insensitive RegExp. */
export function wildcardToRegExp(pattern: string): RegExp {
  let src = "";
  for (const ch of pattern) {
    if (ch === "*") src += ".*";
    else if (ch === "?") src += ".";
    else src += escapeRegexChar(ch);
  }
  return new RegExp(`^${src}$`, "i");
}

export function matchesWildcard(pattern: string, value: string): boolean {
  try {
    return wildcardToRegExp(pattern).test(value);
  } catch {
    return false;
  }
}

// ---- urlOp matching ----

export interface UrlMatchResult {
  matches: boolean;
  /** Regex capture groups (index 0 = $1), empty for non-regex ops. */
  captures: string[];
  error?: string;
}

export function matchUrl(op: UrlOp, value: string, url: string): UrlMatchResult {
  switch (op) {
    case "any":
      return { matches: true, captures: [] };
    case "contains":
      return { matches: url.includes(value), captures: [] };
    case "equals":
      return { matches: url === value, captures: [] };
    case "startsWith":
      return { matches: url.startsWith(value), captures: [] };
    case "endsWith":
      return { matches: url.endsWith(value), captures: [] };
    case "wildcard":
      return { matches: matchesWildcard(value, url), captures: [] };
    case "regex": {
      try {
        const re = new RegExp(value);
        const m = re.exec(url);
        if (!m) return { matches: false, captures: [] };
        return { matches: true, captures: m.slice(1).map((g) => g ?? "") };
      } catch (err) {
        return { matches: false, captures: [], error: err instanceof Error ? err.message : "Invalid regex" };
      }
    }
  }
}

/** Returns an error message if `pattern` is not a valid RegExp, else null. */
export function validateRegex(pattern: string): string | null {
  try {
    new RegExp(pattern);
    return null;
  } catch (err) {
    return err instanceof Error ? err.message : "Invalid regex";
  }
}

/**
 * Approximate count of capturing groups in `pattern`, used only to hint
 * `$1`/`$2` availability in redirect/rewrite targets — a syntactic scan, not
 * a full regex parser, so exotic patterns may be slightly off.
 */
export function countCaptureGroups(pattern: string): number {
  let count = 0;
  let escaped = false;
  let inClass = false;
  for (let i = 0; i < pattern.length; i++) {
    const ch = pattern[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (ch === "\\") {
      escaped = true;
      continue;
    }
    if (ch === "[") {
      inClass = true;
      continue;
    }
    if (ch === "]") {
      inClass = false;
      continue;
    }
    if (inClass) continue;
    if (ch === "(") {
      if (pattern[i + 1] === "?") {
        // Named captures "(?<name>...)" count; non-capturing "(?:" and
        // lookaround "(?=" "(?!" "(?<=" "(?<!" do not.
        const isLookbehind = pattern[i + 2] === "<" && (pattern[i + 3] === "=" || pattern[i + 3] === "!");
        const isNamed = pattern[i + 2] === "<" && !isLookbehind;
        if (isNamed) count++;
      } else {
        count++;
      }
    }
  }
  return count;
}

// ---- methods ----

export function matchesMethod(methods: string[], method: string): boolean {
  if (methods.length === 0) return true;
  const upper = method.toUpperCase();
  return methods.some((m) => m.toUpperCase() === upper);
}

// ---- host:port globs ----

export function matchesHostPort(patterns: string[], hostPort: string): boolean {
  if (patterns.length === 0) return true;
  return patterns.some((p) => matchesWildcard(p, hostPort));
}

// ---- status code patterns: "200" | "4xx" | "500-599" ----

export type StatusPatternKind = "exact" | "class" | "range" | "invalid";

export function statusPatternKind(pattern: string): StatusPatternKind {
  const trimmed = pattern.trim();
  if (/^\d{3}$/.test(trimmed)) return "exact";
  if (/^[1-5]xx$/i.test(trimmed)) return "class";
  if (/^\d{3}-\d{3}$/.test(trimmed)) return "range";
  return "invalid";
}

export function isValidStatusPattern(pattern: string): boolean {
  return statusPatternKind(pattern) !== "invalid";
}

export function matchesStatusPattern(pattern: string, status: number): boolean {
  const trimmed = pattern.trim();
  const kind = statusPatternKind(trimmed);
  if (kind === "exact") return Number(trimmed) === status;
  if (kind === "class") return Math.floor(status / 100) === Number(trimmed[0]);
  if (kind === "range") {
    const [lo, hi] = trimmed.split("-").map(Number);
    return status >= (lo ?? 0) && status <= (hi ?? 0);
  }
  return false;
}

export function matchesStatusCodes(patterns: string[], status: number | null): boolean {
  if (patterns.length === 0) return true;
  if (status === null) return false;
  return patterns.some((p) => matchesStatusPattern(p, status));
}

// ---- live match tester ----

export interface MatchCheck {
  label: string;
  passed: boolean;
  detail?: string;
}

export interface MatchTestInput {
  url: string;
  method: string;
  status: number | null;
}

/**
 * Evaluates the URL/method/status conditions of `matcher` against `input`
 * for the Rules page's live tester. Header/body conditions aren't tested
 * here — the tester only has a URL/method/status to work with, matching the
 * spec's "evaluates the URL/method/status conditions client-side".
 */
export function testMatcher(matcher: Matcher, input: MatchTestInput): MatchCheck[] {
  const checks: MatchCheck[] = [];

  if (matcher.urlOp !== "any") {
    const result = matchUrl(matcher.urlOp, matcher.urlValue, input.url);
    checks.push({
      label: `URL ${matcher.urlOp} "${matcher.urlValue}"`,
      passed: result.matches,
      detail: result.error ?? (result.captures.length > 0 ? `captures: ${result.captures.join(", ")}` : undefined),
    });
  }

  if (matcher.methods.length > 0) {
    checks.push({
      label: `Method in [${matcher.methods.join(", ")}]`,
      passed: matchesMethod(matcher.methods, input.method),
    });
  }

  if (matcher.statusCodes.length > 0) {
    checks.push({
      label: `Status matches [${matcher.statusCodes.join(", ")}]`,
      passed: matchesStatusCodes(matcher.statusCodes, input.status),
      detail: input.status === null ? "no status provided" : undefined,
    });
  }

  return checks;
}

// ---- action-vs-matcher phase compatibility ----
//
// Only urlOp/urlValue, methods, hostPorts, resourceTypes, requestHeaders,
// requestBody are evaluated in the request phase. statusCodes,
// responseHeaders, responseBody require the response, so a rule using any of
// those can only ever fire once the response is known — by which point a
// request-phase-only action (it mutates the outgoing request, or mocks the
// response entirely instead of forwarding it) has already missed its chance
// to run.

const REQUEST_PHASE_ONLY_ACTIONS = new Set<Action["type"]>([
  "redirect",
  "rewriteUrl",
  "setQueryParam",
  "removeQueryParam",
  "setRequestHeader",
  "removeRequestHeader",
  "setRequestBody",
  "replaceInRequestBody",
  "jsonPatchRequest",
  "setMethod",
  "mockResponse",
  "block",
]);

export function matcherRequiresResponsePhase(m: Matcher): boolean {
  return m.statusCodes.length > 0 || m.responseHeaders.length > 0 || m.responseBody !== null;
}

/** Returns a human-readable warning if `actionType` can never fire under `matcher`, else null. */
export function actionPhaseWarning(matcher: Matcher, actionType: Action["type"]): string | null {
  if (!matcherRequiresResponsePhase(matcher)) return null;
  if (REQUEST_PHASE_ONLY_ACTIONS.has(actionType)) {
    return "This rule only matches once the response is known (status/response-header/response-body conditions), so this request-phase action can never run.";
  }
  return null;
}
