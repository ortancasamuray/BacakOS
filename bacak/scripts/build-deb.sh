#!/usr/bin/env bash
# build-deb.sh — Build a Debian package for Bacak OS.
#
# Produces target/deb/bacak_<version>_<arch>.deb from the workspace.
# No external tools required beyond cargo + dpkg-dev.
#
# Usage:
#   scripts/build-deb.sh                       # build for host architecture
#   scripts/build-deb.sh --arch arm64          # specify Debian arch (host or cross-built)
#   scripts/build-deb.sh --output ~/out        # custom output dir
#   scripts/build-deb.sh --skip-build          # reuse existing target/release/bacak
#   scripts/build-deb.sh --lintian             # also run lintian after building
#   scripts/build-deb.sh --install             # sudo dpkg -i the resulting .deb
#   scripts/build-deb.sh --version 0.2.0-rc1   # override the version string
#
# Env overrides (all optional):
#   BACAK_MAINTAINER   "Name <email>"        (default below)
#   BACAK_HOMEPAGE     URL                    (default below)

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

PKG_NAME="bacak"
SECTION="utils"
PRIORITY="optional"
DEPENDS="libc6 (>= 2.34), init-system-helpers (>= 1.52)"
RECOMMENDS="xdg-utils, fonts-inter"
SUGGESTS="greetd, regreet, cage, chromium | firefox"

MAINTAINER="${BACAK_MAINTAINER:-Bacak OS contributors <bacak@example.org>}"
HOMEPAGE="${BACAK_HOMEPAGE:-https://github.com/bacak-os/bacak}"

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

ARCH=""
OUTPUT_DIR=""
SKIP_BUILD=0
RUN_LINTIAN=0
DO_INSTALL=0
VERSION_OVERRIDE=""

usage() {
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)        ARCH="${2:?missing value}"; shift 2 ;;
        --output)      OUTPUT_DIR="${2:?missing value}"; shift 2 ;;
        --version)     VERSION_OVERRIDE="${2:?missing value}"; shift 2 ;;
        --skip-build)  SKIP_BUILD=1; shift ;;
        --lintian)     RUN_LINTIAN=1; shift ;;
        --install)     DO_INSTALL=1; shift ;;
        -h|--help)     usage; exit 0 ;;
        *)             echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------------------
# Pre-flight
# ---------------------------------------------------------------------------

die() { echo "error: $*" >&2; exit 1; }

command -v cargo    >/dev/null || die "cargo not found in PATH"
command -v dpkg-deb >/dev/null || die "dpkg-deb not found — apt install dpkg-dev"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

[[ -f Cargo.toml ]] || die "Cargo.toml not found at $ROOT"

# Read version from the workspace package table.
if [[ -n "$VERSION_OVERRIDE" ]]; then
    VERSION="$VERSION_OVERRIDE"
else
    VERSION="$(
        sed -n '/^\[workspace\.package\]/,/^\[/ {
            s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p
        }' Cargo.toml | head -n1
    )"
fi
[[ -n "$VERSION" ]] || die "could not detect version from Cargo.toml"

# Resolve architecture.
if [[ -z "$ARCH" ]]; then
    ARCH="$(dpkg-architecture -qDEB_HOST_ARCH 2>/dev/null || dpkg --print-architecture)"
fi
[[ -n "$ARCH" ]] || die "could not detect architecture"

[[ -n "$OUTPUT_DIR" ]] || OUTPUT_DIR="$ROOT/target/deb"
STAGE_DIR="$ROOT/target/deb-stage/${PKG_NAME}_${VERSION}_${ARCH}"

echo "==> Packaging ${PKG_NAME} ${VERSION} (${ARCH})"
echo "    output:  $OUTPUT_DIR"
echo "    staging: $STAGE_DIR"

# ---------------------------------------------------------------------------
# Build (unless skipped)
# ---------------------------------------------------------------------------

if [[ "$SKIP_BUILD" -eq 0 ]]; then
    echo "==> cargo build --release --bin $PKG_NAME"
    cargo build --release --bin "$PKG_NAME"
    # The Wayland session host. Built with the `udev` feature so it can
    # drive DRM/KMS + libseat directly — that's what the session entry
    # below Execs.
    echo "==> cargo build --release -p bacak-compositor --features udev"
    cargo build --release -p bacak-compositor --features udev
