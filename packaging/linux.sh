#!/bin/sh
# Builds a .tar.gz and a .deb for the Linux machine this runs on.
#   packaging/linux.sh
set -eu
. "$(dirname "$0")/common.sh"

version="$(app_version)"
arch="$(uname -m)"
case "$arch" in
    x86_64) deb_arch=amd64 ;;
    aarch64) deb_arch=arm64 ;;
    *) deb_arch="$arch" ;;
esac
build_dist
python3 "$ROOT/packaging/licenses.py" --report "$DIST/THIRD-PARTY-PACKAGES.txt" >/dev/null

# One tree, laid out as it will sit on disk, used for both archives.
tree="$DIST/linux-root"
rm -rf "$tree"
mkdir -p "$tree/usr/bin" "$tree/usr/share/applications" "$tree/usr/share/doc/aster-office"
install -m 755 "$ROOT/target/dist/$APP_BINARY" "$tree/usr/bin/aster-office"
# The bridge must sit in the same directory as the executable.
install -m 755 "$ROOT/target/dist/$BRIDGE_BINARY" "$tree/usr/bin/$BRIDGE_BINARY"
cp "$DIST/THIRD-PARTY-PACKAGES.txt" "$tree/usr/share/doc/aster-office/"
for size in 16 32 48 64 128 256 512; do
    icons="$tree/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$icons"
    cp "$ROOT/assets/brand/logo/png/aster-icon-$size.png" "$icons/aster-office.png"
done
cat > "$tree/usr/share/applications/aster-office.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=$APP_NAME
Exec=aster-office %F
Icon=aster-office
Categories=Office;
# The formats in SUPPORTED_EXTENSIONS (crates/aster-shell/src/main.rs). This
# only lists what the application can open; it does not make it the default.
MimeType=text/markdown;text/plain;application/vnd.openxmlformats-officedocument.wordprocessingml.document;application/vnd.ms-word.document.macroEnabled.12;application/vnd.openxmlformats-officedocument.spreadsheetml.sheet;application/vnd.ms-excel.sheet.macroEnabled.12;application/vnd.openxmlformats-officedocument.presentationml.presentation;application/vnd.ms-powerpoint.presentation.macroEnabled.12;application/pdf;
Terminal=false
DESKTOP

tarball="$DIST/aster-office-$version-linux-$arch.tar.gz"
tar -C "$tree" -czf "$tarball" usr

deb="$DIST/aster-office_${version}_${deb_arch}.deb"
mkdir -p "$tree/DEBIAN"
cat > "$tree/DEBIAN/control" <<CONTROL
Package: aster-office
Version: $version
Section: editors
Priority: optional
Architecture: $deb_arch
Maintainer: Aster Office <packaging@invalid.example>
Depends: libc6, libfontconfig1, libfreetype6, libxkbcommon0
Description: $APP_NAME
 Native, offline office suite: Markdown, spreadsheets, documents,
 presentations and PDF. This is a preview build.
CONTROL
dpkg-deb --build --root-owner-group "$tree" "$deb" >/dev/null
rm -rf "$tree/DEBIAN"

echo "Linux packages, version $version ($arch):"
report_size "$tree/usr/bin/aster-office"
report_size "$tree/usr/bin/$BRIDGE_BINARY"
report_size "$tarball"
report_size "$deb"
