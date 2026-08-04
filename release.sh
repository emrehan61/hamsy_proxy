#!/usr/bin/env bash
# release.sh — cut a hamsy-proxy release: bump the version in Cargo.toml,
# commit, tag, and push so the release workflow (tag push `v*`) builds and
# publishes the binaries.
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
Usage: release.sh (-b|-m|-s) [OPTIONS]

Cuts a hamsy-proxy release: bumps the version in the workspace Cargo.toml,
commits, tags, and pushes — a tag push matching `v*` triggers the GitHub
release workflow, which builds and publishes the binaries.

Exactly one bump flag is required:
  -b              Bump major: X.Y.Z -> (X+1).0.0
  -m              Bump minor: X.Y.Z -> X.(Y+1).0
  -s              Bump patch: X.Y.Z -> X.Y.(Z+1)

Options:
  --dry-run, -n   Print what would happen (old -> new version, commit
                  message, tag, push command) and change nothing. Precondition
                  failures below (dirty tree, wrong branch, behind origin,
                  tag already exists) are reported as warnings instead of
                  aborting, so --dry-run is safe to run from any branch or
                  repo state to preview a release.
  --no-push       Commit and tag locally but skip the push to origin.
  --yes, -y       Skip the final confirmation prompt.
  --help, -h      Show this help and exit.

This repo releases from master only: a clean checkout, on master, not
behind origin/master.
EOF
}

BUMP=""
DRY_RUN=0
NO_PUSH=0
YES=0

while [ $# -gt 0 ]; do
  case "$1" in
    -b)
      [ -z "$BUMP" ] || die "Only one bump flag (-b/-m/-s) may be given."
      BUMP="major"
      shift
      ;;
    -m)
      [ -z "$BUMP" ] || die "Only one bump flag (-b/-m/-s) may be given."
      BUMP="minor"
      shift
      ;;
    -s)
      [ -z "$BUMP" ] || die "Only one bump flag (-b/-m/-s) may be given."
      BUMP="patch"
      shift
      ;;
    --dry-run | -n)
      DRY_RUN=1
      shift
      ;;
    --no-push)
      NO_PUSH=1
      shift
      ;;
    --yes | -y)
      YES=1
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

if [ -z "$BUMP" ]; then
  usage >&2
  die "Exactly one bump flag is required: -b (major), -m (minor), or -s (patch)."
fi

# ---------------------------------------------------------------------------
# Precondition/validation failures die normally; under --dry-run they're
# reported as warnings instead, so --dry-run can be exercised from a dirty
# tree or a non-master branch (see --dry-run in `usage` above) rather than
# only ever working on an already-releasable master.
# ---------------------------------------------------------------------------

soft_die() {
  if [ "$DRY_RUN" -eq 1 ]; then
    warn "$1"
  else
    die "$1"
  fi
}

# ---------------------------------------------------------------------------
# Script location — every path/git command below is relative to the repo
# checkout this script lives in, not to wherever the caller's cwd is.
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

CARGO_TOML="Cargo.toml"
[ -f "$CARGO_TOML" ] || die "$SCRIPT_DIR/Cargo.toml not found — release.sh must live at the repo root."

# ---------------------------------------------------------------------------
# Preconditions
# ---------------------------------------------------------------------------

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  die "Not inside a git repository."
fi

if [ -n "$(git status --porcelain)" ]; then
  soft_die "Working tree is not clean. Commit or stash changes before releasing."
fi

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "$CURRENT_BRANCH" != "master" ]; then
  soft_die "On branch '$CURRENT_BRANCH', not 'master' — this repo releases from master only. Switch to master before releasing."
fi

info "Fetching origin/master..."
if git fetch origin master; then
  # Compares the local master ref against origin/master regardless of which
  # branch is currently checked out, so this still works (for --dry-run
  # testing) when run from a branch other than master.
  if BEHIND_COUNT="$(git rev-list --count master..origin/master 2>/dev/null)"; then
    if [ "$BEHIND_COUNT" -gt 0 ]; then
      soft_die "Local master is $BEHIND_COUNT commit(s) behind origin/master. Pull/rebase before releasing."
    fi
  else
    soft_die "Could not compare local master with origin/master (no local 'master' ref?)."
  fi
