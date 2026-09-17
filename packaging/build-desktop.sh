#!/usr/bin/env bash
# Build the portable desktop integration payload without installing anything.
# Usage: build-desktop.sh OUTPUT_DIRECTORY
set -euo pipefail
[ "$#" -eq 1 ] || { echo "Usage: $0 OUTPUT_DIRECTORY" >&2; exit 2; }
source_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$1"
output_dir="$(cd "$1" && pwd)"
[ "$output_dir" != "$source_dir" ] || { echo "Output must differ from source packaging directory" >&2; exit 2; }
cp "$source_dir/install-desktop.sh" "$output_dir/"
cp "$source_dir/README.md" "$output_dir/"
cp "$source_dir/../docs/images/logo.png" "$output_dir/hamsy.png"
chmod +x "$output_dir/install-desktop.sh"
case "$(uname -s)" in
  Darwin)
    mkdir -p "$output_dir/macos"
    final_app="$output_dir/macos/Hamsy.app"
    if [ -e "$final_app" ] && [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$final_app/Contents/Info.plist" 2>/dev/null || true)" != io.hamsy.har-launcher ]; then
      echo "Refusing to replace unrecognized application: $final_app" >&2
      exit 1
    fi
    build_dir="$(mktemp -d "$output_dir/macos/.hamsy-build.XXXXXX")"
    trap 'rm -rf "$build_dir"' EXIT
    app="$build_dir/Hamsy.app"
    /usr/bin/osacompile -o "$app" "$source_dir/macos/launcher.applescript"
    plist="$app/Contents/Info.plist"
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
      /usr/bin/sips -z "$size" "$size" "$output_dir/hamsy.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
      double=$((size * 2))
      /usr/bin/sips -z "$double" "$double" "$output_dir/hamsy.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    /usr/bin/iconutil -c icns "$iconset" -o "$app/Contents/Resources/Hamsy.icns"
    rm -r "$iconset"
    # The compiler's original signature is invalid after adding metadata/icon.
    # This is local ad-hoc signing, not Developer ID signing or notarization.
    /usr/bin/codesign --force --sign - "$app"
    if [ -e "$final_app" ]; then rm -rf "$final_app"; fi
    mv "$app" "$final_app"
    rm -rf "$build_dir"
    trap - EXIT
    ;;
  Linux)
    cp -R "$source_dir/linux" "$output_dir/"
    chmod +x "$output_dir/linux/launch"
    ;;
  *) echo "Desktop integration supports macOS and Linux." >&2; exit 1 ;;
esac
