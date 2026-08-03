#!/usr/bin/env bash
# install.sh — build flproxy from source and put it on your PATH.
#
# There's no prebuilt binary to fetch (no releases, no CI yet), so this
# script always builds from the checkout it's run from: UI first (Vite),
# then the Rust CLI with the UI embedded via rust-embed.
#
# Bash-3.2-compatible on purpose (macOS ships bash 3.2 as /bin/bash): no
# associative arrays, no ${var,,}/${var^^}, no mapfile/readarray.

set -euo pipefail

# ---------------------------------------------------------------------------
# Output helpers
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
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---------------------------------------------------------------------------
# Usage / flags
# ---------------------------------------------------------------------------

usage() {
  cat <<'EOF'
Usage: install.sh [OPTIONS]

Builds flproxy from source (Rust + the SolidJS UI) and installs the
resulting binary onto your PATH. Always builds from this checkout —
there is nothing to download.

Options:
  --prefix DIR    Install directory (default: $HOME/.local/bin)
  --yes, -y       Assume yes to prompts (e.g. installing Rust via rustup)
  --skip-deps     Only check dependencies; never install anything
  --no-ui         Skip the UI build; build flproxy without --features embed-ui
  --uninstall     Remove the installed binary (optionally ~/.flproxy too)
  --help, -h      Show this help and exit
EOF
}

PREFIX="$HOME/.local/bin"
YES=0
SKIP_DEPS=0
NO_UI=0
UNINSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)
      [ $# -ge 2 ] || die "--prefix requires an argument"
      PREFIX="$2"
      shift 2
      ;;
    --yes | -y)
      YES=1
      shift
      ;;
    --skip-deps)
      SKIP_DEPS=1
      shift
      ;;
    --no-ui)
      NO_UI=1
      shift
      ;;
    --uninstall)
      UNINSTALL=1
      shift
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Platform check — refuse anything we haven't built/tested for rather than
# fail confusingly deep inside a cargo/pnpm invocation.
# ---------------------------------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin | Linux) ;;
  *) die "Unsupported OS: '$OS'. Supported: Darwin (macOS), Linux." ;;
esac

case "$ARCH" in
  x86_64 | amd64 | arm64 | aarch64) ;;
  *) die "Unsupported architecture: '$ARCH'. Supported: x86_64/amd64, arm64/aarch64." ;;
esac

# ---------------------------------------------------------------------------
# Uninstall — short-circuits everything else below.
# ---------------------------------------------------------------------------

do_uninstall() {
  bin_path="$PREFIX/flproxy"
  if [ -e "$bin_path" ]; then
    rm -f "$bin_path"
    info "Removed $bin_path"
  else
    info "No binary found at $bin_path — nothing to remove."
  fi

  data_dir="${FLPROXY_HOME:-$HOME/.flproxy}"

  # A bare --yes never removes user data: settings/rules/CA key are too easy
  # to lose by accident. This always asks interactively, and only ever
  # defaults to "keep" when there's no tty to ask on.
  remove_data=0
  if [ -t 0 ]; then
    printf 'Also remove %s (settings, rules, CA cert/key)? [y/N] ' "$data_dir"
    reply=""
    read -r reply || true
    case "$reply" in
      y | Y | yes | YES) remove_data=1 ;;
      *) remove_data=0 ;;
    esac
  else
    warn "Non-interactive, no tty — keeping $data_dir. Re-run interactively to remove it."
  fi

  if [ "$remove_data" -eq 1 ]; then
    rm -rf "$data_dir"
    info "Removed $data_dir"
  else
    info "Kept $data_dir"
  fi
}

if [ "$UNINSTALL" -eq 1 ]; then
  do_uninstall
  exit 0
fi

# ---------------------------------------------------------------------------
# Dependency preflight — checked and reported as one batch, not fail-fast,
# so a run never dies on the first missing thing without mentioning the rest.
# ---------------------------------------------------------------------------

REPORT=()
PROBLEMS=0

install_rust() {
  info "Installing Rust via rustup..."
  if curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y; then
    # shellcheck disable=SC1091 # only exists after rustup just installed it
    . "$HOME/.cargo/env"
    PATH="$HOME/.cargo/bin:$PATH"
    export PATH
    return 0
  fi
  return 1
}

