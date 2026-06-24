#!/bin/sh
# run-weston-demo.sh — end-to-end BDM greeter demo on a nested weston, with no
# root, no PAM, no real seat. Uses the bundled mock daemon (password: bacak).
#
# Requires: weston installed, plus a host display to nest into (X11 or Wayland).
# On a headless box it falls back to weston's headless backend.
#
# Usage:  packaging/prototype/run-weston-demo.sh

set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SOCK="${BDM_GREETER_SOCKET:-/tmp/bdm-demo.sock}"
export BDM_GREETER_SOCKET="$SOCK"

if ! command -v weston >/dev/null 2>&1; then
    echo "error: weston is not installed (apt install weston)" >&2
    exit 1
fi

echo "[demo] building greeter (gui) + mock daemon…"
cargo build --release -p bacak-greeter --features gui
cargo build --release -p bacak-greeter --example mock_daemon

echo "[demo] starting mock daemon on $SOCK"
"$ROOT/target/release/examples/mock_daemon" &
MOCK=$!
trap 'kill "$MOCK" 2>/dev/null || true' EXIT INT TERM
sleep 0.5

# Pick a nested backend matching the host session.
if [ -n "${WAYLAND_DISPLAY:-}" ]; then
    BK=wayland-backend.so
elif [ -n "${DISPLAY:-}" ]; then
    BK=x11-backend.so
else
    BK=headless-backend.so
fi
echo "[demo] launching weston (backend=$BK) hosting the greeter fullscreen"
echo "[demo] log in with password: bacak"

WESTON_BACKEND="$BK" \
WESTON_EXTRA_ARGS="--width=1024 --height=720" \
    "$ROOT/packaging/prototype/bacak-compositor" \
        --greeter "$ROOT/target/release/bacak-greeter"
