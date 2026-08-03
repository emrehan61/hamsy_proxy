// Renders a request/response body, branching on content-type + payload kind.
// Never throws: every decode/parse step is try/caught with a graceful
// fallback, since bodies are untrusted/arbitrary data from the wire.

import type { Component, JSX } from "solid-js";
import { Match, Show, Switch, createMemo, createSignal } from "solid-js";
import type { BodyPayload, HeaderPair } from "../lib/types";
import { formatBytes } from "../lib/format";
import { triggerDownload } from "../lib/download";
import { pushToast } from "../stores/ui";
import JsonTree from "./JsonTree";
import HexViewer from "./HexViewer";
import HeadersTable from "./HeadersTable";
import Button from "./Button";

export interface BodyViewerProps {
  body: BodyPayload | null | undefined;
  /**
   * Precedence: when `contentTypeHeader` is explicitly passed (including
   * `null`, meaning "caller looked and there isn't one"), it wins outright
   * and `headers` is not scanned. When it's omitted entirely (`undefined`,
   * the default when the prop isn't passed), `headers` is scanned
   * case-insensitively for a `content-type` entry instead.
   */
  contentTypeHeader?: string | null;
  headers?: HeaderPair[];
}

function resolveContentType(props: BodyViewerProps): string | null {
  if (props.contentTypeHeader !== undefined) return props.contentTypeHeader;
  const found = (props.headers ?? []).find((h) => h.name.toLowerCase() === "content-type");
  return found ? found.value : null;
}

type EffectiveEncoding = "text" | "base64";

function effectiveEncoding(body: BodyPayload): EffectiveEncoding {
  if (body.encoding === "base64" || body.encoding === "text") return body.encoding;
  return body.kind === "base64" ? "base64" : "text";
}

/**
 * Decodes `body.data` to a JS string regardless of whether it's stored as
 * literal text or base64 — used by the JSON/urlencoded/plain-text branches,
 * which need decoded text even when the wire payload was transported as
 * base64. Returns null (never throws) if decoding fails.
 */
function decodeBodyText(body: BodyPayload): string | null {
  try {
    if (effectiveEncoding(body) === "base64") {
      const binary = atob(body.data);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
      return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
    }
    return body.data;
  } catch {
    return null;
  }
}

type RenderInfo =
  | { kind: "empty" }
  | { kind: "json"; value: unknown; copyText: string }
  | { kind: "image"; dataUrl: string; mime: string; copyBase64: string }
  | { kind: "image-unavailable" }
  | { kind: "urlencoded"; pairs: HeaderPair[]; copyText: string }
  | { kind: "text"; text: string }
  | { kind: "hex"; base64: string }
  | { kind: "undecodable" };

function looksLikeJson(text: string): boolean {
  const trimmed = text.trim();
  return trimmed.startsWith("{") || trimmed.startsWith("[");
}

function computeRenderInfo(body: BodyPayload, mime: string | null): RenderInfo {
  try {
    if (body.kind === "none") return { kind: "empty" };

    const mimeBase = mime ? (mime.split(";")[0]?.trim().toLowerCase() ?? null) : null;
    const decoded = decodeBodyText(body);

    const shouldTryJson =
      (mimeBase !== null && mimeBase.includes("json")) ||
      (mimeBase === null && decoded !== null && looksLikeJson(decoded));

    if (shouldTryJson && decoded !== null) {
      try {
        return { kind: "json", value: JSON.parse(decoded), copyText: decoded };
      } catch {
        // Parse failure falls through to the remaining branches below,
        // per spec ("on parse failure fall through to plain text").
      }
    }

    if (mimeBase !== null && mimeBase.startsWith("image/")) {
      if (effectiveEncoding(body) === "base64") {
        return { kind: "image", dataUrl: `data:${mimeBase};base64,${body.data}`, mime: mimeBase, copyBase64: body.data };
      }
      return { kind: "image-unavailable" };
    }

    if (mimeBase === "application/x-www-form-urlencoded" && decoded !== null) {
      const params = new URLSearchParams(decoded);
      const pairs: HeaderPair[] = Array.from(params.entries()).map(([name, value]) => ({ name, value }));
      return { kind: "urlencoded", pairs, copyText: decoded };
    }

    if (effectiveEncoding(body) === "text") {
      return decoded !== null ? { kind: "text", text: decoded } : { kind: "undecodable" };
    }

    // Binary/unknown base64 that isn't JSON/image/urlencoded.
    return { kind: "hex", base64: body.data };
  } catch {
    return { kind: "undecodable" };
  }
}

