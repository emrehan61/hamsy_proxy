import type { Component } from "solid-js";
import { For, createMemo } from "solid-js";
import type { Timings } from "../lib/types";
import { formatDuration } from "../lib/format";

export interface TimingsBarProps {
  timings: Timings;
  /** Optional total (ms) to scale segments against; defaults to the sum of phases. */
  total?: number;
}

interface Phase {
  key: keyof Timings;
  label: string;
  color: string;
}

const PHASES: Phase[] = [
  { key: "blocked", label: "Blocked", color: "var(--fg-dim)" },
  { key: "dns", label: "DNS", color: "var(--purple)" },
  { key: "connect", label: "Connect", color: "var(--cyan)" },
  { key: "ssl", label: "SSL", color: "var(--amber)" },
  { key: "send", label: "Send", color: "var(--green)" },
  { key: "wait", label: "Wait", color: "var(--accent)" },
  { key: "receive", label: "Receive", color: "var(--fg-muted)" },
];

function clampNonNegative(v: number | undefined): number {
  if (v === undefined || !Number.isFinite(v) || v < 0) return 0;
  return v;
}

const TimingsBar: Component<TimingsBarProps> = (props) => {
  const values = createMemo(() => PHASES.map((phase) => clampNonNegative(props.timings[phase.key])));

  const total = createMemo(() => {
    const explicit = props.total;
    if (explicit !== undefined && Number.isFinite(explicit) && explicit > 0) return explicit;
    const sum = values().reduce((acc, v) => acc + v, 0);
    return sum > 0 ? sum : 1;
  });

  const widths = createMemo(() => values().map((v) => (v / total()) * 100));

  return (
    <div class="timings-bar">
      <div class="timings-bar__track">
        <For each={PHASES}>
          {(phase, index) => (
            <div
              class="timings-bar__segment"
              style={{ width: `${widths()[index()]}%`, "background-color": phase.color }}
              title={`${phase.label}: ${formatDuration(values()[index()] ?? 0)}`}
            />
          )}
        </For>
      </div>
      <ul class="timings-bar__legend">
        <For each={PHASES}>
          {(phase, index) => (
            <li class="timings-bar__legend-item">
              <span class="timings-bar__legend-swatch" style={{ "background-color": phase.color }} />
              <span class="timings-bar__legend-label">{phase.label}</span>
              <span class="timings-bar__legend-value mono">{formatDuration(values()[index()] ?? 0)}</span>
            </li>
          )}
        </For>
      </ul>
    </div>
  );
};

export default TimingsBar;
