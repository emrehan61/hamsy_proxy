// Pure formatting helpers. No Solid imports — safe to use anywhere, including
// off the reactive graph (workers, plain TS, tests).

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0 B";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = n / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const digits = value < 10 ? 1 : 0;
  return `${value.toFixed(digits)} ${units[unitIndex]}`;
}

export function formatDuration(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms)) return "-";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export function formatTimestamp(ms: number): string {
  const date = new Date(ms);
  const time = date.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
  const millis = String(date.getMilliseconds()).padStart(3, "0");
  return `${time}.${millis}`;
}

export function formatDateShort(ms: number): string {
  const date = new Date(ms);
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export type StatusClass = "1xx" | "2xx" | "3xx" | "4xx" | "5xx" | "-";

export function statusClassOf(status: number | null): StatusClass {
  if (status === null) return "-";
  if (status >= 100 && status < 200) return "1xx";
  if (status >= 200 && status < 300) return "2xx";
  if (status >= 300 && status < 400) return "3xx";
  if (status >= 400 && status < 500) return "4xx";
  if (status >= 500 && status < 600) return "5xx";
  return "-";
}

export function statusColor(status: number | null): string {
  const cls = statusClassOf(status);
  switch (cls) {
    case "2xx":
      return "var(--green)";
    case "3xx":
      return "var(--cyan)";
    case "4xx":
      return "var(--amber)";
    case "5xx":
      return "var(--red)";
    case "1xx":
      return "var(--fg-dim)";
    case "-":
    default:
      return "var(--fg-muted)";
  }
}

export function methodColor(method: string): string {
  switch (method.toUpperCase()) {
    case "GET":
      return "var(--green)";
    case "POST":
      return "var(--cyan)";
    case "PUT":
      return "var(--amber)";
    case "PATCH":
      return "var(--purple)";
    case "DELETE":
      return "var(--red)";
    case "OPTIONS":
      return "var(--fg-dim)";
    case "HEAD":
      return "var(--fg-dim)";
    default:
      return "var(--fg-muted)";
  }
}

export function prettyJson(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * Lightweight regex-based JSON syntax highlighter — NOT a full parser.
 * Callers must already know `text` is JSON-ish; malformed input is still
 * highlighted best-effort but not validated. Returns an HTML string meant
 * for `innerHTML` inside a `<pre>`.
 */
export function highlightJson(text: string): string {
  const escaped = escapeHtml(text);
  const tokenPattern =
    /("(\\u[a-fA-F0-9]{4}|\\[^u]|[^\\"])*"(\s*:)?)|\b(true|false)\b|\bnull\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g;
  return escaped.replace(tokenPattern, (match) => {
    if (match.startsWith('"')) {
      const cls = match.endsWith(":") || /:\s*$/.test(match) ? "jsonkey" : "jsonstr";
      return `<span class="${cls}">${match}</span>`;
    }
    if (match === "true" || match === "false") {
      return `<span class="jsonbool">${match}</span>`;
    }
    if (match === "null") {
      return `<span class="jsonnull">${match}</span>`;
    }
    return `<span class="jsonnum">${match}</span>`;
  });
}
