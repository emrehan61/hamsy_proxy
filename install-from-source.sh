#!/usr/bin/env bash
# install-from-source.sh — build hamsy-proxy from this checkout and put it on
# your PATH. The normal install.sh entry point installs a prebuilt release;
# use that entry point with --from-source to select this workflow explicitly.
#
# This script builds from the checkout it's run from: UI first (Vite),
# then the Rust CLI with the UI embedded via rust-embed.
# Use install.sh (or install_source.sh for compatibility) to install a
# prebuilt release instead.
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
#
# Piped/`-c`/process-substitution invocations (curl | bash, bash -c "$(curl
# ...)", bash <(curl ...)) leave BASH_SOURCE[0] unset — under set -u a bare
# ${BASH_SOURCE[0]} is an unbound-variable error, so it's read via the safe
# default expansion below, then checked against the sentinel values those
# invocation styles are known to produce.
# ---------------------------------------------------------------------------

BOOTSTRAP_SRC="${BASH_SOURCE[0]:-}"
case "$BOOTSTRAP_SRC" in
  '' | bash | - | /dev/stdin | /dev/fd/*) BOOTSTRAP_SRC="" ;;
esac

if [ -n "$BOOTSTRAP_SRC" ]; then
  # shellcheck disable=SC2164 # set -e already aborts here if cd fails; the
  # fallback error message would just be less friendly.
  SCRIPT_DIR="$(cd "$(dirname "$BOOTSTRAP_SRC")" && pwd)"
fi

# Bootstrap mode: either BASH_SOURCE told us nothing usable, or it pointed
# somewhere that isn't actually a hamsy-proxy checkout (e.g. a lone install.sh
# copied out standalone). Either way, clone the real repo and re-run from
# there instead of guessing at paths that don't exist.
if [ -z "$BOOTSTRAP_SRC" ] || [ ! -f "$SCRIPT_DIR/Cargo.toml" ]; then
  have git || die "git not found. Install git, then re-run — it's needed to fetch the hamsy-proxy source for this piped/standalone install."

  repo_url="${HAMSY_REPO_URL:-https://github.com/emrehan61/hamsy_proxy.git}"
  clone_dir="$(mktemp -d)"
  info "Fetching hamsy-proxy source into $clone_dir..."

  if ! git clone --depth 1 "$repo_url" "$clone_dir"; then
    rm -rf "$clone_dir"
    die "Failed to clone $repo_url. Check the URL/network, or override it with HAMSY_REPO_URL."
  fi

  if [ -n "${HAMSY_REPO_REF:-}" ]; then
    # Subshell so this doesn't change the running script's own cwd. A
    # --depth 1 fetch of the explicit ref, then checking out FETCH_HEAD,
    # works for branches, tags, and commit SHAs alike — unlike `git clone
    # --branch`, which only resolves refs known at clone time.
    if ! (cd "$clone_dir" && git fetch --depth 1 origin "$HAMSY_REPO_REF" && git checkout FETCH_HEAD); then
      rm -rf "$clone_dir"
      die "Failed to check out ref '$HAMSY_REPO_REF' from $repo_url."
    fi
  fi

  if [ ! -f "$clone_dir/install-from-source.sh" ]; then
    rm -rf "$clone_dir"
    die "Cloned $repo_url but install-from-source.sh is missing from it."
  fi

  status=0
  bash "$clone_dir/install-from-source.sh" "$@" || status=$?
  rm -rf "$clone_dir"
  exit "$status"
fi

# ---------------------------------------------------------------------------
# Usage / flags
# ---------------------------------------------------------------------------

usage() {
  cat <<'EOF'
Usage: install-from-source.sh [OPTIONS]

Builds hamsy-proxy from source (Rust + the SolidJS UI) and installs the
resulting binary onto your PATH. Always builds from this checkout —
there is nothing to download.

Options:
  --prefix DIR    Install directory (default: $HOME/.local/bin)
  --yes, -y       Assume yes to prompts (e.g. installing Rust via rustup)
  --skip-deps     Only check dependencies; never install anything
  --no-ui         Skip the UI build; build hamsy-proxy without --features embed-ui
  --no-cert       Skip trusting the CA in the OS trust store
  --no-desktop    Skip HAR file desktop integration
  --no-path       Skip offering to add the install dir to your PATH
  --uninstall     Remove the binary and desktop integration (optionally ~/.hamsy too)
  --help, -h      Show this help and exit

Run this script outside a hamsy-proxy checkout (e.g. piped via curl | bash)
and it clones the repo into a temp dir and re-runs itself there, forwarding
every flag above verbatim. Override the source with HAMSY_REPO_URL (default:
https://github.com/emrehan61/hamsy_proxy.git) and HAMSY_REPO_REF (branch,
tag, or commit — checked out after cloning).
EOF
}

PREFIX="$HOME/.local/bin"
YES=0
SKIP_DEPS=0
NO_UI=0
NO_CERT=0
NO_PATH=0
NO_DESKTOP=0
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
    --no-cert)
      NO_CERT=1
      shift
      ;;
    --no-desktop)
      NO_DESKTOP=1
      shift
      ;;
    --no-path)
      NO_PATH=1
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
  bin_path="$PREFIX/hamsy"
  # Guarded helper only removes the integration owned by this binary path.
  bash "$SCRIPT_DIR/packaging/install-desktop.sh" --uninstall --binary "$bin_path" || warn "Could not remove HAR desktop integration."

  # Offer to remove the CA from the OS trust store while the binary that
  # knows how to do that is still here. $YES -eq 0 gates even asking, so a
  # bare --yes uninstall never touches the trust store.
  if [ -x "$bin_path" ] && [ "$YES" -eq 0 ] && [ -t 0 ]; then
    printf 'Remove the hamsy CA from your OS trust store too (%s cert uninstall)? [y/N] ' "$bin_path"
    reply=""
    read -r reply || true
    case "$reply" in
      y | Y | yes | YES)
        if "$bin_path" cert uninstall; then
          info "Removed the hamsy CA from the OS trust store."
        else
          warn "'$bin_path cert uninstall' failed — remove it manually if needed."
        fi
        ;;
    esac
  fi

  if [ -e "$bin_path" ]; then
    rm -f "$bin_path"
    info "Removed $bin_path"
  else
    info "No binary found at $bin_path — nothing to remove."
  fi

  data_dir="${HAMSY_HOME:-$HOME/.hamsy}"

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
  info "Building hamsy-proxy (release, UI embedded)..."
  (cd "$SCRIPT_DIR" && cargo build --release --features embed-ui -p hamsy-cli)
else
  info "Building hamsy-proxy (release, no embedded UI)..."
  (cd "$SCRIPT_DIR" && cargo build --release -p hamsy-cli)
fi

BUILT_BIN="$SCRIPT_DIR/target/release/hamsy"
[ -f "$BUILT_BIN" ] || die "Build finished but $BUILT_BIN is missing."

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------

if ! mkdir -p "$PREFIX" 2>/dev/null; then
  die "Can't create $PREFIX (permission denied?). Retry with --prefix DIR somewhere writable, or fix permissions yourself — this script never invokes sudo."
fi

DEST="$PREFIX/hamsy"
if [ -e "$DEST" ]; then
  warn "Overwriting existing $DEST"
fi

if ! cp "$BUILT_BIN" "$DEST"; then
  die "Failed to copy binary to $DEST (permission denied?). Retry with --prefix DIR somewhere writable."
fi
chmod +x "$DEST"
info "Installed $DEST"

if [ "$NO_UI" -eq 1 ]; then
  info "Skipping HAR desktop integration (--no-ui: browser viewer is not embedded)."
elif [ "$NO_DESKTOP" -eq 1 ]; then
  info "Skipping HAR desktop integration (--no-desktop)."
else
  desktop_payload="$(mktemp -d)"
  if bash "$SCRIPT_DIR/packaging/build-desktop.sh" "$desktop_payload" &&
     bash "$desktop_payload/install-desktop.sh" --binary "$DEST"; then
    info "HAR desktop integration installed."
  else
    warn "HAR desktop integration failed; the command-line installation is available. Retry the installer or use --no-desktop."
  fi
  rm -rf "$desktop_payload"
fi

# ---------------------------------------------------------------------------
# Cert setup — best-effort; hamsy itself prints manual per-OS steps on
# failure, so a decline or a failure here is never fatal to the install.
# ---------------------------------------------------------------------------

CERT_OK=0
if [ "$NO_CERT" -eq 1 ]; then
  info "Skipping CA trust setup (--no-cert)."
else
  cert_consent=0
  if [ "$YES" -eq 1 ]; then
    cert_consent=1
  elif [ -t 0 ]; then
    # Default YES here, unlike the other prompts in this file: trusting the
    # CA is what makes HTTPS capture work at all, so opting in is the
    # expected path and a blank/garbage reply should mean "yes".
    printf 'Trust the hamsy CA in your OS trust store now? [Y/n] '
    reply=""
    read -r reply || true
    case "$reply" in
      n | N | no | NO) cert_consent=0 ;;
      *) cert_consent=1 ;;
    esac
  else
    warn "Non-interactive, no tty — skipping CA trust setup. Run 'hamsy cert install' later."
  fi

  if [ "$cert_consent" -eq 1 ]; then
    if "$DEST" cert install; then
      CERT_OK=1
    else
      warn "'hamsy cert install' failed — see its output above for manual per-OS steps, or re-run it later."
    fi
  fi
fi

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
# PATH setup — best-effort; the end-of-script PATH warning in the Summary
# below still fires unchanged if this is skipped, declined, or the rc file
# can't be determined.
# ---------------------------------------------------------------------------

PATH_APPENDED=0
if [ "$NO_PATH" -eq 0 ] && ! path_has_prefix; then
  rc_file="$(guess_rc_file)"
  case "$rc_file" in
    "$HOME"/*)
      # Only offer to auto-append when guess_rc_file() returned a real path
      # — it falls back to the literal string "your shell's rc file" when it
      # can't tell, and that's not something we can safely edit.
      path_consent=0
      if [ "$YES" -eq 1 ]; then
        path_consent=1
      elif [ -t 0 ]; then
        # Default YES, same inverted style as the cert prompt above.
        printf 'Add %s to your PATH by editing %s now? [Y/n] ' "$PREFIX" "$rc_file"
        reply=""
        read -r reply || true
        case "$reply" in
          n | N | no | NO) path_consent=0 ;;
          *) path_consent=1 ;;
        esac
      fi

      if [ "$path_consent" -eq 1 ]; then
        path_line="export PATH=\"$PREFIX:\$PATH\""
        if grep -qF "$path_line" "$rc_file" 2>/dev/null; then
          info "$rc_file already adds $PREFIX to your PATH."
        else
          printf '\n%s\n' "$path_line" >> "$rc_file"
          info "Added $PREFIX to your PATH in $rc_file. Restart your shell or run: source $rc_file"
        fi
        PATH_APPENDED=1
      fi
      ;;
  esac
fi

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
echo "(override with -p/--proxy-port, -u/--ui-port, -b/--bind on 'hamsy'/'hamsy run')"
echo
echo "Next steps:"
echo
echo "  hamsy"
if [ "$CERT_OK" -eq 0 ]; then
  echo "  hamsy cert install"
fi
echo
echo "Plain 'hamsy' captures traffic system-wide out of the box: it points"
echo "your OS proxy settings at 127.0.0.1:9080 and restores your previous"
echo "settings on shutdown. Prefer to configure clients yourself instead?"
echo "Run 'hamsy --manual' to leave your OS proxy settings untouched and"
echo "point individual apps/browsers at 127.0.0.1:9080 by hand."

if [ "$CERT_OK" -eq 0 ]; then
  echo
  echo "HTTPS capture will not work until the CA is trusted. 'cert install' is"
  echo "best-effort and prints manual per-OS steps if it can't do it automatically."
fi

if [ "$PATH_APPENDED" -eq 0 ] && ! path_has_prefix; then
  echo
  warn "$PREFIX is not on your PATH. Add this to $(guess_rc_file):"
  echo
  echo "  export PATH=\"$PREFIX:\$PATH\""
fi
