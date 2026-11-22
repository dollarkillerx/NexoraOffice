#!/bin/sh
# Builds the platform icon files from the PNG exports in png/.
#   aster.icns  macOS   (needs iconutil, so this part runs on macOS only)
#   aster.ico   Windows (plain Python, runs anywhere)
# Both outputs are committed, so packaging never has to run this.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"

python3 - "$here" <<'PY'
import struct, sys, pathlib
here = pathlib.Path(sys.argv[1])
sizes = [16, 32, 48, 64, 128, 256]
images = [(s, (here / "png" / f"aster-icon-{s}.png").read_bytes()) for s in sizes]
# ICO: a directory of entries followed by the images, stored as PNG.
header = struct.pack("<HHH", 0, 1, len(images))
offset = len(header) + 16 * len(images)
entries, blobs = b"", b""
for size, data in images:
    side = 0 if size == 256 else size  # 0 means 256 in this format
    entries += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
    blobs += data
(here / "aster.ico").write_bytes(header + entries + blobs)
print("wrote aster.ico")
PY

if command -v iconutil >/dev/null 2>&1; then
    set_dir="$(mktemp -d)/aster.iconset"
    mkdir -p "$set_dir"
    for pair in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x \
                256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
        cp "$here/png/aster-icon-${pair%%:*}.png" "$set_dir/icon_${pair##*:}.png"
    done
    iconutil -c icns "$set_dir" -o "$here/aster.icns"
    echo "wrote aster.icns"
else
    echo "iconutil not found: aster.icns was not rebuilt" >&2
fi
