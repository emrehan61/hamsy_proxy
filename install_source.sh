#!/usr/bin/env bash
# Compatibility entry point for callers that used install_source.sh before
# install.sh became the canonical prebuilt installer.
set -euo pipefail

source_path="${BASH_SOURCE[0]:-}"
case "$source_path" in
  '' | bash | - | /dev/stdin | /dev/fd/*) source_path="" ;;
  *) source_path="$(cd "$(dirname "$source_path")" && pwd)/install.sh" ;;
esac

if [ -n "$source_path" ] && [ -f "$source_path" ]; then
  exec bash "$source_path" "$@"
fi

# Preserve streamed/standalone use by fetching the canonical script itself.
command -v curl >/dev/null 2>&1 || {
  printf '%s\n' 'error: install_source.sh needs curl when run without install.sh beside it.' >&2
  exit 1
}
tmp_script="$(mktemp)"
trap 'rm -f "$tmp_script"' EXIT
installer_url="${HAMSY_INSTALLER_URL:-https://raw.githubusercontent.com/emrehan61/hamsy_proxy/master/install.sh}"
curl -fL --proto '=https' -sS "$installer_url" -o "$tmp_script" || {
  printf 'error: failed to download canonical installer from %s\n' "$installer_url" >&2
  exit 1
}
run_rc=0
bash "$tmp_script" "$@" || run_rc=$?
rm -f "$tmp_script"
trap - EXIT
exit "$run_rc"
