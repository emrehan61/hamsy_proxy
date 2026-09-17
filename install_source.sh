#!/usr/bin/env bash
# install_source.sh — despite the name, this installs the PREBUILT hamsy
# binary from a GitHub Release. No Rust or Node toolchain needed and
# nothing is built locally: download a release tarball, verify its
# checksum, put `hamsy` on your PATH. Use install.sh instead to build from
# source.
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
# Usage / flags
# ---------------------------------------------------------------------------

usage() {
  cat <<'EOF'
Usage: install_source.sh [OPTIONS]

Installs the PREBUILT hamsy binary from a GitHub Release — no Rust or Node
toolchain needed, nothing built locally. Downloads a release tarball for
your platform, verifies its checksum, and puts `hamsy` on your PATH.

Options:
  --prefix DIR      Install directory (default: $HOME/.local/bin)
  --yes, -y         Assume yes to prompts (cert trust, PATH setup)
  --no-cert         Skip trusting the CA in the OS trust store
  --no-desktop      Skip HAR file desktop integration
  --no-path         Skip offering to add the install dir to your PATH
  --version vX.Y.Z  Install a specific release instead of the latest
  --help, -h        Show this help and exit

There is no --uninstall here — use install.sh --uninstall to remove an
installed binary (and optionally its data dir), regardless of which script
originally installed it.
EOF
}

PREFIX="$HOME/.local/bin"
YES=0
NO_CERT=0
NO_PATH=0
NO_DESKTOP=0
VERSION=""

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
    --version)
      [ $# -ge 2 ] || die "--version requires an argument"
      VERSION="$2"
      shift 2
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
# Platform check — map uname to the exact Rust target triple used to name
# release assets in .github/workflows/release.yml.
# ---------------------------------------------------------------------------

SUPPORTED_PLATFORMS="Darwin/arm64, Darwin/x86_64, Linux/x86_64 (or amd64), Linux/arm64 (or aarch64)"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64) TRIPLE="aarch64-apple-darwin" ;;
      x86_64) TRIPLE="x86_64-apple-darwin" ;;
      *) die "Unsupported architecture '$ARCH' on Darwin. Supported platforms: $SUPPORTED_PLATFORMS" ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64 | amd64) TRIPLE="x86_64-unknown-linux-gnu" ;;
      arm64 | aarch64) TRIPLE="aarch64-unknown-linux-gnu" ;;
      *) die "Unsupported architecture '$ARCH' on Linux. Supported platforms: $SUPPORTED_PLATFORMS" ;;
    esac
    ;;
  *)
    die "Unsupported OS '$OS'. Supported platforms: $SUPPORTED_PLATFORMS"
    ;;
esac

# ---------------------------------------------------------------------------
# Dependency preflight
# ---------------------------------------------------------------------------

have curl || die "curl not found. Install curl (e.g. 'brew install curl' on macOS, or your distro's package manager), then re-run."
have tar || die "tar not found. Install tar (usually preinstalled; check your distro's package manager), then re-run."

# ---------------------------------------------------------------------------
# Download — releases/latest/download/... for the newest release, or
# releases/download/<tag>/... when --version pins a specific one.
# ---------------------------------------------------------------------------

if [ -n "$VERSION" ]; then
  RELEASE_PATH="download/$VERSION"
  info "Installing hamsy $VERSION for $TRIPLE..."
else
  RELEASE_PATH="latest/download"
  info "Installing latest hamsy release for $TRIPLE..."
fi

BASE_URL="https://github.com/emrehan61/hamsy_proxy/releases/$RELEASE_PATH"
TARBALL_NAME="hamsy-$TRIPLE.tar.gz"
TARBALL_URL="$BASE_URL/$TARBALL_NAME"
SUMS_URL="$BASE_URL/sha256sums.txt"

DOWNLOAD_DIR="$(mktemp -d)"
trap 'rm -rf "$DOWNLOAD_DIR"' EXIT