const ImageBody: Component<{ dataUrl: string; mime: string; size: number }> = (props) => {
  const [dims, setDims] = createSignal<{ w: number; h: number } | null>(null);
  return (
    <div class="body-viewer__image-wrap">
      <img
        src={props.dataUrl}
        alt="Response body preview"
        class="body-viewer__image"
        onLoad={(e) => {
          const img = e.currentTarget;
          setDims({ w: img.naturalWidth, h: img.naturalHeight });
        }}
      />
      <div class="body-viewer__image-caption mono">
        {props.mime} · {formatBytes(props.size)}
        <Show when={dims()}>{(d) => ` · ${d().w}×${d().h}`}</Show>
      </div>
    </div>
  );
};

const BodyViewer: Component<BodyViewerProps> = (props) => {
  const mime = createMemo(() => resolveContentType(props));
  const mimeLabel = createMemo(() => mime() ?? "unknown");

  const info = createMemo<RenderInfo>(() => {
    if (!props.body) return { kind: "empty" };
    return computeRenderInfo(props.body, mime());
  });

  const copyText = async (text: string, label: string): Promise<void> => {
    try {
      await navigator.clipboard.writeText(text);
      pushToast({ level: "success", message: `Copied ${label}` });
    } catch {
      pushToast({ level: "error", message: `Failed to copy ${label}` });
    }
  };

  const downloadText = (text: string, filename: string, mimeType: string): void => {
    triggerDownload(new Blob([text], { type: mimeType }), filename);
  };

  const headerActions = (): JSX.Element | null => {
    const current = info();
    const body = props.body;
    if (!body || current.kind === "empty") return null;
    switch (current.kind) {
      case "json":
        return (
          <>
            <Button variant="ghost" size="sm" icon="copy" onClick={() => void copyText(current.copyText, "body")}>
              Copy
            </Button>
            <Button
              variant="ghost"
              size="sm"
              icon="download"
              onClick={() => downloadText(current.copyText, "body.json", "application/json")}
            >
              Download
            </Button>
          </>
        );
      case "text":
        return (
          <>
            <Button variant="ghost" size="sm" icon="copy" onClick={() => void copyText(current.text, "body")}>
              Copy
            </Button>
            <Button variant="ghost" size="sm" icon="download" onClick={() => downloadText(current.text, "body.txt", "text/plain")}>
              Download
            </Button>
          </>
        );
      case "urlencoded":
        return (
          <Button variant="ghost" size="sm" icon="copy" onClick={() => void copyText(current.copyText, "body")}>
            Copy
          </Button>
        );
      case "image":
        return (
          <Button variant="ghost" size="sm" icon="copy" onClick={() => void copyText(current.copyBase64, "base64")}>
            Copy base64
          </Button>
        );
      case "hex":
        return (
          <Button variant="ghost" size="sm" icon="copy" onClick={() => void copyText(body.data, "base64")}>
            Copy base64
          </Button>
        );
      default:
        return null;
    }
  };

  return (
    <div class="body-viewer">
      <Show when={props.body?.kind === "truncated"}>
        <div class="body-viewer__truncated-banner">Truncated ({formatBytes(props.body?.size ?? 0)})</div>
      </Show>
      <Show when={props.body && info().kind !== "empty"}>
        <div class="body-viewer__header">
          <span class="body-viewer__mime mono">{mimeLabel()}</span>
          <span class="body-viewer__size mono">{formatBytes(props.body?.size ?? 0)}</span>
          <div class="body-viewer__actions">{headerActions()}</div>
        </div>
      </Show>
      <div class="body-viewer__content">
        <Switch fallback={<div class="body-viewer__empty">No body</div>}>
          <Match when={info().kind === "json"}>
            <JsonTree data={(info() as { kind: "json"; value: unknown }).value} />
          </Match>
          <Match when={info().kind === "image"}>
            {(() => {
              const i = info() as { kind: "image"; dataUrl: string; mime: string };
              return <ImageBody dataUrl={i.dataUrl} mime={i.mime} size={props.body?.size ?? 0} />;
            })()}
          </Match>
          <Match when={info().kind === "image-unavailable"}>
            <div class="body-viewer__note">Binary image data unavailable as text.</div>
          </Match>
          <Match when={info().kind === "urlencoded"}>
            <HeadersTable headers={(info() as { kind: "urlencoded"; pairs: HeaderPair[] }).pairs} title="Form fields" />
          </Match>
          <Match when={info().kind === "text"}>
            <pre class="body-viewer__text mono">{(info() as { kind: "text"; text: string }).text}</pre>
          </Match>
          <Match when={info().kind === "hex"}>
            <HexViewer base64={(info() as { kind: "hex"; base64: string }).base64} />
          </Match>
          <Match when={info().kind === "undecodable"}>
            <div class="body-viewer__note">Unable to decode body.</div>
          </Match>
        </Switch>
      </div>
    </div>
  );
};

export default BodyViewer;
