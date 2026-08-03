import type { Component } from "solid-js";
import { For, Show, createMemo } from "solid-js";

export interface HexViewerProps {
  /** Matches BodyPayload.data when kind === "base64". */
  base64: string;
  maxBytes?: number;
}

const DEFAULT_MAX_BYTES = 65536;
const BYTES_PER_ROW = 16;

interface DecodedResult {
  ok: boolean;
  bytes: Uint8Array;
  totalLength: number;
}

function decodeBase64(base64: string): DecodedResult {
  try {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return { ok: true, bytes, totalLength: bytes.length };
  } catch {
    return { ok: false, bytes: new Uint8Array(0), totalLength: 0 };
  }
}

interface HexRow {
  offset: number;
  bytes: number[];
}

function toRows(bytes: Uint8Array): HexRow[] {
  const rows: HexRow[] = [];
  for (let offset = 0; offset < bytes.length; offset += BYTES_PER_ROW) {
    rows.push({ offset, bytes: Array.from(bytes.slice(offset, offset + BYTES_PER_ROW)) });
  }
  return rows;
}

function toHexPair(byte: number): string {
  return byte.toString(16).padStart(2, "0");
}

function toAscii(byte: number): string {
  return byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : ".";
}

// Renders every row eagerly, capped by maxBytes — NOT virtualized. Fine at
// this cap (default 64KB); a later phase's JsonTree owns the virtualization
// story for much larger payloads.
const HexViewer: Component<HexViewerProps> = (props) => {
  const maxBytes = () => props.maxBytes ?? DEFAULT_MAX_BYTES;

  const decoded = createMemo(() => decodeBase64(props.base64));
  const visibleBytes = createMemo(() => decoded().bytes.slice(0, maxBytes()));
  const rows = createMemo(() => toRows(visibleBytes()));

  return (
    <Show
      when={decoded().ok}
      fallback={<div class="hex-viewer__error">Unable to decode body as binary data</div>}
    >
      <div class="hex-viewer">
        <Show when={decoded().totalLength > maxBytes()}>
          <div class="hex-viewer__note">
            Showing first {maxBytes()} of {decoded().totalLength} bytes
          </div>
        </Show>
        <pre class="hex-viewer__grid mono">
          <For each={rows()}>
            {(row) => (
              <div class="hex-viewer__row">
                <span class="hex-viewer__offset">{row.offset.toString(16).padStart(8, "0")}</span>
                <span class="hex-viewer__bytes">
                  <For each={row.bytes}>
                    {(byte, i) => (
                      <span class={i() === 7 ? "hex-viewer__byte hex-viewer__byte--gap" : "hex-viewer__byte"}>
                        {toHexPair(byte)}
                      </span>
                    )}
                  </For>
                </span>
                <span class="hex-viewer__ascii">
                  <For each={row.bytes}>
                    {(byte) => <span class="hex-viewer__ascii-char">{toAscii(byte)}</span>}
                  </For>
                </span>
              </div>
            )}
          </For>
        </pre>
      </div>
    </Show>
  );
};

export default HexViewer;
