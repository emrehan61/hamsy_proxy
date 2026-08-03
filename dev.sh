#!/usr/bin/env bash
# dev.sh — run hamsy-proxy locally for development: the Rust backend
# (cargo run -p hamsy-cli) and/or the Vite UI dev server, together or
# separately, with one command.
#
# Bash-3.2-compatible on purpose (macOS ships bash 3.2 as /bin/bash): no
# associative arrays, no ${var,,}/${var^^}, no `wait -n`.

set -euo pipefail

# ---------------------------------------------------------------------------
# Output helpers (mirrors install.sh's style)
# ---------------------------------------------------------------------------

COLOR=0
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  COLOR=1
fi

info() {
  if [ "$COLOR" -eq 1 ]; then
    printf '\033[1;36m==>\033[0m %s\n' "$*"
  else
    printf '==> %s\n' "$*"
  fi
}

warn() {
  if [ "$COLOR" -eq 1 ]; then
    printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2
  else
    printf 'warn: %s\n' "$*" >&2
  fi
}

die() {
  if [ "$COLOR" -eq 1 ]; then
    printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
  else
    printf 'error: %s\n' "$*" >&2
  fi
  exit 1
}

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------------------
# Script location — resolve once so every path below is relative to the repo
# checkout, not to wherever the caller happened to be sitting.
# ---------------------------------------------------------------------------

# shellcheck disable=SC2164 # set -e already aborts here if cd fails; the
# fallback error message would just be less friendly.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---------------------------------------------------------------------------
# Usage / mode
# ---------------------------------------------------------------------------

usage() {
  cat <<'EOF'
Usage: dev.sh [both|build|backend|ui] [--help]

Runs hamsy-proxy locally for development, from this checkout, no install
needed. With no argument (or "both"), runs the backend and the UI dev
server concurrently.

Modes:
  both      (default) cargo run -p hamsy-cli  +  cd ui && pnpm dev
            Backend: proxy on http://127.0.0.1:9080, web UI/API on
            http://127.0.0.1:9081. UI dev server (hot reload) on
            http://localhost:5173, proxying /api and /cert to 9081
            (see ui/vite.config.ts). Open http://localhost:5173.

  build     cd ui && pnpm build, then cargo run -p hamsy-cli. The
            binary serves the built UI straight off ./ui/dist (checked
            before the packaged placeholder page) — one server, no Vite.
            Open http://127.0.0.1:9081.

  backend   Just cargo run -p hamsy-cli (proxy on 9080, UI/API on
            9081). Serves ./ui/dist if it's already built, else a
            placeholder page. Pair with `./dev.sh ui` for hot reload.

  ui        Just cd ui && pnpm dev (http://localhost:5173). Expects a
            backend already running on 127.0.0.1:9081 — start one
            separately with `./dev.sh backend`.

Options:
  --help, -h   Show this help and exit

Every hamsy-cli run above passes --manual, so your OS-wide proxy
settings are left untouched — hamsy-proxy binds 127.0.0.1:9080 same as
always, it just won't ask the system to route traffic through it.
Set HAMSY_DEV_SYSTEM_PROXY=1 to run with --system-proxy instead, if
you actually need to test the real system-proxy behavior.

Ctrl-C stops everything dev.sh started, cleanly — no leftover cargo/vite/
node processes on 9080, 9081, or 5173.
EOF
}

MODE="both"
case "${1:-}" in
  "") ;;
  both | build | backend | ui) MODE="$1" ;;
  --help | -h | help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

if [ $# -gt 1 ]; then
  usage >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------

require_cargo() {
  have cargo || die "cargo not found. Install Rust: https://rustup.rs"
}

require_pnpm() {
  if ! have pnpm; then
    die "pnpm not found. Try: corepack enable pnpm (or see https://pnpm.io/installation)."
  fi
}

ensure_ui_deps() {
  if [ ! -d "$ROOT_DIR/ui/node_modules" ]; then
    info "ui/node_modules missing — running pnpm install..."
    (cd "$ROOT_DIR/ui" && pnpm install)
  fi
}

# ---------------------------------------------------------------------------
# Port checks — fail up front naming the port, rather than dying confusingly
# once cargo/vite are already starting up.
# ---------------------------------------------------------------------------

port_in_use() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}