check_cargo() {
  if have cargo; then
    REPORT+=("  [ok]   cargo: $(cargo --version)")
    return
  fi

  if [ "$SKIP_DEPS" -eq 1 ]; then
    REPORT+=("  [FAIL] cargo not found. Install: https://rustup.rs")
    PROBLEMS=$((PROBLEMS + 1))
    return
  fi

  install_consent=0
  if [ "$YES" -eq 1 ]; then
    install_consent=1
  elif [ -t 0 ]; then
    printf 'cargo not found. Install Rust via rustup now? [y/N] '
    reply=""
    read -r reply || true
    case "$reply" in
      y | Y | yes | YES) install_consent=1 ;;
    esac
  fi

  if [ "$install_consent" -eq 1 ] && install_rust && have cargo; then
    REPORT+=("  [ok]   cargo: $(cargo --version) (just installed via rustup)")
  else
    REPORT+=("  [FAIL] cargo not found. Install: https://rustup.rs (curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)")
    PROBLEMS=$((PROBLEMS + 1))
  fi
}

check_node() {
  if [ "$NO_UI" -eq 1 ]; then
    REPORT+=("  [skip] node (--no-ui, UI build skipped)")
    return
  fi

  if ! have node; then
    REPORT+=("  [FAIL] node not found. Need Node >= 22. Install via nvm ('nvm install 22'), brew ('brew install node'), or your distro's package manager.")
    PROBLEMS=$((PROBLEMS + 1))
    return
  fi

  node_version="$(node --version)" # format: vX.Y.Z
  node_major="${node_version#v}"
  node_major="${node_major%%.*}"
  case "$node_major" in
    '' | *[!0-9]*) node_major=0 ;;
  esac

  if [ "$node_major" -ge 22 ]; then
    REPORT+=("  [ok]   node: $node_version")
  else
    REPORT+=("  [FAIL] node $node_version found, need >= 22. Install via nvm ('nvm install 22'), brew ('brew install node@22' or 'brew install node'), or your distro's package manager.")
    PROBLEMS=$((PROBLEMS + 1))
  fi
}