fi

BIN_SRC="$ROOT/target/release/$PKG_NAME"
[[ -x "$BIN_SRC" ]] || die "release binary not found: $BIN_SRC (drop --skip-build?)"
COMPOSITOR_SRC="$ROOT/target/release/bacak-compositor"
[[ -x "$COMPOSITOR_SRC" ]] || die "compositor binary not found: $COMPOSITOR_SRC (drop --skip-build?)"

# ---------------------------------------------------------------------------
# Stage the filesystem
# ---------------------------------------------------------------------------

rm -rf "$STAGE_DIR"
install -d \
    "$STAGE_DIR/DEBIAN" \
    "$STAGE_DIR/usr/bin" \
    "$STAGE_DIR/etc/profile.d" \
    "$STAGE_DIR/usr/share/$PKG_NAME/ui" \
    "$STAGE_DIR/usr/share/$PKG_NAME/docs" \
    "$STAGE_DIR/usr/share/$PKG_NAME/contrib" \
    "$STAGE_DIR/usr/share/applications" \
    "$STAGE_DIR/usr/share/wayland-sessions" \
    "$STAGE_DIR/usr/share/doc/$PKG_NAME" \
    "$STAGE_DIR/usr/share/man/man1" \
    "$STAGE_DIR/usr/lib/systemd/system"

# Binary — install and strip a copy (keep the original for debugging).
install -m 0755 "$BIN_SRC" "$STAGE_DIR/usr/bin/$PKG_NAME"
if command -v strip >/dev/null; then
    strip --strip-unneeded "$STAGE_DIR/usr/bin/$PKG_NAME" 2>/dev/null || true
fi

# Compositor — the real Wayland session host the session entry Execs.
# Same install + strip as the CLI.
install -m 0755 "$COMPOSITOR_SRC" "$STAGE_DIR/usr/bin/bacak-compositor"
if command -v strip >/dev/null; then
    strip --strip-unneeded "$STAGE_DIR/usr/bin/bacak-compositor" 2>/dev/null || true
fi

# Shell drop-in: repair DISPLAY for terminals that clobber it under Wayland.
# VTE-based terminals (mate-terminal, gnome-terminal, …) set the child shell's
# DISPLAY to the GDK *Wayland* display name (e.g. "wayland-bacak-0"), which is
# not a valid X display — so X11/XWayland apps launched from such a shell die
# with "Could not connect to an X display". The compositor exports the real
# XWayland DISPLAY (":0") to the systemd user environment; this restores it.
# Login shells run /etc/profile.d automatically; for non-login interactive bash
# enable mate-terminal's "Run command as a login shell" or source this from
# ~/.bashrc.
cat > "$STAGE_DIR/etc/profile.d/bacak-display.sh" <<'EOF'
# Installed by bacak. Undo VTE's DISPLAY clobber under Wayland (see package).
case "${DISPLAY:-}" in
    ''|wayland-*)
        __bacak_x=$(systemctl --user show-environment 2>/dev/null \
            | sed -n 's/^DISPLAY=//p')
        [ -n "$__bacak_x" ] && export DISPLAY="$__bacak_x"
        unset __bacak_x
        ;;
esac
EOF
chmod 0644 "$STAGE_DIR/etc/profile.d/bacak-display.sh"

# Frontend prototype, if it's still in the tree. The project's primary
# surface is now the Rust Wayland compositor (`bacak-compositor`); the
# HTML preview is shipped only when its sources are present so this
# script keeps working on trees that have already removed them.
SHIP_UI=0
# Frontend sources live under assets/ui/ in the current tree; fall back to
# the project root for older layouts that kept them alongside Cargo.toml.
UI_SRC=""
for d in "$ROOT/assets/ui" "$ROOT"; do
    if [[ -f "$d/index.html" && -f "$d/styles.css" && -f "$d/app.js" ]]; then
        UI_SRC="$d"; break
    fi
