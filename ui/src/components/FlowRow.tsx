// Single flow row — hot path, rendered for every visible virtualized item
// across potentially 50k+ flows. Kept presentational: no internal effects
// beyond what FLOW_COLUMNS iteration requires, no signals of its own.

import type { Component, JSX } from "solid-js";
import { Show } from "solid-js";
import type { FlowSummary } from "../lib/types";
import { formatBytes, formatDuration, formatTimestamp, methodColor, statusClassOf, statusColor } from "../lib/format";
import { FLOW_COLUMNS, type FlowColumn } from "./FlowTable";

export interface FlowRowProps {
  flow: FlowSummary;
  selected: boolean;
  style?: JSX.CSSProperties | string;
  onClick: () => void;
  columnWidths: Record<string, number>;
}

function isPending(flow: FlowSummary): boolean {
  return flow.state === "pending" || flow.state === "requesting";
}

function isFailed(flow: FlowSummary): boolean {
  return flow.state === "error" || flow.error !== null;
}

function statusRowClass(flow: FlowSummary): string {
  if (isFailed(flow)) return "flow-row--error";
  const cls = statusClassOf(flow.status);
  return cls === "1xx" || cls === "-" ? "" : `flow-row--${cls}`;
}

function renderCell(flow: FlowSummary, col: FlowColumn): JSX.Element {
  switch (col.key) {
    case "seq":
      return <span class="flow-row__seq mono">{flow.seq}</span>;
    case "method":
      return (
        <span class="flow-row__method mono" style={{ color: methodColor(flow.method) }}>
          {flow.method}
        </span>
      );
    case "status":
      return (
        <Show
          when={flow.status !== null}
          fallback={<span class="flow-row__status mono">{isPending(flow) ? "…" : "-"}</span>}
        >
          <span class="flow-row__status mono" style={{ color: statusColor(flow.status) }}>
            {flow.status}
          </span>
        </Show>
      );
    case "host":
      return (
        <span class="flow-row__host mono truncate" title={flow.host}>
          {flow.host}
        </span>
      );
    case "path":
      return (
        <span class={`flow-row__path mono truncate${isFailed(flow) ? " flow-row__path--failed" : ""}`} title={flow.url}>
          {flow.path}
        </span>
      );
    case "type":
      return <span class="flow-row__type truncate">{flow.resourceType}</span>;
    case "size":
      return <span class="flow-row__size mono">{formatBytes(flow.requestSize + flow.responseSize)}</span>;
    case "time":
      return <span class="flow-row__time mono">{formatTimestamp(flow.startedAt)}</span>;
    case "duration":
      return <span class="flow-row__duration mono">{formatDuration(flow.durationMs)}</span>;
    default:
      return null;
  }
}

const FlowRow: Component<FlowRowProps> = (props) => {
  const widthStyle = (col: FlowColumn): JSX.CSSProperties => {
    const w = `${props.columnWidths[col.key] ?? col.defaultWidth}px`;
    return { width: w, "flex-basis": w };
  };

  return (
    <div
      role="row"
      class={`flow-row ${statusRowClass(props.flow)}${props.selected ? " flow-row--selected" : ""}${
        isPending(props.flow) ? " flow-row--pending" : ""
      }`}
      style={props.style}
      onClick={props.onClick}
    >
      <Show when={props.flow.modified}>
        <span class="flow-row__modified-dot" aria-hidden="true" />
      </Show>
      {FLOW_COLUMNS.map((col) => (
        <div class={`flow-row__cell flow-row__cell--${col.key}`} role="gridcell" style={widthStyle(col)}>
          {renderCell(props.flow, col)}
        </div>
      ))}
    </div>
  );
};

export default FlowRow;
