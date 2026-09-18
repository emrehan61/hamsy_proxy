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
    legacy_app="$output_dir/macos/Hamsy.app"
    if [ -e "$legacy_app" ]; then
      if [ -d "$legacy_app" ] && [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$legacy_app/Contents/Info.plist" 2>/dev/null || true)" = io.hamsy.har-launcher ]; then
        rm -rf "$legacy_app"
      else
        echo "Refusing to replace unrecognized application: $legacy_app" >&2
        exit 1
      fi
    fi
    cp "$source_dir/macos/launcher.applescript" "$output_dir/macos/"
    ;;
  Linux)
    cp -R "$source_dir/linux" "$output_dir/"
    chmod +x "$output_dir/linux/launch"
    ;;
  *) echo "Desktop integration supports macOS and Linux." >&2; exit 1 ;;
esac