done
if [[ -n "$UI_SRC" ]]; then
    SHIP_UI=1
    install -m 0644 "$UI_SRC/styles.css" "$STAGE_DIR/usr/share/$PKG_NAME/ui/styles.css"
    install -m 0644 "$UI_SRC/app.js"     "$STAGE_DIR/usr/share/$PKG_NAME/ui/app.js"
    # Strip Google Fonts <link> tags — lintian flags them as privacy
    # breaches. Falls back to system fonts (Inter via fonts-inter).
    sed -E '/<link[^>]*fonts\.(google|gstatic)apis?\.com/d; /<link[^>]*fonts\.googleapis\.com/d; /<link[^>]*fonts\.gstatic\.com/d' \
        "$UI_SRC/index.html" > "$STAGE_DIR/usr/share/$PKG_NAME/ui/index.html"
    chmod 0644 "$STAGE_DIR/usr/share/$PKG_NAME/ui/index.html"
else
    # Drop the empty ui/ dir — nothing landed in it.
    rmdir "$STAGE_DIR/usr/share/$PKG_NAME/ui" 2>/dev/null || true
    echo "    note: HTML preview sources not found — packaging CLI only."
fi

# Architecture / design docs.
for f in ARCHITECTURE.md DESIGN_SYSTEM.md; do
    [[ -f "$ROOT/$f" ]] && install -m 0644 "$ROOT/$f" "$STAGE_DIR/usr/share/$PKG_NAME/docs/$f"
done

# Desktop entry — only when we actually have a preview to point at.
if [[ "$SHIP_UI" -eq 1 ]]; then
    cat > "$STAGE_DIR/usr/share/applications/${PKG_NAME}-preview.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Bacak Desktop Preview
GenericName=OS Shell Prototype
Comment=Glassmorphic desktop environment prototype (dock, snap windows, OSK)
Exec=xdg-open /usr/share/${PKG_NAME}/ui/index.html
Icon=bacak
Terminal=false
Categories=Utility;Development;
Keywords=desktop;shell;preview;bacak;
StartupNotify=false
EOF
    chmod 0644 "$STAGE_DIR/usr/share/applications/${PKG_NAME}-preview.desktop"
else
    rmdir "$STAGE_DIR/usr/share/applications" 2>/dev/null || true
fi

# Wayland session entry — display managers (greetd/SDDM/GDM) will list this
# under the "Bacak OS" session and Exec `bacak-compositor` (udev backend)
# when picked. The Exec of a Wayland session must be the compositor itself
# (the process that provides the display), not a client.
cat > "$STAGE_DIR/usr/share/wayland-sessions/${PKG_NAME}.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Bacak OS
Comment=Glassmorphic Rust desktop environment
Exec=bacak-compositor
TryExec=bacak-compositor
DesktopNames=Bacak
Keywords=desktop;shell;wayland;
EOF
chmod 0644 "$STAGE_DIR/usr/share/wayland-sessions/${PKG_NAME}.desktop"

# contrib/ — sample greetd integration files. Not auto-installed; admin
# copies the parts they want into /etc/greetd/.
cat > "$STAGE_DIR/usr/share/$PKG_NAME/contrib/greetd-config.toml" <<'EOF'
# Example /etc/greetd/config.toml for Bacak OS.
#
# Install (as root):
#   apt install greetd regreet cage
#   cp /usr/share/bacak/contrib/greetd-config.toml /etc/greetd/config.toml
#   cp /usr/share/bacak/contrib/regreet.css       /etc/greetd/regreet.css
#   systemctl enable greetd.service
#   systemctl set-default graphical.target
#
# At login, pick "Bacak OS" from the session menu.

[terminal]
vt = 1

[default_session]
# `cage` provides a single-window Wayland compositor for the greeter.
# regreet is GTK4 and reads /etc/greetd/regreet.css for theming.
command = "cage -s -- regreet"
user = "_greetd"

# Optional: autologin (uncomment + adjust)
# [initial_session]
# command = "bacak shell"
# user = "your-username"
EOF

cat > "$STAGE_DIR/usr/share/$PKG_NAME/contrib/regreet.css" <<'EOF'
/* regreet theme for Bacak OS — ported from the workspace design system.
 * Drop this at /etc/greetd/regreet.css.
 *
 * Tokens (kept inline because regreet does not load CSS variables from
 * external files reliably):
 *   Rust accent  : #d96c2d / #ef8348
 *   Aegean deep  : #062a3d / mid #0c4e6b
 *   Glass border : rgba(255,255,255,0.22)
 */

