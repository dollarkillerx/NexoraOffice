#!/bin/sh
# Wraps the built binary in a minimal .app bundle so macOS treats it as a regular
# application (own Dock icon, bundle identifier, addressable by automation).
# Usage: spikes/g1-ime/bundle-macos.sh [debug|release]
set -eu
profile="${1:-debug}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
bin="$root/target/$profile/g1-ime"
app="$root/target/$profile/G1Ime.app"
[ -x "$bin" ] || { echo "build first: cargo build -p g1-ime" >&2; exit 1; }
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/g1-ime"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>g1-ime</string>
    <key>CFBundleIdentifier</key><string>dev.aster.spike.g1ime</string>
    <key>CFBundleName</key><string>G1Ime</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.0.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
echo "$app"
