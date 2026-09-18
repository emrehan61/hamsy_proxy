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

# Build the macOS bundle on the user's machine. Release archives carry the
# AppleScript and image so the wrapper is generated with the user's stock
# macOS tools during installation.
build_macos_app() {
  [ -x /usr/bin/osacompile ] || die 'macOS launcher requires /usr/bin/osacompile'
  [ -x /usr/libexec/PlistBuddy ] || die 'macOS launcher requires /usr/libexec/PlistBuddy'
  [ -x /usr/bin/sips ] || die 'macOS launcher requires /usr/bin/sips'
  [ -x /usr/bin/iconutil ] || die 'macOS launcher requires /usr/bin/iconutil'
  [ -x /usr/bin/codesign ] || die 'macOS launcher requires /usr/bin/codesign'
  [ -f "$payload_dir/macos/launcher.applescript" ] || die 'macOS launcher source missing from package'
  [ -f "$payload_dir/hamsy.png" ] || die 'macOS launcher icon missing from package'

  build_dir="$(mktemp -d "${TMPDIR:-/tmp}/hamsy-desktop.XXXXXX")"
  cleanup_build() {
    rc=$?
    rm -rf "$build_dir"
    [ -z "${stage_parent:-}" ] || rm -rf "$stage_parent"
    [ -z "${backup_parent:-}" ] || rm -rf "$backup_parent"
    if [ "$rc" -ne 0 ]; then
      printf '%s\n' 'error: local macOS launcher generation failed; use hamsy open FILE.har from a terminal.' >&2
    fi
    exit "$rc"
  }
  trap cleanup_build EXIT
  macos_app="$build_dir/Hamsy.app"
  /usr/bin/osacompile -o "$macos_app" "$payload_dir/macos/launcher.applescript"
  plist="$macos_app/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c 'Add :CFBundleIdentifier string io.hamsy.har-launcher' "$plist"
  /usr/libexec/PlistBuddy -c 'Set :CFBundleName Hamsy' "$plist"
  /usr/libexec/PlistBuddy -c 'Set :CFBundleIconFile Hamsy' "$plist"
  /usr/libexec/PlistBuddy -c 'Delete :CFBundleIconName' "$plist" 2>/dev/null || true
  /usr/libexec/PlistBuddy -c 'Add :LSUIElement bool true' "$plist"
  /usr/libexec/PlistBuddy -c 'Delete :CFBundleDocumentTypes' "$plist" 2>/dev/null || true
  /usr/libexec/PlistBuddy -c 'Add :CFBundleDocumentTypes array' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :CFBundleDocumentTypes:0 dict' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :CFBundleDocumentTypes:0:CFBundleTypeName string HTTP Archive' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :CFBundleDocumentTypes:0:CFBundleTypeRole string Viewer' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :CFBundleDocumentTypes:0:LSHandlerRank string Alternate' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :CFBundleDocumentTypes:0:LSItemContentTypes array' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :CFBundleDocumentTypes:0:LSItemContentTypes:0 string io.hamsy.har' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations array' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0 dict' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0:UTTypeIdentifier string io.hamsy.har' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0:UTTypeDescription string HTTP Archive' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0:UTTypeConformsTo array' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0:UTTypeConformsTo:0 string public.json' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0:UTTypeTagSpecification dict' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension array' "$plist"
  /usr/libexec/PlistBuddy -c 'Add :UTImportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension:0 string har' "$plist"
  iconset="$build_dir/Hamsy.iconset"
  mkdir -p "$iconset"
  for size in 16 32 128 256 512; do
    /usr/bin/sips -z "$size" "$size" "$payload_dir/hamsy.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    /usr/bin/sips -z "$double" "$double" "$payload_dir/hamsy.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  /usr/bin/iconutil -c icns "$iconset" -o "$macos_app/Contents/Resources/Hamsy.icns"
  rm -rf "$iconset"
  # Local ad-hoc signing keeps LaunchServices metadata coherent. This does
  # not use a Developer ID, Apple account, or notarization service.
  /usr/bin/codesign --force --sign - "$macos_app"
}

replace_macos_app() {
  mkdir -p "$HOME/Applications"
  stage_parent="$(mktemp -d "$HOME/Applications/.hamsy-stage.XXXXXX")" || die 'Could not create a macOS launcher staging directory'
  stage_app="$stage_parent/Hamsy.app"
  cp -R "$macos_app" "$stage_app" || die 'Could not stage the macOS launcher bundle'

  backup_parent=""
  backup_app=""
  if [ -e "$app_path" ]; then
    backup_parent="$(mktemp -d "$HOME/Applications/.hamsy-backup.XXXXXX")" || die 'Could not create a macOS launcher backup directory'
    backup_app="$backup_parent/Hamsy.app"
    if ! mv "$app_path" "$backup_app"; then
      rm -rf "$backup_parent" "$stage_parent"
      die 'Could not stage the existing macOS launcher for replacement'
    fi
  fi

  if ! mv "$stage_app" "$app_path"; then
    restore_failed=0
    if [ -n "$backup_app" ] && [ -e "$backup_app" ]; then
      if ! mv "$backup_app" "$app_path"; then
        restore_failed=1
      fi
    fi
    rm -rf "$stage_parent"
    if [ "$restore_failed" -ne 0 ]; then
      preserved_backup="$backup_app"
      backup_parent=""
      die "Could not install the staged macOS launcher; the previous launcher was preserved at $preserved_backup"
    fi
    rm -rf "$backup_parent"
    die 'Could not install the staged macOS launcher; the previous launcher was restored'
  fi
  rm -rf "$backup_parent" "$stage_parent"
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
  Darwin) [ -f "$payload_dir/macos/launcher.applescript" ] || die 'macOS launcher source missing; run packaging/build-desktop.sh first' ;;
  Linux) [ -f "$payload_dir/linux/launch" ] || die 'Linux launcher missing from package' ;;
esac

# Complete the replacement bundle before touching an existing installation.
# A failed compiler/tool/icon/signing step therefore leaves the current app
# usable and gives the caller a direct CLI fallback.
if [ "$platform" = Darwin ]; then
  build_macos_app
fi

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
    replace_macos_app
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
# Keep the proxy CLI out of the app menu; the MIME association still exposes
# Hamsy in HAR file managers' Open With chooser.
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