check_pnpm() {
  if [ "$NO_UI" -eq 1 ]; then
    REPORT+=("  [skip] pnpm (--no-ui, UI build skipped)")
    return
  fi

  pnpm_pin=""
  if [ -f "$SCRIPT_DIR/ui/package.json" ]; then
    pnpm_pin="$(grep -o '"packageManager"[[:space:]]*:[[:space:]]*"pnpm@[^"]*"' "$SCRIPT_DIR/ui/package.json" 2>/dev/null | sed -E 's/.*pnpm@([^"]*)".*/\1/')"
  fi

  if [ "$SKIP_DEPS" -eq 1 ]; then
    # Never touch corepack's config here — just confirm *something* usable
    # already exists on PATH.
    if have pnpm || have corepack; then
      REPORT+=("  [ok]   pnpm: available (corepack and/or pnpm on PATH; not activated, --skip-deps)")
    else
      REPORT+=("  [FAIL] no pnpm and no corepack found. Install Node >=16.9 (bundles corepack) or 'npm install -g pnpm'.")
      PROBLEMS=$((PROBLEMS + 1))
    fi
    return
  fi

  if have corepack; then
    # Prefer corepack + the version pinned in ui/package.json over a
    # hardcoded pnpm version here: the pin lives in exactly one place and
    # this script can never drift out of sync with it.
    corepack enable >/dev/null 2>&1 || true
    if [ -n "$pnpm_pin" ] && (cd "$SCRIPT_DIR/ui" && corepack prepare "pnpm@$pnpm_pin" --activate >/dev/null 2>&1); then
      pnpm_ver="$(cd "$SCRIPT_DIR/ui" && pnpm --version)"
      REPORT+=("  [ok]   pnpm: $pnpm_ver (via corepack, pinned to $pnpm_pin)")
      return
    fi
  fi

  if have pnpm; then
    # Running pnpm from inside ui/ matters here too: pnpm (or its corepack
    # shim) walks up the directory tree looking for a packageManager field,
    # and an ancestor package.json (e.g. in $HOME) can hijack that lookup.
    pnpm_ver="$(cd "$SCRIPT_DIR/ui" && pnpm --version 2>/dev/null || true)"
    if [ -n "$pnpm_ver" ]; then
      REPORT+=("  [ok]   pnpm: $pnpm_ver (pre-existing on PATH)")
      return
    fi
  fi

  REPORT+=("  [FAIL] pnpm unavailable: corepack couldn't activate pnpm@${pnpm_pin:-<unknown>} and no working pnpm on PATH. Install Node >=16.9 (bundles corepack) or 'npm install -g pnpm'.")
  PROBLEMS=$((PROBLEMS + 1))
}

check_cargo
check_node
check_pnpm

echo
info "Dependency check:"
printf '%s\n' "${REPORT[@]}"
echo

if [ "$PROBLEMS" -gt 0 ]; then
  die "Fix the dependency issues above, then re-run."
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

if [ "$NO_UI" -eq 0 ]; then
  info "Building UI (ui/)..."
  (
    # cd into ui/ rather than `pnpm --dir ui`: pnpm --dir still walks up the
    # directory tree for package-manager detection, and an ancestor
    # directory's own package.json/packageManager field (e.g. $HOME) can
    # make it pick the wrong package manager. Being inside ui/ avoids that.
    cd "$SCRIPT_DIR/ui"
    if [ -f pnpm-lock.yaml ]; then
      pnpm install --frozen-lockfile
    else
      warn "No pnpm-lock.yaml in ui/ — falling back to plain 'pnpm install'."
      pnpm install
    fi
    pnpm build
  )
else
  info "Skipping UI build (--no-ui)."
fi

if [ "$NO_UI" -eq 0 ]; then
  info "Building flproxy (release, UI embedded)..."
  (cd "$SCRIPT_DIR" && cargo build --release --features embed-ui -p flproxy-cli)
else
  info "Building flproxy (release, no embedded UI)..."
  (cd "$SCRIPT_DIR" && cargo build --release -p flproxy-cli)
fi

BUILT_BIN="$SCRIPT_DIR/target/release/flproxy"
[ -f "$BUILT_BIN" ] || die "Build finished but $BUILT_BIN is missing."

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------

if ! mkdir -p "$PREFIX" 2>/dev/null; then
  die "Can't create $PREFIX (permission denied?). Retry with --prefix DIR somewhere writable, or fix permissions yourself — this script never invokes sudo."
fi

DEST="$PREFIX/flproxy"
if [ -e "$DEST" ]; then
  warn "Overwriting existing $DEST"
fi

if ! cp "$BUILT_BIN" "$DEST"; then
  die "Failed to copy binary to $DEST (permission denied?). Retry with --prefix DIR somewhere writable."
fi
chmod +x "$DEST"
info "Installed $DEST"

path_has_prefix() {
  case ":$PATH:" in
    *":$PREFIX:"*) return 0 ;;
    *) return 1 ;;
  esac
}

guess_rc_file() {
  case "${SHELL:-}" in
    */zsh) echo "$HOME/.zshrc" ;;
    */bash)
      case "$OS" in
        Darwin) echo "$HOME/.bash_profile" ;;
        *) echo "$HOME/.bashrc" ;;
      esac
      ;;
    *) echo "your shell's rc file" ;;
  esac
}

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo
info "Installed: $DEST"
if VERSION_OUTPUT="$("$DEST" --version 2>&1)"; then
  info "Version: $VERSION_OUTPUT"
fi

echo
echo "Defaults: proxy http://127.0.0.1:9080, UI http://127.0.0.1:9081"
echo "(override with -p/--proxy-port, -u/--ui-port, -b/--bind on 'flproxy'/'flproxy run')"
echo
echo "Next steps:"
echo
echo "  flproxy"
echo "  flproxy cert install"
echo "  flproxy proxy on"
echo
echo "HTTPS capture will not work until the CA is trusted. 'cert install' is"
echo "best-effort and prints manual per-OS steps if it can't do it automatically."

if ! path_has_prefix; then
  echo
  warn "$PREFIX is not on your PATH. Add this to $(guess_rc_file):"
  echo
  echo "  export PATH=\"$PREFIX:\$PATH\""
fi
