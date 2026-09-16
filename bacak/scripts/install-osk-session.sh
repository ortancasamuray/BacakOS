#!/usr/bin/env bash
#
# install-osk-session.sh — install the freshly-built udev compositor (with the
# on-screen keyboard) as the GDM "Bacak OS" Wayland session, and wire the
# toolkit IM modules so GTK/Qt apps use Wayland text-input-v3 (which both
# avoids the crashy system `scim` IM module and feeds the OSK auto-show).
#
# Run with root:   sudo bash scripts/install-osk-session.sh
#
# Idempotent and reversible: the previous binary + desktop file are backed up
# to *.bak-<timestamp> before being replaced.

set -euo pipefail

REPO="${REPO:-/home/os/bacak}"
SRC="$REPO/target/release/bacak-compositor"
DST="/usr/bin/bacak-compositor"
WRAPPER="/usr/bin/bacak-session"
DESKTOP="/usr/share/wayland-sessions/bacak.desktop"
TS="$(date +%Y%m%d-%H%M%S)"

if [[ $EUID -ne 0 ]]; then
    echo "error: must run as root (sudo bash $0)" >&2
    exit 1
fi

if [[ ! -x "$SRC" ]]; then
    echo "error: $SRC not found — build it first:" >&2
    echo "  cargo build --release -p bacak-compositor --features udev" >&2
    exit 1
fi

echo "==> 1/4  Backing up + installing the compositor binary"
if [[ -e "$DST" ]]; then
    cp -a "$DST" "$DST.bak-$TS"
    echo "    backup: $DST.bak-$TS"
fi
install -m 0755 "$SRC" "$DST"
if command -v strip >/dev/null; then
    strip --strip-unneeded "$DST" 2>/dev/null || true
fi
echo "    installed: $DST ($(stat -c %s "$DST") bytes)"

echo "==> 2/4  Installing the session wrapper (IM env → Wayland text-input)"
cat > "$WRAPPER" <<'EOF'
#!/usr/bin/env bash
# Bacak session entry. Force the toolkit IM modules to the Wayland
# text-input-v3 backend: this (a) sidesteps the system `scim`/`ibus` GTK IM
# modules that segfault under a bare Bacak session, and (b) is exactly the
# context that drives Bacak's on-screen-keyboard auto-show. Child apps the
# compositor spawns inherit this environment.
export GTK_IM_MODULE=wayland
export QT_IM_MODULE=wayland
export XMODIFIERS="@im=none"
export CLUTTER_IM_MODULE=wayland
exec /usr/bin/bacak-compositor "$@"
EOF
chmod 0755 "$WRAPPER"
echo "    installed: $WRAPPER"

echo "==> 3/4  Pointing the GDM session at the wrapper"
if [[ -e "$DESKTOP" ]]; then
    cp -a "$DESKTOP" "$DESKTOP.bak-$TS"
    echo "    backup: $DESKTOP.bak-$TS"
fi
cat > "$DESKTOP" <<'EOF'
[Desktop Entry]
Type=Application
Name=Bacak OS
Comment=Glassmorphic Rust desktop environment
Exec=bacak-session
TryExec=bacak-session
DesktopNames=Bacak
Keywords=desktop;shell;wayland;
EOF
chmod 0644 "$DESKTOP"
echo "    session: $DESKTOP → Exec=bacak-session"

echo "==> 4/4  Verifying"
command -v bacak-session >/dev/null && echo "    bacak-session on PATH: ok"
"$DST" --version 2>/dev/null || true   # may not support --version; non-fatal
echo
echo "Done. The new compositor (with the on-screen keyboard) is installed."
echo
echo "Next:"
echo "  1. Log OUT of the current Bacak session (or reboot)."
echo "  2. At the GDM greeter, pick the gear → \"Bacak OS\"."
echo "  3. Log in. Tap a text field in any GTK/Qt app → the keyboard"
echo "     should slide up from the bottom; drag its title strip to move it,"
echo "     and use the TR/EN/☺ keys to switch layouts."
echo
echo "Roll back:  sudo cp -a $DST.bak-$TS $DST   (and likewise the .desktop)"
