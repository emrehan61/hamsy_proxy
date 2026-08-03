import type { Component, JSX } from "solid-js";
import { createSignal } from "solid-js";
import { getSplitSize, setSplitSize } from "../stores/ui";

export interface SplitPaneProps {
  /**
   * "horizontal" = two panes side by side, split by a vertical drag bar
   * (dragging left/right resizes the first/left pane's width).
   * "vertical" = two panes stacked, split by a horizontal drag bar
   * (dragging up/down resizes the first/top pane's height).
   */
  direction: "horizontal" | "vertical";
  /** Key passed to getSplitSize/setSplitSize to persist the split ratio. */
  sizeKey: string;
  min?: number;
  max?: number;
  initial?: number;
  /**
   * Panes are explicit props (not `children`) so each slot is unambiguous
   * and type-safe, rather than relying on children-array ordering.
   */
  first: JSX.Element;
  second: JSX.Element;
}

function clamp(v: number, min: number, max: number): number {
  return Math.min(Math.max(v, min), max);
}

const SplitPane: Component<SplitPaneProps> = (props) => {
  const min = () => props.min ?? 200;
  const max = () => props.max ?? Infinity;
  const initial = () => props.initial ?? 640;

  const [size, setSize] = createSignal(clamp(getSplitSize(props.sizeKey, initial()), min(), max()));
  const [isDragging, setIsDragging] = createSignal(false);

  let dragging = false;
  let startPos = 0;
  let startSize = 0;

  const onPointerDown = (e: PointerEvent) => {
    dragging = true;
    setIsDragging(true);
    startPos = props.direction === "horizontal" ? e.clientX : e.clientY;
    startSize = size();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };

  const onPointerMove = (e: PointerEvent) => {
    if (!dragging) return;
    const pos = props.direction === "horizontal" ? e.clientX : e.clientY;
    const delta = pos - startPos;
    setSize(clamp(startSize + delta, min(), max()));
  };

  const onPointerUp = (e: PointerEvent) => {
    if (!dragging) return;
    dragging = false;
    setIsDragging(false);
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    setSplitSize(props.sizeKey, size());
  };

  return (
    <div class={`split-pane split-pane--${props.direction}`}>
      <div
        class="split-pane__first"
        style={
          props.direction === "horizontal"
            ? { width: `${size()}px`, "flex-basis": `${size()}px` }
            : { height: `${size()}px`, "flex-basis": `${size()}px` }
        }
      >
        {props.first}
      </div>
      <div
        class={`split-pane__divider split-pane__divider--${props.direction}${
          isDragging() ? " split-pane__divider--dragging" : ""
        }`}
        role="separator"
        aria-orientation={props.direction === "horizontal" ? "vertical" : "horizontal"}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      />
      <div class="split-pane__second">{props.second}</div>
    </div>
  );
};

export default SplitPane;