window, .background {
    background:
        radial-gradient(circle at 80% 10%, #2c8aa3 0%, transparent 55%),
        radial-gradient(circle at 10% 90%, #0f5970 0%, transparent 60%),
        linear-gradient(180deg, #0c4e6b 0%, #0a3c55 45%, #062a3d 100%);
}

/* Card / dialog around the login form */
.dialog, box.vertical, #main_content {
    background-color: rgba(8, 26, 38, 0.65);
    border: 1px solid rgba(255, 255, 255, 0.22);
    border-radius: 22px;
    padding: 32px;
    color: rgba(255, 255, 255, 0.92);
    box-shadow: 0 28px 70px -18px rgba(2, 28, 42, 0.7);
}

label {
    color: rgba(255, 255, 255, 0.92);
    font-weight: 500;
    letter-spacing: 0.1px;
}

entry, combobox button {
    background-color: rgba(0, 0, 0, 0.30);
    border: 1px solid rgba(255, 255, 255, 0.10);
    border-radius: 14px;
    padding: 10px 14px;
    color: #ffffff;
    caret-color: #ef8348;
    min-height: 28px;
}
entry:focus {
    border-color: #ef8348;
    box-shadow: 0 0 0 3px rgba(217, 108, 45, 0.22);
}

button.suggested-action, button#login_button {
    background-color: #d96c2d;
    background-image: none;
    color: #ffffff;
    border: 0;
    border-radius: 999px;
    padding: 10px 22px;
    box-shadow: 0 4px 14px rgba(217, 108, 45, 0.35);
    transition: background-color 140ms ease, transform 140ms cubic-bezier(0.34, 1.56, 0.64, 1);
}
button.suggested-action:hover, button#login_button:hover {
    background-color: #ef8348;
}
button.suggested-action:active, button#login_button:active {
    transform: scale(0.98);
}

button {
    background-color: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.10);
    border-radius: 14px;
    color: #ffffff;
    padding: 8px 16px;
}
button:hover { background-color: rgba(255, 255, 255, 0.12); }
EOF

cat > "$STAGE_DIR/usr/share/$PKG_NAME/contrib/README.md" <<EOF
# Bacak OS — display-manager integration

This directory ships sample configs for running Bacak as a Wayland session
under a display manager. Nothing here is auto-installed; copy what you want.

## Recommended path: greetd + regreet