else
  soft_die "git fetch origin master failed. Check your network/remote."
fi

# ---------------------------------------------------------------------------
# Current version — [workspace.package] in Cargo.toml is the single source
# of truth compiled into the binary and compared by `hamsy update`.
# ---------------------------------------------------------------------------

# Restricted to the [workspace.package] block (stops at the next `[section]`)
# so this can never pick up an unrelated `version = "..."` line, such as one
# of the inline `name = { version = "...", ... }` dependency pins further
# down the same file.
WORKSPACE_PACKAGE_BLOCK="$(awk '/^\[workspace\.package\]/{flag=1; next} /^\[/{flag=0} flag' "$CARGO_TOML")"
CURRENT_VERSION="$(printf '%s\n' "$WORKSPACE_PACKAGE_BLOCK" | grep -E '^version[[:space:]]*=' | head -n1 | sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/')"

# Parsed defensively per-component (rather than one regex, to keep this
# bash-3.2-friendly like the rest of the repo's scripts) so a malformed or
# missing version line dies here instead of producing a bad tag/commit later.
CURRENT_MAJOR="${CURRENT_VERSION%%.*}"
CURRENT_REST="${CURRENT_VERSION#*.}"
CURRENT_MINOR="${CURRENT_REST%%.*}"
CURRENT_PATCH="${CURRENT_REST#*.}"

case "$CURRENT_MAJOR" in '' | *[!0-9]*) CURRENT_MAJOR="" ;; esac
case "$CURRENT_MINOR" in '' | *[!0-9]*) CURRENT_MINOR="" ;; esac
case "$CURRENT_PATCH" in '' | *[!0-9]*) CURRENT_PATCH="" ;; esac

if [ -z "$CURRENT_MAJOR" ] || [ -z "$CURRENT_MINOR" ] || [ -z "$CURRENT_PATCH" ]; then
  die "Could not parse a X.Y.Z version out of [workspace.package] in $CARGO_TOML (got '$CURRENT_VERSION')."
fi

# ---------------------------------------------------------------------------
# New version
# ---------------------------------------------------------------------------