# $1 = url, $2 = destination file, $3 = human label for error messages.
# Separate from a plain `curl -fL ... || die` because a 404 (release still
# building) deserves a different message than a generic network failure —
# `-w '%{http_code}'` still reports the status even when `-f` makes curl
# itself exit non-zero on that same request.
fetch() {
  code="$(curl -fL --proto '=https' -sS -o "$2" -w '%{http_code}' "$1" 2>"$DOWNLOAD_DIR/curl-err.log")" && return 0
  if [ "$code" = "404" ]; then
    die "No release found for $3 (HTTP 404) — the release may still be building. Check https://github.com/emrehan61/hamsy_proxy/releases"
  fi
  cat "$DOWNLOAD_DIR/curl-err.log" >&2
  die "Failed to download $3 (HTTP ${code:-unknown})."
}

TARBALL="$DOWNLOAD_DIR/$TARBALL_NAME"
SUMS="$DOWNLOAD_DIR/sha256sums.txt"

info "Downloading $TARBALL_NAME..."
fetch "$TARBALL_URL" "$TARBALL" "$TARBALL_NAME"
fetch "$SUMS_URL" "$SUMS" "sha256sums.txt"

# ---------------------------------------------------------------------------
# Checksum verification — defense-in-depth, not a hard gate: if neither
# hashing tool is available, warn and continue rather than blocking install.
# ---------------------------------------------------------------------------

SUMS_LINE="$(grep -F "$TARBALL_NAME" "$SUMS" | head -n 1)"
[ -n "$SUMS_LINE" ] || die "sha256sums.txt has no entry for $TARBALL_NAME — can't verify the download."
EXPECTED_SHA="$(printf '%s\n' "$SUMS_LINE" | awk '{print $1}')"

ACTUAL_SHA=""
if have sha256sum; then
  ACTUAL_SHA="$(sha256sum "$TARBALL" | awk '{print $1}')"
elif have shasum; then
  ACTUAL_SHA="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
fi

if [ -z "$ACTUAL_SHA" ]; then
  warn "Neither sha256sum nor shasum found — skipping checksum verification."
else
  if [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
    die "Checksum mismatch for $TARBALL_NAME: expected $EXPECTED_SHA, got $ACTUAL_SHA. Download may be corrupted — try again."
  fi
  info "Checksum verified."
fi

# ---------------------------------------------------------------------------
# Extract + install
# ---------------------------------------------------------------------------

tar xzf "$TARBALL" -C "$DOWNLOAD_DIR" hamsy || die "Failed to extract hamsy from $TARBALL_NAME."
if [ "$NO_DESKTOP" -eq 0 ] && tar tzf "$TARBALL" | grep '^packaging/install-desktop.sh$' >/dev/null; then
  tar xzf "$TARBALL" -C "$DOWNLOAD_DIR" packaging || die "Failed to extract desktop integration."
fi
[ -f "$DOWNLOAD_DIR/hamsy" ] || die "Extracted $TARBALL_NAME but the hamsy binary is missing from it."

if ! mkdir -p "$PREFIX" 2>/dev/null; then
  die "Can't create $PREFIX (permission denied?). Retry with --prefix DIR somewhere writable, or fix permissions yourself — this script never invokes sudo."
fi

DEST="$PREFIX/hamsy"
if [ -e "$DEST" ]; then
  warn "Overwriting existing $DEST"
fi

if ! cp "$DOWNLOAD_DIR/hamsy" "$DEST"; then
  die "Failed to copy binary to $DEST (permission denied?). Retry with --prefix DIR somewhere writable."
fi
chmod +x "$DEST"
info "Installed $DEST"

if [ "$NO_DESKTOP" -eq 1 ]; then
  info "Skipping HAR desktop integration (--no-desktop)."
elif [ -f "$DOWNLOAD_DIR/packaging/install-desktop.sh" ]; then
  if ! bash "$DOWNLOAD_DIR/packaging/install-desktop.sh" --binary "$DEST"; then
    warn "HAR desktop integration failed; the command-line installation is available."
  fi
else
  info "This release does not include HAR desktop integration."
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
