#!/usr/bin/env bash
# Register HAR file opening for this user. Never changes the default handler.
set -euo pipefail
payload_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
binary_path=""
uninstall=0
usage() {
  echo "Usage: $0 --binary /path/to/hamsy"
  echo "       $0 --uninstall [--binary /path/to/hamsy]"
}
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --binary) [ "$#" -ge 2 ] || die '--binary needs a path'; binary_path="$2"; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

platform="$(uname -s)"
case "$platform" in
  Darwin)
    state_dir="$HOME/Library/Application Support/Hamsy/desktop"
    app_path="$HOME/Applications/Hamsy.app"
    ;;
  Linux)
    data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
    case "$data_dir" in /*) ;; *) die 'XDG_DATA_HOME must be an absolute path' ;; esac
    state_dir="$data_dir/hamsy/desktop"
    app_path="$data_dir/applications/io.hamsy.har.desktop"
    ;;
  *) die "Unsupported platform: $platform" ;;
esac

# These files use a single line for a path. Reject line breaks instead of
# silently producing a broken launcher. Spaces and shell metacharacters work.
case "$binary_path$state_dir" in
  *$'\n'*|*$'\r'*) die 'Installation paths must not contain line breaks' ;;
esac
if [ -n "$binary_path" ]; then
  binary_parent="$(cd "$(dirname "$binary_path")" && pwd)" || die 'Binary directory does not exist'
  binary_path="$binary_parent/$(basename "$binary_path")"
fi
marker="$state_dir/owner"
owned=0
if [ -f "$marker" ] && [ "$(cat "$marker")" = 'io.hamsy.har-launcher' ]; then owned=1; fi

refresh_linux() {
  [ "${HAMSY_DESKTOP_SKIP_REGISTER:-0}" != 1 ] || return 0
  if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database "$data_dir/mime" || echo 'Could not refresh MIME database; log out and in if needed.' >&2
  fi
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data_dir/applications" || echo 'Could not refresh desktop database.' >&2
  fi
}

if [ "$uninstall" -eq 1 ]; then
  if [ "$owned" -ne 1 ]; then echo 'No Hamsy desktop integration owned by this installer.'; exit 0; fi
  if [ -n "$binary_path" ] && [ "$(cat "$state_dir/binary-path")" != "$binary_path" ]; then
    echo 'Desktop integration belongs to another Hamsy installation; keeping it.'
    exit 0
  fi
  case "$platform" in
    Darwin)
      if [ -d "$app_path" ] && [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app_path/Contents/Info.plist" 2>/dev/null || true)" = io.hamsy.har-launcher ]; then
        if [ "${HAMSY_DESKTOP_SKIP_REGISTER:-0}" != 1 ]; then
          /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$app_path" || true
        fi
        rm -rf "$app_path"
      fi
      ;;
    Linux)
      if [ -f "$app_path" ] && grep -q '^X-Hamsy-Owner=io.hamsy.har-launcher$' "$app_path"; then rm -f "$app_path"; fi
      rm -f "$data_dir/mime/packages/io.hamsy.har.xml" "$data_dir/icons/hicolor/512x512/apps/io.hamsy.har.png"
      refresh_linux
      ;;
  esac
  rm -rf "$state_dir"
  echo 'Removed Hamsy HAR desktop integration.'
  exit 0
fi

[ -n "$binary_path" ] || die '--binary is required'
[ -x "$binary_path" ] || die "Not an executable: $binary_path"
if [ -e "$state_dir" ] && [ "$owned" -ne 1 ]; then die "Refusing to replace unrecognized directory: $state_dir"; fi
if [ -e "$app_path" ]; then
  [ "$owned" -eq 1 ] || die "Refusing to overwrite existing $app_path"
  case "$platform" in
    Darwin)
      [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app_path/Contents/Info.plist" 2>/dev/null || true)" = io.hamsy.har-launcher ] || die "Unrecognized application at $app_path"
      ;;
    Linux)
      grep -q '^X-Hamsy-Owner=io.hamsy.har-launcher$' "$app_path" || die "Unrecognized launcher at $app_path"
      ;;
  esac
fi
case "$platform" in
  Darwin) [ -d "$payload_dir/macos/Hamsy.app" ] || die 'macOS launcher missing; run packaging/build-desktop.sh first' ;;
  Linux) [ -f "$payload_dir/linux/launch" ] || die 'Linux launcher missing from package' ;;
esac

mkdir -p "$state_dir"
printf '%s\n' io.hamsy.har-launcher > "$marker"
printf '%s\n' "$binary_path" > "$state_dir/binary-path"
# Retain installer/resources for later refresh or uninstall after archive removal.
if [ "$payload_dir" != "$state_dir/integration" ]; then
  mkdir -p "$state_dir/integration"
  cp "$payload_dir/install-desktop.sh" "$payload_dir/README.md" "$payload_dir/hamsy.png" "$state_dir/integration/"
  case "$platform" in
    Darwin) rm -rf "$state_dir/integration/macos"; cp -R "$payload_dir/macos" "$state_dir/integration/" ;;
    Linux) cp -R "$payload_dir/linux" "$state_dir/integration/" ;;
  esac
fi
case "$platform" in
  Darwin)
    mkdir -p "$HOME/Applications"
    if [ -e "$app_path" ]; then rm -rf "$app_path"; fi
    cp -R "$payload_dir/macos/Hamsy.app" "$app_path"
    if [ "${HAMSY_DESKTOP_SKIP_REGISTER:-0}" != 1 ]; then
      /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app_path" || true
    fi
    echo "Installed $app_path. Select Hamsy in a HAR file's Get Info > Open with."
    ;;
  Linux)
    mkdir -p "$data_dir/applications" "$data_dir/mime/packages" "$data_dir/icons/hicolor/512x512/apps"
    cp "$payload_dir/linux/launch" "$state_dir/launch"
    chmod +x "$state_dir/launch"
    cp "$payload_dir/linux/hamsy-har.xml" "$data_dir/mime/packages/io.hamsy.har.xml"
    cp "$payload_dir/hamsy.png" "$data_dir/icons/hicolor/512x512/apps/io.hamsy.har.png"
    # Desktop Entry escaping is two layers: string escapes, then Exec quoting.
    exec_path="$state_dir/launch"
    exec_path="${exec_path//\\/\\\\\\\\}"
    exec_path="${exec_path//\"/\\\\\"}"
    exec_path="${exec_path//\$/\\\\\$}"
    exec_path="${exec_path//\`/\\\\\`}"
    exec_path="${exec_path//%/%%}"
    cat > "$app_path" <<EOF
[Desktop Entry]
Type=Application
Name=Hamsy HAR Viewer
Comment=Open HTTP Archive files in your browser
Exec="$exec_path" %F
Icon=io.hamsy.har
Terminal=false
NoDisplay=true
Categories=Development;Network;
MimeType=application/har+json;application/x-har;
X-Hamsy-Owner=io.hamsy.har-launcher
EOF
    refresh_linux
    echo "Installed $app_path. Choose Hamsy HAR Viewer using your file manager's Open With menu."
    ;;
esac
echo 'Existing default applications have been preserved.'
printf 'Integration helper retained at: %s\n' "$state_dir/integration/install-desktop.sh"
