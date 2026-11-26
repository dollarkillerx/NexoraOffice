#!/bin/sh
# Builds "Aster Office.app" and a .dmg for the Mac this runs on.
#   packaging/macos.sh
# The bundle is signed ad hoc, which is enough to run it on the machine that
# built it. Distribution needs a Developer ID signature and notarisation.
set -eu
. "$(dirname "$0")/common.sh"

version="$(app_version)"
arch="$(uname -m)"
build_dist
python3 "$ROOT/packaging/licenses.py" --report "$DIST/THIRD-PARTY-PACKAGES.txt" >/dev/null

app="$DIST/$APP_NAME.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# The executable is named aster-office inside the bundle, as on Linux and Windows.
# The bridge sits beside it, where aster_mcp::setup::bridge_path looks.
cp "$ROOT/target/dist/$APP_BINARY" "$app/Contents/MacOS/aster-office"
cp "$ROOT/target/dist/$BRIDGE_BINARY" "$app/Contents/MacOS/$BRIDGE_BINARY"
cp "$ROOT/assets/brand/logo/aster.icns" "$app/Contents/Resources/aster.icns"
cp "$DIST/THIRD-PARTY-PACKAGES.txt" "$app/Contents/Resources/"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleExecutable</key><string>aster-office</string>
    <key>CFBundleIdentifier</key><string>$APP_ID</string>
    <key>CFBundleIconFile</key><string>aster</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
PLIST
# The file types the application opens, all with the rank Alternate.
cat "$ROOT/packaging/macos-document-types.plist" >> "$app/Contents/Info.plist"
printf '</dict>\n</plist>\n' >> "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
# Sign the helper first, then the bundle (which seals the executable and the
# helper's signature). Never --deep when signing: Apple deprecates it.
codesign --force --sign - "$app/Contents/MacOS/$BRIDGE_BINARY"
codesign --force --sign - "$app"
codesign --verify --deep --strict "$app"

dmg="$DIST/aster-office-$version-macos-$arch.dmg"
rm -f "$dmg"
staging="$(mktemp -d)"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname "$APP_NAME $version" -srcfolder "$staging" -format UDZO "$dmg"
rm -rf "$staging"

echo "macOS packages, version $version ($arch):"
report_size "$app/Contents/MacOS/aster-office"
report_size "$app/Contents/MacOS/$BRIDGE_BINARY"
report_size "$dmg"