check_port() {
  port="$1"
  label="$2"
  if port_in_use "$port"; then
    die "Port $port ($label) is already in use. Stop whatever's using it, then re-run."
  fi
}

# ---------------------------------------------------------------------------
# System-proxy flag for dev runs — hamsy-proxy now flips on the OS-wide system
# proxy by default. Dev servers restart constantly (edit, crash, Ctrl-C,
# repeat), and if a dev run left the machine's proxy setting pointed at
# 127.0.0.1:9080 and then died, every app on the box would silently lose
# network access until someone noticed and fixed it by hand. So every
# hamsy-cli invocation below passes --manual, which leaves the OS proxy
# alone entirely.
#
# Set HAMSY_DEV_SYSTEM_PROXY=1 to opt back into the real system-wide
# behaviour anyway — e.g. to actually exercise the macOS/Windows/Linux
# proxy-setting code during development. This swaps --manual for
# --system-proxy (belt-and-braces: forces the system proxy on, same as
# the new default, but explicit here since --manual is what it's replacing).
# ---------------------------------------------------------------------------

HAMSY_FLAG="--manual"
if [ "${HAMSY_DEV_SYSTEM_PROXY:-0}" = "1" ]; then
  HAMSY_FLAG="--system-proxy"
  warn "HAMSY_DEV_SYSTEM_PROXY=1 — this run will change your OS-wide proxy settings."
fi

# ---------------------------------------------------------------------------
# Single-process modes — exec straight into the command so Ctrl-C is
# delivered by the terminal to it (and anything it spawns) directly; there's
# no dev.sh wrapper process left around to leak anything.
# ---------------------------------------------------------------------------

run_backend() {
  require_cargo
  check_port 9080 "proxy"
  check_port 9081 "web UI/API"
  info "Starting backend only: cargo run -p hamsy-cli -- $HAMSY_FLAG"
  info "Proxy   http://127.0.0.1:9080"
  info "Web UI  http://127.0.0.1:9081"
  echo
  cd "$ROOT_DIR"
  exec cargo run -p hamsy-cli -- "$HAMSY_FLAG"
}

run_ui() {
  require_pnpm
  ensure_ui_deps
  check_port 5173 "vite dev server"
  info "Starting UI dev server only: cd ui && pnpm dev"
  info "UI  http://localhost:5173 (proxies /api, /cert to 127.0.0.1:9081 — start a backend separately)"
  echo
  cd "$ROOT_DIR/ui"
  exec pnpm dev
}

run_build() {
  require_cargo
  require_pnpm
  ensure_ui_deps
  check_port 9080 "proxy"
  check_port 9081 "web UI/API"
  info "Building UI: cd ui && pnpm build"
  (cd "$ROOT_DIR/ui" && pnpm build)
  info "Starting backend, serving the built UI off ./ui/dist: cargo run -p hamsy-cli -- $HAMSY_FLAG"
  info "Open http://127.0.0.1:9081"
  echo
  cd "$ROOT_DIR"
  exec cargo run -p hamsy-cli -- "$HAMSY_FLAG"
}

# ---------------------------------------------------------------------------
# Concurrent mode — both processes run in the foreground, prefixed output,
# clean teardown of both on Ctrl-C or on either exiting on its own.
#
# API_PID/UI_PID/CLEANED_UP and the functions below are top-level (not
# nested inside a function, no `local`) — see the "Dispatch" comment
# further down for why that matters.
# ---------------------------------------------------------------------------

API_PID=""
UI_PID=""
CLEANED_UP=0

