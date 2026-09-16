#!/usr/bin/env bash
#
# Build the Altay Debian package (.deb).
#
#   scripts/build-deb.sh            build the .deb
#   scripts/build-deb.sh --install  build, then install it with apt
#   scripts/build-deb.sh --lint     build, then run lintian
#
# Honours SPDX/GPL metadata and the [package.metadata.deb] section in Cargo.toml.
set -euo pipefail

# Run from the project root regardless of where the script is invoked.
cd "$(dirname "$0")/.."

INSTALL=0
LINT=0
for arg in "$@"; do
    case "$arg" in
        --install) INSTALL=1 ;;
        --lint)    LINT=1 ;;
        -h|--help) sed -n '3,9p' "$0"; exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

step() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }

# 1. Toolchain checks.
command -v cargo >/dev/null || { echo "error: cargo (Rust) is required" >&2; exit 1; }
if ! cargo deb --version >/dev/null 2>&1; then
    step "cargo-deb not found — installing it"
    cargo install cargo-deb
fi
command -v dpkg-shlibdeps >/dev/null || \
    echo "warning: dpkg-dev not installed; dependency auto-detection may be limited" >&2

# 2. Refresh the themed icon from the source logo if available.
if [ -f assets/Altay.png ]; then
    step "Regenerating assets/altay.png from assets/Altay.png"
    cargo run --quiet --example mkicon || echo "warning: icon regeneration failed; using existing assets/altay.png" >&2
fi

# 3. Build the package (cargo-deb builds --release first).
step "Building the .deb (release profile)"
DEB_PATH="$(cargo deb | tail -n1)"
step "Built: $DEB_PATH ($(du -h "$DEB_PATH" | cut -f1))"

# apt needs a leading ./ for relative paths to tell them from package names;
# absolute paths are used verbatim.
case "$DEB_PATH" in
    /*) APT_ARG="$DEB_PATH" ;;
    *)  APT_ARG="./$DEB_PATH" ;;
esac

# 4. Optional lint.
if [ "$LINT" -eq 1 ]; then
    if command -v lintian >/dev/null; then
        step "Running lintian"
        lintian "$DEB_PATH" && echo "lintian: clean"
    else
        echo "warning: lintian not installed; skipping lint" >&2
    fi
fi

# 5. Optional install.
if [ "$INSTALL" -eq 1 ]; then
    step "Installing $DEB_PATH (sudo apt)"
    sudo apt install -y "$APT_ARG"
fi

step "Done."
echo "Install manually with:  sudo apt install $APT_ARG"
