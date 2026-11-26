# Shared by the packaging scripts. Source it; do not run it.
#
# The product that gets packaged is the application, aster-shell. It needs one
# helper beside its own executable: the MCP bridge, aster-mcp (see
# aster_mcp::setup::bridge_path). The PDF viewer's sandboxed worker is the same
# executable started with --pdf-worker, so it needs no file of its own.
APP_NAME="Aster Office"
APP_BINARY="aster-shell"
APP_PACKAGE="aster-shell"
BRIDGE_BINARY="aster-mcp"
BRIDGE_PACKAGE="aster-mcp-bridge"
# Placeholder. Must become a reverse-DNS name under a domain we own before
# anything is signed or published.
APP_ID="dev.aster.office.preview"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/target/dist-packages"

# The version is written once, in the workspace manifest.
app_version() {
    cargo metadata --format-version 1 --no-deps --locked --manifest-path "$ROOT/Cargo.toml" |
        python3 -c "import json, sys
packages = json.load(sys.stdin)['packages']
print(next(p['version'] for p in packages if p['name'] == '$APP_PACKAGE'))"
}

build_dist() {
    cargo build --locked --profile dist -p "$APP_PACKAGE" -p "$BRIDGE_PACKAGE" --manifest-path "$ROOT/Cargo.toml" "$@"
}

report_size() {
    printf '  %-44s %8s KB\n' "$(basename "$1")" "$(( $(wc -c < "$1") / 1024 ))"
}
