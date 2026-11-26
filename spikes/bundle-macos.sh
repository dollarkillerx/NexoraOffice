#!/bin/sh
# Wraps a built spike binary in a minimal .app bundle so macOS treats it as a
# regular application: it can be brought to the front, gets the Aster icon in the
# Dock and its own bundle identifier, and can be addressed by automation.
# Usage: spikes/bundle-macos.sh <binary> [debug|release] [documents]
#
# With the third argument `documents` the Info.plist also declares the file
# types the application opens (packaging/macos-document-types.plist), so that
# Finder, "Open With" and the Dock icon can hand files to it. Use it for
# aster-shell only: the other spike binaries cannot open documents, and
# declaring types for them would make macOS offer them for files they refuse.
# The types are all `Alternate`: the bundle never becomes a default application.
set -eu
name="${1:?usage: spikes/bundle-macos.sh <binary> [debug|release] [documents]}"
profile="${2:-debug}"
documents="${3:-}"
case "$documents" in
    ""|documents) ;;
    *) echo "unknown option '$documents' (expected: documents)" >&2; exit 2 ;;
esac
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="$root/target/$profile/$name"
app="$root/target/$profile/$name.app"
[ -x "$bin" ] || { echo "build first: cargo build -p $name" >&2; exit 1; }
id="dev.aster.spike.$(printf '%s' "$name" | tr -cd 'a-zA-Z0-9')"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/$name"
cp "$root/assets/brand/logo/aster.icns" "$app/Contents/Resources/aster.icns"
{
cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>$name</string>
    <key>CFBundleIdentifier</key><string>$id</string>
    <key>CFBundleIconFile</key><string>aster</string>
    <key>CFBundleName</key><string>$name</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.0.0</string>
    <key>NSHighResolutionCapable</key><true/>
PLIST
if [ "$documents" = documents ]; then
    cat "$root/packaging/macos-document-types.plist"
fi
printf '</dict>\n</plist>\n'
} > "$app/Contents/Info.plist"
# Finder and the Dock cache icons by path; touching the bundle makes them look again.
touch "$app"
echo "$app"
