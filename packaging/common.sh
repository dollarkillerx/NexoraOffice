# Shared by the packaging scripts. Source it; do not run it.
#
# Until apps/desktop exists, the product that gets packaged is the Phase 0 input
# demo. Changing these four lines is all it takes to point packaging at the real
# application.
APP_NAME="Aster Office"
APP_BINARY="g1-ime"
APP_PACKAGE="g1-ime"
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
    cargo build --locked --profile dist -p "$APP_PACKAGE" --manifest-path "$ROOT/Cargo.toml" "$@"
}

report_size() {
    printf '  %-44s %8s KB\n' "$(basename "$1")" "$(( $(wc -c < "$1") / 1024 ))"
}
