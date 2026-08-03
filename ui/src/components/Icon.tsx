// Inline feather-icons-like SVG icon set. No external icon library — every
// icon is a small line-drawing built from primitive SVG shapes so the whole
// app ships zero extra icon-font/sprite dependency.

import type { Component, JSX } from "solid-js";

export type IconName =
  | "play"
  | "pause"
  | "trash"
  | "download"
  | "replay"
  | "copy"
  | "search"
  | "close"
  | "chevron-down"
  | "chevron-up"
  | "chevron-right"
  | "filter"
  | "settings"
  | "list"
  | "wrench"
  | "sun"
  | "moon"
  | "circle"
  | "check"
  | "external-link"
  | "arrow-up"
  | "arrow-down"
  | "dot"
  | "zap"
  | "image"
  | "file"
  | "code"
  | "hash"
  | "plug"
  | "plug-off"
  | "clock"
  | "alert-triangle"
  | "info"
  | "plus"
  | "drag-handle"
  | "more-vertical";

export interface IconProps {
  name: IconName;
  size?: number;
  class?: string;
}

function renderPaths(name: IconName): JSX.Element {
  switch (name) {
    case "play":
      return <polygon points="5 3 19 12 5 21 5 3" />;
    case "pause":
      return (
        <>
          <rect x="6" y="4" width="4" height="16" rx="1" />
          <rect x="14" y="4" width="4" height="16" rx="1" />
        </>
      );
    case "trash":
      return (
        <>
          <polyline points="3 6 5 6 21 6" />
          <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
          <path d="M10 11v6" />
          <path d="M14 11v6" />
          <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
        </>
      );
    case "download":
      return (
        <>
          <path d="M12 3v12" />
          <polyline points="7 10 12 15 17 10" />
          <path d="M5 19h14" />
        </>
      );
    case "replay":
      return (
        <>
          <polyline points="1 4 1 10 7 10" />
          <path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" />
        </>
      );
    case "copy":
      return (
        <>
          <rect x="9" y="9" width="11" height="11" rx="2" />
          <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
        </>
      );
    case "search":
      return (
        <>
          <circle cx="11" cy="11" r="7" />
          <line x1="21" y1="21" x2="16.65" y2="16.65" />
        </>
      );
    case "close":
      return (
        <>
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </>
      );
    case "chevron-down":
      return <polyline points="6 9 12 15 18 9" />;
    case "chevron-up":
      return <polyline points="18 15 12 9 6 15" />;
    case "chevron-right":
      return <polyline points="9 18 15 12 9 6" />;
    case "filter":
      return <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" />;
    case "settings":
      return (
        <>
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </>
      );
    case "list":
      return (
        <>
          <line x1="8" y1="6" x2="21" y2="6" />
          <line x1="8" y1="12" x2="21" y2="12" />
          <line x1="8" y1="18" x2="21" y2="18" />
          <line x1="3" y1="6" x2="3.01" y2="6" />
          <line x1="3" y1="12" x2="3.01" y2="12" />
          <line x1="3" y1="18" x2="3.01" y2="18" />
        </>
      );
    case "wrench":
      return (
        <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94z" />
      );
    case "sun":
      return (
        <>
          <circle cx="12" cy="12" r="5" />
          <line x1="12" y1="1" x2="12" y2="3" />
          <line x1="12" y1="21" x2="12" y2="23" />
          <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
          <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
          <line x1="1" y1="12" x2="3" y2="12" />
          <line x1="21" y1="12" x2="23" y2="12" />
          <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
          <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
        </>
      );
    case "moon":
      return <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />;
    case "circle":
      return <circle cx="12" cy="12" r="9" />;
    case "check":
      return <polyline points="20 6 9 17 4 12" />;
    case "external-link":
      return (
        <>
          <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
          <polyline points="15 3 21 3 21 9" />
          <line x1="10" y1="14" x2="21" y2="3" />
        </>
      );
    case "arrow-up":
      return (
        <>
          <line x1="12" y1="19" x2="12" y2="5" />
          <polyline points="5 12 12 5 19 12" />
        </>
      );
    case "arrow-down":
      return (
        <>
          <line x1="12" y1="5" x2="12" y2="19" />
          <polyline points="19 12 12 19 5 12" />
        </>
      );
    case "dot":
      return <circle cx="12" cy="12" r="3" fill="currentColor" stroke="none" />;
    case "zap":
      return <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />;
    case "image":
      return (
        <>
          <rect x="3" y="3" width="18" height="18" rx="2" />
          <circle cx="8.5" cy="8.5" r="1.5" />
          <polyline points="21 15 16 10 5 21" />
        </>
      );
    case "file":
      return (
        <>
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </>
      );
    case "code":
      return (
        <>
          <polyline points="16 18 22 12 16 6" />
          <polyline points="8 6 2 12 8 18" />
        </>
      );
    case "hash":
      return (
        <>
          <line x1="4" y1="9" x2="20" y2="9" />
          <line x1="4" y1="15" x2="20" y2="15" />
          <line x1="10" y1="3" x2="8" y2="21" />
          <line x1="16" y1="3" x2="14" y2="21" />
        </>
      );
    case "plug":
      return (
        <>
          <path d="M12 22v-6" />
          <path d="M9 8V2" />
          <path d="M15 8V2" />
          <path d="M5 8h14v3a7 7 0 0 1-14 0z" />
        </>
      );
    case "plug-off":
      return (
        <>
          <path d="M12 22v-6" />
          <path d="M9 8V2" />
          <path d="M15 8V2" />
          <path d="M5 8h14v3a7 7 0 0 1-14 0z" />
          <line x1="2" y1="2" x2="22" y2="22" />
        </>
      );
    case "clock":
      return (
        <>
          <circle cx="12" cy="12" r="9" />
          <polyline points="12 7 12 12 15 15" />
        </>
      );
    case "alert-triangle":
      return (
        <>
          <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
          <line x1="12" y1="9" x2="12" y2="13" />
          <line x1="12" y1="17" x2="12.01" y2="17" />
        </>
      );
    case "info":
      return (
        <>
          <circle cx="12" cy="12" r="9" />
          <line x1="12" y1="16" x2="12" y2="12" />
          <line x1="12" y1="8" x2="12.01" y2="8" />
        </>
      );
    case "plus":
      return (
        <>
          <line x1="12" y1="5" x2="12" y2="19" />
          <line x1="5" y1="12" x2="19" y2="12" />
        </>
      );
    case "drag-handle":
      return (
        <>
          <circle cx="8" cy="6" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="16" cy="6" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="8" cy="12" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="16" cy="12" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="8" cy="18" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="16" cy="18" r="1.2" fill="currentColor" stroke="none" />
        </>
      );
    case "more-vertical":
      return (
        <>
          <circle cx="12" cy="5" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="12" cy="12" r="1.2" fill="currentColor" stroke="none" />
          <circle cx="12" cy="19" r="1.2" fill="currentColor" stroke="none" />
        </>
      );
  }
}

const Icon: Component<IconProps> = (props) => {
  return (
    <svg
      aria-hidden="true"
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      class={props.class}
    >
      {renderPaths(props.name)}
    </svg>
  );
};

export default Icon;
