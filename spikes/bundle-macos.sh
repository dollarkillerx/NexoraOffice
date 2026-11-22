#!/bin/sh
# Wraps a built spike binary in a minimal .app bundle so macOS treats it as a
# regular application: it can be brought to the front, gets its own Dock icon and
# bundle identifier, and can be addressed by automation.
# Usage: spikes/bundle-macos.sh <binary> [debug|release]
set -eu
name="${1:?usage: spikes/bundle-macos.sh <binary> [debug|release]}"
profile="${2:-debug}"
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="$root/target/$profile/$name"
app="$root/target/$profile/$name.app"
[ -x "$bin" ] || { echo "build first: cargo build -p $name" >&2; exit 1; }
id="dev.aster.spike.$(printf '%s' "$name" | tr -cd 'a-zA-Z0-9')"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/$name"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>$name</string>
    <key>CFBundleIdentifier</key><string>$id</string>
    <key>CFBundleName</key><string>$name</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.0.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
echo "$app"