# 10#$var forces base-10 interpretation so a component with a leading zero
# (e.g. "08") never gets misread as an invalid octal literal.
case "$BUMP" in
  major)
    NEW_MAJOR=$((10#$CURRENT_MAJOR + 1))
    NEW_MINOR=0
    NEW_PATCH=0
    ;;
  minor)
    NEW_MAJOR=$((10#$CURRENT_MAJOR))
    NEW_MINOR=$((10#$CURRENT_MINOR + 1))
    NEW_PATCH=0
    ;;
  patch)
    NEW_MAJOR=$((10#$CURRENT_MAJOR))
    NEW_MINOR=$((10#$CURRENT_MINOR))
    NEW_PATCH=$((10#$CURRENT_PATCH + 1))
    ;;
esac
NEW_VERSION="$NEW_MAJOR.$NEW_MINOR.$NEW_PATCH"
NEW_TAG="v$NEW_VERSION"

if git rev-parse -q --verify "refs/tags/$NEW_TAG" >/dev/null 2>&1; then
  soft_die "Tag $NEW_TAG already exists locally."
fi

if git ls-remote --exit-code --tags origin "$NEW_TAG" >/dev/null 2>&1; then
  soft_die "Tag $NEW_TAG already exists on origin."
fi

# ---------------------------------------------------------------------------
# Plan / confirmation
# ---------------------------------------------------------------------------

echo
echo "  v$CURRENT_VERSION -> v$NEW_VERSION"
echo
echo "Steps:"
echo "  1. Edit $CARGO_TOML: [workspace.package] version = \"$NEW_VERSION\""
echo "  2. cargo update --workspace   (refresh Cargo.lock)"
echo "  3. git add Cargo.toml Cargo.lock"
echo "  4. git commit -m \"Release v$NEW_VERSION\""
echo "  5. git tag -a $NEW_TAG -m \"$NEW_TAG\""
if [ "$NO_PUSH" -eq 1 ]; then
  echo "  6. (skipped: --no-push) git push origin master $NEW_TAG"
else
  echo "  6. git push origin master $NEW_TAG"
fi
echo

if [ "$DRY_RUN" -eq 1 ]; then
  info "Dry run — no changes made."
  exit 0
fi

if [ "$YES" -ne 1 ]; then
  printf 'Proceed with this release? [y/N] '
  reply=""
  read -r reply || true
  case "$reply" in
    y | Y | yes | YES) ;;
    *)
      info "Aborted — no changes made."
      exit 0
      ;;
  esac
fi

# ---------------------------------------------------------------------------
# Edit Cargo.toml
# ---------------------------------------------------------------------------

info "Updating $CARGO_TOML to $NEW_VERSION..."

# -i.bak (no space before the suffix) is the one *-i* spelling accepted
# identically by both BSD/macOS sed and GNU sed; the address range restricts
# the substitution to the [workspace.package] block for the same reason as
# the awk extraction above, so a dependency pin's `version = "..."` can
# never be touched.
sed -i.bak -E "/^\[workspace\.package\]/,/^\[/ s/^version[[:space:]]*=.*/version = \"$NEW_VERSION\"/" "$CARGO_TOML"
rm -f "${CARGO_TOML}.bak"

VERIFY_BLOCK="$(awk '/^\[workspace\.package\]/{flag=1; next} /^\[/{flag=0} flag' "$CARGO_TOML")"
VERIFY_VERSION="$(printf '%s\n' "$VERIFY_BLOCK" | grep -E '^version[[:space:]]*=' | head -n1 | sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/')"
[ "$VERIFY_VERSION" = "$NEW_VERSION" ] || die "Failed to update $CARGO_TOML (found version '$VERIFY_VERSION', expected '$NEW_VERSION'). No commit was made — check $CARGO_TOML by hand."

# ---------------------------------------------------------------------------
# Refresh Cargo.lock
# ---------------------------------------------------------------------------

have cargo || die "cargo not found. Install Rust: https://rustup.rs"

info "Refreshing Cargo.lock..."
if ! { cargo update --workspace --offline 2>/dev/null || cargo update --workspace; }; then
  die "cargo update --workspace failed. $CARGO_TOML was already edited to $NEW_VERSION — revert with: git checkout -- $CARGO_TOML"
fi

# ---------------------------------------------------------------------------
# Commit, tag, push
# ---------------------------------------------------------------------------

info "Committing Release v$NEW_VERSION..."
git add Cargo.toml Cargo.lock
git commit -m "Release v$NEW_VERSION" || die "git commit failed. $CARGO_TOML/Cargo.lock changes are staged but not committed — fix the issue and re-run."

if ! git tag -a "$NEW_TAG" -m "$NEW_TAG"; then
  warn "git tag failed after the release commit was already created."
  echo
  echo "Recovery — undo the local commit:"
  echo "  git reset --hard HEAD~1"
  die "Aborting before push."
fi

if [ "$NO_PUSH" -eq 1 ]; then
  info "Local commit and tag created. Skipped push (--no-push). Push manually with:"
  echo "  git push origin master $NEW_TAG"
else
  info "Pushing master and $NEW_TAG to origin..."
  if ! git push origin master "$NEW_TAG"; then
    warn "git push failed."
    echo
    echo "Recovery — undo the local commit and tag:"
    echo "  git tag -d $NEW_TAG"
    echo "  git reset --hard HEAD~1"
    die "Aborting after push failure. Fix the issue above (or run the recovery commands), then re-run this script."
  fi
  info "Pushed. GitHub release workflow is now building v$NEW_VERSION:"
  echo "  https://github.com/emrehan61/hamsy_proxy/actions"
fi