kill_group() {
  pid="$1"
  kill -TERM "-$pid" 2>/dev/null || true
  i=0
  while [ "$i" -lt 50 ]; do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 0.1
    i=$((i + 1))
  done
  # Still alive after 5s of SIGTERM grace — force it.
  kill -KILL "-$pid" 2>/dev/null || true
}

cleanup() {
  if [ "$CLEANED_UP" -eq 1 ]; then
    return
  fi
  CLEANED_UP=1
  [ -n "$API_PID" ] && kill_group "$API_PID"
  [ -n "$UI_PID" ] && kill_group "$UI_PID"
  [ -n "$API_PID" ] && wait "$API_PID" 2>/dev/null
  [ -n "$UI_PID" ] && wait "$UI_PID" 2>/dev/null
  true
}

on_int() {
  trap - INT EXIT
  echo
  info "Stopping..."
  cleanup
  exit 130 # 128 + SIGINT
}

on_term() {
  trap - TERM EXIT
  echo
  info "Stopping..."
  cleanup
  exit 143 # 128 + SIGTERM
}

# ---------------------------------------------------------------------------
# Dispatch. "both" is deliberately inlined here rather than called as a
# function like the other modes: with `set -m` active, calling `exit` from
# a trap while a *function* is still on the call stack — even one that
# doesn't use `local` — can hit a bash bug ("pop_var_context: head of
# shell_variables not a function context"), reproduced repeatedly while
# testing this script under a real Ctrl-C in a real terminal (both bash
# 3.2 and a newer bash). Cleanup still ran correctly every time it
# happened, but it's an alarming line to print over a plain Ctrl-C, so
# "both" runs with no function frame on the stack at all to avoid it.
# ---------------------------------------------------------------------------

case "$MODE" in
  build)
    run_build
    exit 0
    ;;
  backend)
    run_backend
    exit 0
    ;;
  ui)
    run_ui
    exit 0
    ;;
esac

require_cargo
require_pnpm
ensure_ui_deps
check_port 9080 "proxy"
check_port 9081 "web UI/API"
check_port 5173 "vite dev server"

if [ "$COLOR" -eq 1 ]; then
  API_TAG=$'\033[1;35m[api]\033[0m'
  UI_TAG=$'\033[1;34m[ui]\033[0m'
else
  API_TAG='[api]'
  UI_TAG='[ui]'
fi

# Job control on, so each job backgrounded below gets its own process
# group (pgid == its own pid). That's what lets cleanup() below kill an
# entire tree — cargo plus the hamsy child it spawns, or pnpm plus the
# vite/node process it spawns — with one `kill -- -PID`, instead of
# leaving grandchildren orphaned and still listening on 9080/9081/5173.
set -m

trap cleanup EXIT
trap on_int INT
trap on_term TERM

(
  cd "$ROOT_DIR"
  cargo run -p hamsy-cli -- "$HAMSY_FLAG" 2>&1 | while IFS= read -r line; do
    printf '%s %s\n' "$API_TAG" "$line"
  done
) &
API_PID=$!

(
  cd "$ROOT_DIR/ui"
  pnpm dev 2>&1 | while IFS= read -r line; do
    printf '%s %s\n' "$UI_TAG" "$line"
  done
) &
UI_PID=$!

echo
info "Backend: proxy http://127.0.0.1:9080, web UI/API http://127.0.0.1:9081"
info "UI dev server: http://localhost:5173 (proxies /api, /cert to 9081)"
echo
info "Open http://localhost:5173"
info "Press Ctrl-C to stop both."
echo

EXIT_CODE=0
DIED=""
while true; do
  if ! kill -0 "$API_PID" 2>/dev/null; then
    wait "$API_PID" || EXIT_CODE=$?
    DIED="backend"
    break
  fi
  if ! kill -0 "$UI_PID" 2>/dev/null; then
    wait "$UI_PID" || EXIT_CODE=$?
    DIED="ui"
    break
  fi
  sleep 0.5
done

warn "$DIED process exited (code $EXIT_CODE) — stopping the other one."
exit "$EXIT_CODE"
