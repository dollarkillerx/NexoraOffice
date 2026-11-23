#!/bin/sh
# The checks the GitHub workflows run, on this machine. While the project is in
# development the workflows are switched off and this is the gate: run it
# before merging anything into main.
#
#   scripts/ci.sh             format, lint, tests and licences of the workspace
#   scripts/ci.sh --spikes    also the G4 and G5 spikes, which are workspaces of
#                             their own and take minutes to build
#   scripts/ci.sh --package   also build the installer for this system
#
# Stops at the first step that fails. It checks one system only: what a build
# on the other two would have found, it cannot find.
set -eu

spikes=0
package=0
for arg in "$@"; do
    case "$arg" in
        --spikes) spikes=1 ;;
        --package) package=1 ;;
        *)
            echo "usage: scripts/ci.sh [--spikes] [--package]" >&2
            exit 2
            ;;
    esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
started=$(date +%s)

step() {
    printf '\n== %s\n' "$1"
}

step "format"
cargo fmt --all --check

step "lint"
cargo clippy --locked --workspace --all-targets -- -D warnings

step "test"
cargo test --locked --workspace

step "licences"
python3 packaging/licenses.py --check

# Security advisories need cargo-deny, which is not part of the toolchain.
step "advisories"
if cargo deny --version >/dev/null 2>&1; then
    cargo deny --locked check
else
    echo "skipped: cargo-deny is not installed (cargo install cargo-deny)"
fi

if [ "$spikes" = 1 ]; then
    for spike in g4-ooxml g5-formula; do
        step "$spike: format"
        (cd "spikes/$spike" && cargo fmt --check)

        step "$spike: lint and test without the libraries under evaluation"
        (cd "spikes/$spike" &&
            cargo clippy --locked --all-targets --no-default-features -- -D warnings &&
            cargo test --locked --no-default-features)

        step "$spike: lint and test with them"
        (cd "spikes/$spike" &&
            cargo clippy --locked --all-targets -- -D warnings &&
            cargo test --locked)
    done
fi

if [ "$package" = 1 ]; then
    step "package"
    case "$(uname -s)" in
        Darwin) packaging/macos.sh ;;
        Linux) packaging/linux.sh ;;
        *)
            echo "on Windows run packaging/windows.ps1 from PowerShell" >&2
            exit 1
            ;;
    esac
fi

printf '\nall checks passed in %s s\n' "$(($(date +%s) - started))"
