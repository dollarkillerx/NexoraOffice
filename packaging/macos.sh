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
cp "$ROOT/target/dist/$APP_BINARY" "$app/Contents/MacOS/$APP_BINARY"
cp "$ROOT/assets/brand/logo/aster.icns" "$app/Contents/Resources/aster.icns"
cp "$DIST/THIRD-PARTY-PACKAGES.txt" "$app/Contents/Resources/"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleExecutable</key><string>$APP_BINARY</string>
    <key>CFBundleIdentifier</key><string>$APP_ID</string>
    <key>CFBundleIconFile</key><string>aster</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$app" >/dev/null 2>&1

dmg="$DIST/aster-office-$version-macos-$arch.dmg"
rm -f "$dmg"
staging="$(mktemp -d)"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname "$APP_NAME $version" -srcfolder "$staging" -format UDZO "$dmg"
rm -rf "$staging"

echo "macOS packages, version $version ($arch):"
report_size "$app/Contents/MacOS/$APP_BINARY"
report_size "$dmg"
