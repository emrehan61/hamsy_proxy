// Left nav rail. Self-contained (no props) — reads the route directly via
// `useLocation` so it always reflects the current page without a parent
// having to thread anything down.
//
// Visual density: compact icon-over-label vertical rail (like a devtools
// panel switcher) rather than a wide icon+label sidebar — this is a proxy
// inspector where the traffic table wants maximum horizontal space, so the
// nav itself should cost as little width as possible.

import type { Component } from "solid-js";
import { For } from "solid-js";
import { A, useLocation } from "@solidjs/router";
import Icon, { type IconName } from "./Icon";

interface NavEntry {
  href: string;
  label: string;
  icon: IconName;
}

const NAV_ENTRIES: NavEntry[] = [
  { href: "/", label: "Traffic", icon: "list" },
  { href: "/rules", label: "Rules", icon: "filter" },
  { href: "/settings", label: "Settings", icon: "settings" },
  { href: "/setup", label: "Setup", icon: "wrench" },
];

function isActive(pathname: string, href: string): boolean {
  if (href === "/") return pathname === "/";
  return pathname === href || pathname.startsWith(`${href}/`);
}

const Sidebar: Component = () => {
  const location = useLocation();

  return (
    <nav class="sidebar" aria-label="Primary">
      <img src="/favicon.ico" alt="flproxy" class="sidebar__logo" />
      <For each={NAV_ENTRIES}>
        {(entry) => (
          <A
            href={entry.href}
            class={`sidebar__item${isActive(location.pathname, entry.href) ? " sidebar__item--active" : ""}`}
            title={entry.label}
            aria-label={entry.label}
          >
            <Icon name={entry.icon} size={18} class="sidebar__icon" />
            <span class="sidebar__label">{entry.label}</span>
          </A>
        )}
      </For>
    </nav>
  );
};

export default Sidebar;