\`\`\`bash
sudo apt install greetd regreet cage
sudo cp /usr/share/$PKG_NAME/contrib/greetd-config.toml /etc/greetd/config.toml
sudo cp /usr/share/$PKG_NAME/contrib/regreet.css       /etc/greetd/regreet.css
sudo systemctl enable greetd.service
sudo systemctl set-default graphical.target
sudo reboot
\`\`\`

After reboot, the regreet greeter shows the Bacak glassmorphic login form.
Picking the **Bacak OS** session runs \`bacak shell\`, which launches the
prototype shell in a kiosk browser. (Long-term, \`bacak shell\` will be
replaced by a Smithay-based Wayland compositor written against the
\`bacak-wm\` crate.)

## Files

| File | Destination | Purpose |
|------|-------------|---------|
| \`greetd-config.toml\` | \`/etc/greetd/config.toml\`   | greetd daemon config |
| \`regreet.css\`        | \`/etc/greetd/regreet.css\`   | Glassmorphism theme |
| (auto) \`/usr/share/wayland-sessions/$PKG_NAME.desktop\` | already installed | Session entry |

## Alternatives

- **SDDM** — \`/usr/share/wayland-sessions/$PKG_NAME.desktop\` is already
  in the standard location, so SDDM lists Bacak OS automatically. Install
  a QML glassmorphism theme separately if desired.
- **GDM** — same story, but GDM imposes more GNOME assumptions; greetd is
  the recommended path.

## Notes

- \`bacak shell --dry-run\` prints the browser command that would run
  without actually launching it. Useful for debugging session entries.
- The user \`_greetd\` is created by the greetd package's postinst on
  Debian; no manual setup needed.
EOF

chmod 0644 "$STAGE_DIR/usr/share/$PKG_NAME/contrib/"*

# Lintian override — only relevant when the preview .desktop exists.
if [[ "$SHIP_UI" -eq 1 ]]; then
    install -d "$STAGE_DIR/usr/share/lintian/overrides"
    cat > "$STAGE_DIR/usr/share/lintian/overrides/$PKG_NAME" <<EOF
# xdg-open lives in the xdg-utils package, which is declared in Recommends.
$PKG_NAME: desktop-command-not-in-package xdg-open [usr/share/applications/${PKG_NAME}-preview.desktop]
EOF
    chmod 0644 "$STAGE_DIR/usr/share/lintian/overrides/$PKG_NAME"
fi

# ---------------------------------------------------------------------------
# systemd service unit
# ---------------------------------------------------------------------------

cat > "$STAGE_DIR/usr/lib/systemd/system/bacak-display-manager.service" <<'EOF'
[Unit]
Description=Bacak Display Manager
Documentation=man:bacak-display-manager(8)
Conflicts=getty@tty1.service
After=systemd-user-sessions.service getty@tty1.service plymouth-quit.service
After=systemd-logind.service
Wants=systemd-logind.service
Conflicts=plymouth-start.service

[Service]
Type=simple
ExecStart=/usr/bin/bacak-display-manager
TTYPath=/dev/tty1
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
Environment=XDG_VTNR=1
User=root
Restart=always
RestartSec=1s
KillMode=mixed
ExecStopPost=/usr/bin/loginctl terminate-seat seat0

[Install]
Alias=display-manager.service
WantedBy=graphical.target
EOF
chmod 0644 "$STAGE_DIR/usr/lib/systemd/system/bacak-display-manager.service"

# DEBIAN/postinst — enable the display manager service after install/upgrade.
cat > "$STAGE_DIR/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
case "$1" in
    configure)
        if command -v deb-systemd-helper >/dev/null 2>&1; then
            deb-systemd-helper enable bacak-display-manager.service >/dev/null || true
        elif command -v systemctl >/dev/null 2>&1; then
            systemctl enable bacak-display-manager.service 2>/dev/null || true
        fi
        ;;
esac
EOF
chmod 0755 "$STAGE_DIR/DEBIAN/postinst"

# DEBIAN/prerm — disable the service before package removal.
cat > "$STAGE_DIR/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
case "$1" in
    remove|purge)
        if command -v deb-systemd-helper >/dev/null 2>&1; then
            deb-systemd-helper disable bacak-display-manager.service >/dev/null || true
        elif command -v systemctl >/dev/null 2>&1; then
            systemctl disable bacak-display-manager.service 2>/dev/null || true
        fi
        ;;
esac
EOF
chmod 0755 "$STAGE_DIR/DEBIAN/prerm"

# ---------------------------------------------------------------------------
# DEBIAN/control
# ---------------------------------------------------------------------------

# Installed-size in KiB, excluding the DEBIAN dir itself (per policy).
INSTALLED_SIZE="$(du -sk --exclude=DEBIAN "$STAGE_DIR" | awk '{print $1}')"

cat > "$STAGE_DIR/DEBIAN/control" <<EOF
Package: $PKG_NAME
Version: $VERSION
Section: $SECTION
Priority: $PRIORITY
Architecture: $ARCH
Maintainer: $MAINTAINER
Homepage: $HOMEPAGE
Depends: $DEPENDS
Recommends: $RECOMMENDS
Suggests: $SUGGESTS
Installed-Size: $INSTALLED_SIZE
Description: service CLI for the Bacak OS desktop environment
 Bacak is a glassmorphic desktop shell built on Rust. This package ships
 the workspace CLI, which exposes every backend service from a terminal:
 .
   * virtual filesystem (native paths + ZIP/TAR/TAR.GZ as virtual folders)
   * window manager core (floating + snap zones, workspace state)
   * on-screen keyboard state machine + gesture classifier
   * device surfaces — audio sinks, Wi-Fi networks, Bluetooth devices
   * dock controls — pin/unpin/list, config edit/validate, session show/clear
EOF
if [[ "$SHIP_UI" -eq 1 ]]; then
    cat >> "$STAGE_DIR/DEBIAN/control" <<EOF
 .
 The bundled frontend prototype is installed at
 /usr/share/bacak/ui/index.html and demonstrates the dock, snap-aware
 windows, layout-aware on-screen keyboard, and the network/Bluetooth/sound
 tray panels. Use "Bacak Desktop Preview" from the application menu or
 open the file directly in a browser.
EOF
fi
chmod 0644 "$STAGE_DIR/DEBIAN/control"

# DEBIAN/conffiles — config files under /etc that dpkg must preserve across
# upgrades (and that keep lintian quiet). Only our profile.d drop-in.
cat > "$STAGE_DIR/DEBIAN/conffiles" <<EOF
/etc/profile.d/bacak-display.sh
EOF
chmod 0644 "$STAGE_DIR/DEBIAN/conffiles"

# ---------------------------------------------------------------------------
# /usr/share/doc/<pkg>/copyright (machine-readable format)
# ---------------------------------------------------------------------------

cat > "$STAGE_DIR/usr/share/doc/$PKG_NAME/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: $PKG_NAME
Upstream-Contact: $MAINTAINER
Source: $HOMEPAGE

Files: *
Copyright: $(date +%Y) Bacak OS contributors
License: MIT or Apache-2.0

License: MIT
 Permission is hereby granted, free of charge, to any person obtaining
 a copy of this software and associated documentation files (the
 "Software"), to deal in the Software without restriction, including
 without limitation the rights to use, copy, modify, merge, publish,
 distribute, sublicense, and/or sell copies of the Software, and to
 permit persons to whom the Software is furnished to do so, subject to
 the following conditions:
 .
 The above copyright notice and this permission notice shall be
 included in all copies or substantial portions of the Software.
 .
 THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
 IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
 CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
 TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
 SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

License: Apache-2.0
 Licensed under the Apache License, Version 2.0 (the "License"); you
 may not use this file except in compliance with the License. You may
 obtain a copy of the License at
 .
   http://www.apache.org/licenses/LICENSE-2.0
 .
 On Debian systems, the complete text of the Apache License, Version
 2.0 can be found in /usr/share/common-licenses/Apache-2.0.
EOF
chmod 0644 "$STAGE_DIR/usr/share/doc/$PKG_NAME/copyright"

# ---------------------------------------------------------------------------
# Debian changelog (gzipped). Native packages (no "-revision" suffix) use
# changelog.gz; non-native packages use changelog.Debian.gz.
# ---------------------------------------------------------------------------

if [[ "$VERSION" == *-* ]]; then
    CHANGELOG_NAME="changelog.Debian"
else
    CHANGELOG_NAME="changelog"
fi
CHANGELOG="$STAGE_DIR/usr/share/doc/$PKG_NAME/$CHANGELOG_NAME"
cat > "$CHANGELOG" <<EOF
$PKG_NAME ($VERSION) unstable; urgency=medium

  * Debian package for Bacak OS ${VERSION}.
    - bacak CLI: ls, archive, wm, osk, device, config, session, dock subcommands.
    - Wayland session entry: /usr/share/wayland-sessions/$PKG_NAME.desktop.
    - greetd integration samples at /usr/share/$PKG_NAME/contrib/.
    - Architecture + design system docs at /usr/share/$PKG_NAME/docs/.$( [[ "$SHIP_UI" -eq 1 ]] && printf '\n    - Frontend prototype shipped at /usr/share/%s/ui/ + "Bacak Desktop Preview" .desktop.' "$PKG_NAME" )

 -- $MAINTAINER  $(date -R)
EOF
gzip -9n "$CHANGELOG"
chmod 0644 "${CHANGELOG}.gz"

# ---------------------------------------------------------------------------
# man page (gzipped)
# ---------------------------------------------------------------------------

MAN="$STAGE_DIR/usr/share/man/man1/${PKG_NAME}.1"
cat > "$MAN" <<EOF
.TH BACAK 1 "$(date '+%B %Y')" "Bacak OS $VERSION" "User Commands"
.SH NAME
bacak \- service CLI for the Bacak OS desktop environment
.SH SYNOPSIS
.B bacak
[\fB\-\-json\fR]
.I COMMAND
[\fIARGS\fR...]
.SH DESCRIPTION
.B bacak
exercises every service crate of the Bacak OS workspace from a terminal:
the virtual filesystem (native paths plus archive contents), the archive
backends (ZIP, TAR, TAR.GZ), the window manager core, the on-screen
keyboard state machine, and the device surfaces (audio, Wi-Fi,
Bluetooth).
.SH COMMANDS
.TP
.B ls \fIPATH\fR [\fB\-\-archive\fR \fIFILE\fR]
List a directory through the virtual filesystem. With \fB\-\-archive\fR,
\fIPATH\fR is treated as relative to inside the named archive.
.TP
.B archive list \fIARCHIVE\fR [\fIDIR\fR]
List entries directly inside \fIDIR\fR (default: root) of \fIARCHIVE\fR.
.TP
.B archive extract \fIARCHIVE\fR \fIINSIDE\fR \fITO\fR
Extract a single entry to a destination on disk.
.TP
.B wm demo
Run a scripted window manager demonstration.
.TP
.B osk demo
Run a scripted on-screen keyboard demonstration.
.TP
.B "device sound" \fISTATE\fR|\fBvolume\fR \fIN\fR|\fBmute\fR|\fBoutput\fR \fIID\fR
Audio controls — print state, set master volume (0..=100), toggle mute,
or pick the active output sink.
.TP
.B "device net" \fISTATE\fR|\fBtoggle\fR \fIon|off\fR|\fBconnect\fR \fISSID\fR [\fB\-\-password\fR \fIPW\fR]|\fBdisconnect\fR
Wi-Fi controls.
.TP
.B "device bt" \fISTATE\fR|\fBtoggle\fR \fIon|off\fR|\fBscan\fR \fIon|off\fR|\fBpair\fR \fIMAC\fR|\fBconnect\fR \fIMAC\fR|\fBdisconnect\fR \fIMAC\fR
Bluetooth controls.
.SH OPTIONS
.TP
.B \-\-json
Emit machine-readable JSON instead of pretty terminal output.
.SH FILES
.TP
.I /usr/share/wayland-sessions/bacak.desktop
Wayland session entry — pick "Bacak OS" at the login screen.
.TP
.I /usr/share/bacak/contrib/
Sample greetd / regreet integration files (copy into /etc/greetd/).
.TP
.I /usr/share/bacak/docs/
Architecture and design system documents.
.SH EXAMPLES
.PP
List the contents of a ZIP without extracting it:
.PP
.RS
.B bacak ls \-\-archive ./release.zip docs
.RE
.PP
Connect to a secured Wi-Fi network:
.PP
.RS
.B bacak device net connect rust-lang \-\-password hunter2
.RE
.SH SEE ALSO
.IR /usr/share/bacak/docs/ARCHITECTURE.md
EOF
gzip -9n "$MAN"
chmod 0644 "${MAN}.gz"

# ---------------------------------------------------------------------------
# Normalize permissions
# ---------------------------------------------------------------------------

find "$STAGE_DIR" -type d -exec chmod 0755 {} +
find "$STAGE_DIR/usr/share" -type f -exec chmod 0644 {} +
chmod 0755 "$STAGE_DIR/usr/bin/$PKG_NAME"

# ---------------------------------------------------------------------------
# Build the .deb
# ---------------------------------------------------------------------------

mkdir -p "$OUTPUT_DIR"
OUT_DEB="$OUTPUT_DIR/${PKG_NAME}_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$STAGE_DIR" "$OUT_DEB" >/dev/null
echo "==> Built: $OUT_DEB"
ls -lh "$OUT_DEB" | awk '{print "    " $5 "  " $NF}'

# ---------------------------------------------------------------------------
# Optional: lintian
# ---------------------------------------------------------------------------

if [[ "$RUN_LINTIAN" -eq 1 ]]; then
    if command -v lintian >/dev/null; then
        echo "==> lintian $OUT_DEB"
        # Don't fail the build on lintian warnings, but surface them.
        lintian --tag-display-limit 0 "$OUT_DEB" || true
    else
        echo "lintian not installed — skipping (apt install lintian)"
    fi
fi

# ---------------------------------------------------------------------------
# Optional: install
# ---------------------------------------------------------------------------

if [[ "$DO_INSTALL" -eq 1 ]]; then
    echo "==> sudo dpkg -i $OUT_DEB"
    sudo dpkg -i "$OUT_DEB"
fi

echo "==> Done."
