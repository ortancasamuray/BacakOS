#!/usr/bin/env bash
# install.sh — install / enable / revert / uninstall the Bacak Display Manager.
#
# Subcommands:
#   install     Install the .deb + compositor + session wrapper. Does NOT change
#               your display manager — safe.
#   enable      Make BDM the display manager (disables the current one). The risky
#               step; prints recovery instructions and asks for confirmation.
#               Options: --autologin USER  (set up autologin), --now (start now,
#               vs. the default of taking effect on reboot), -y/--yes (no prompt).
#   revert      Recovery: re-enable the previous display manager and start it.
#               Run this from a text console (Ctrl+Alt+F3) if login doesn't come up.
#   status      Show what's installed / active.
#   uninstall   Revert, then remove the BDM package.
#
# Compositor: uses the real bacak-compositor if found (BDM_COMPOSITOR=/path, or
# BDM_COMPOSITOR_SRC=/path/to/bacak — built if needed); otherwise the .deb's
# bundled weston wrapper is kept.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
STATE_DIR="/var/lib/bacak-display-manager"
PREV_DM_FILE="$STATE_DIR/previous-dm"
PREV_DDM_BAK="$STATE_DIR/default-display-manager.bak"

c_blue=$'\033[1;34m'; c_red=$'\033[1;31m'; c_yel=$'\033[1;33m'; c_rst=$'\033[0m'
log()  { printf '%s[bdm]%s %s\n' "$c_blue" "$c_rst" "$*"; }
warn() { printf '%s[bdm] %s%s\n' "$c_yel" "$*" "$c_rst"; }
die()  { printf '%s[bdm] %s%s\n' "$c_red" "$*" "$c_rst" >&2; exit 1; }

need_root() { [ "$(id -u)" -eq 0 ] || die "run as root:  sudo $0 $*"; }

confirm() {  # confirm "question"   (auto-yes with $YES)
    [ "${YES:-0}" = 1 ] && return 0
    printf '%s[bdm] %s [y/N] %s' "$c_yel" "$1" "$c_rst"
    read -r ans </dev/tty || ans=""
    case "$ans" in [yY]|[yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

# --- resolve the .deb ------------------------------------------------------
resolve_deb() {
    if [ -n "${BDM_DEB:-}" ]; then echo "$BDM_DEB"; return; fi
    # Always build from source when cargo is available to avoid stale packages.
    if command -v cargo >/dev/null; then
        log "building bacak-display-manager from source…" >&2
        ( cd "$REPO"
          cargo build --release -p bacak-display-manager --features system-pam
          cargo build --release -p bacak-greeter --features gui --bin bacak-greeter
          cargo deb --no-build -p bacak-display-manager ) >&2
    fi
    local d
    d="$(ls -t "$REPO"/target/debian/bacak-display-manager_*_amd64.deb 2>/dev/null | head -n1 || true)"
    echo "$d"
}

# --- resolve the real compositor (optional) --------------------------------
resolve_compositor() {
    if [ -n "${BDM_COMPOSITOR:-}" ] && [ -x "${BDM_COMPOSITOR}" ]; then echo "$BDM_COMPOSITOR"; return; fi
    local src; src="${BDM_COMPOSITOR_SRC:-$REPO/../bacak}"
    # Always build from source when cargo is available to avoid stale binaries.
    if [ -f "$src/Cargo.toml" ] && command -v cargo >/dev/null; then
        log "building bacak-compositor from source…" >&2
        ( cd "$src" && cargo build --release -p bacak-compositor --features udev ) >&2 || true
        [ -x "$src/target/release/bacak-compositor" ] && echo "$src/target/release/bacak-compositor"; return
    fi
    [ -x "$src/target/release/bacak-compositor" ] && echo "$src/target/release/bacak-compositor"
}

current_dm() {  # echoes the active DM unit base name, or empty
    local link; link="$(readlink -f /etc/systemd/system/display-manager.service 2>/dev/null || true)"
    [ -n "$link" ] && basename "$link" .service
}

cmd_install() {
    need_root install
    # Install the .deb only when BDM isn't already present. When this script runs
    # as the packaged `/usr/bin/bacak-dm-setup`, the package is obviously already
    # installed, so we just ensure the runtime deps + compositor + session.
    if ! command -v bacak-display-manager >/dev/null; then
        local deb; deb="$(resolve_deb)"
        [ -n "$deb" ] && [ -f "$deb" ] || die "BDM package not found (set BDM_DEB=/path, or run from the source tree)"
        log "installing package: $deb"
        apt-get install -y "$deb" weston dbus || { dpkg -i "$deb" || true; apt-get -f install -y; }
    else
        log "bacak-display-manager already installed; ensuring runtime deps (weston, dbus)"
        apt-get install -y weston dbus >/dev/null 2>&1 || true
    fi

    mkdir -p "$STATE_DIR"

    local weston_fb=/usr/share/bacak-display-manager/bacak-compositor-weston
    if [ -n "${BDM_COMPOSITOR:-}" ] && [ -x "${BDM_COMPOSITOR}" ]; then
        log "installing compositor (BDM_COMPOSITOR) → /usr/bin/bacak-compositor"
        install -m755 "$BDM_COMPOSITOR" /usr/bin/bacak-compositor
    elif [ -x /usr/bin/bacak-compositor ] && ! file -b /usr/bin/bacak-compositor | grep -qi 'shell script'; then
        log "real compositor already present at /usr/bin/bacak-compositor — keeping it"
    else
        local comp; comp="$(resolve_compositor || true)"
        if [ -n "$comp" ] && [ -x "$comp" ]; then
            log "installing real compositor → /usr/bin/bacak-compositor"
            install -m755 "$comp" /usr/bin/bacak-compositor
        elif [ -x "$weston_fb" ]; then
            warn "no real compositor — falling back to the bundled weston wrapper"
            sed -i "s#^compositor[[:space:]]*=.*#compositor   = \"$weston_fb\"#" /etc/bacak-display-manager.conf
            log "set [daemon] compositor = $weston_fb (install the real compositor for production)"
        else
            warn "no compositor available — set BDM_COMPOSITOR or install the bacak compositor package"
        fi
    fi

    if [ ! -x /usr/bin/bacak-session ]; then
        log "installing /usr/bin/bacak-session wrapper"
        cat > /usr/bin/bacak-session <<'EOF'
#!/bin/sh
# Bacak desktop session: Wayland text-input IM, then the compositor.
export GTK_IM_MODULE=wayland QT_IM_MODULE=wayland XMODIFIERS=@im=none
exec /usr/bin/bacak-compositor "$@"
EOF
        chmod 0755 /usr/bin/bacak-session
    fi

    systemctl daemon-reload 2>/dev/null || true
    log "installed. Your display manager is unchanged."
    log "Next:  sudo $0 enable   (switch to BDM)   — read the recovery note first."
}

set_autologin() {  # set_autologin USER
    local u="$1" conf=/etc/bacak-display-manager.conf
    [ -f "$conf" ] || die "$conf not found (install first)"
    sed -i \
        -e 's/^enabled[[:space:]]*=[[:space:]]*false/enabled = true/' \
        -e "s/^#[[:space:]]*user[[:space:]]*=.*/user = \"$u\"/" \
        -e 's/^#[[:space:]]*session[[:space:]]*=.*/session = "bacak"/' \
        "$conf"
    log "autologin enabled for '$u' (session=bacak)"
}

cmd_enable() {
    need_root enable
    local autologin="" now=0
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --autologin) autologin="${2:?--autologin needs a user}"; shift 2 ;;
            --now) now=1; shift ;;
            -y|--yes) YES=1; shift ;;
            *) die "unknown option: $1" ;;
        esac
    done
    command -v bacak-display-manager >/dev/null || die "BDM not installed; run:  sudo $0 install"

    local prev; prev="$(current_dm || true)"
    echo
    warn "================ READ THIS FIRST (recovery) ================"
    warn "Switching your display manager is risky. If the login screen"
    warn "doesn't appear, recover from a text console:"
    warn "    1) press Ctrl+Alt+F3 and log in"
    warn "    2) run:  sudo $0 revert"
    warn "Keep ${prev:-your current DM} installed; do NOT remove it yet."
    warn "==========================================================="
    echo
    confirm "Make BDM the display manager${prev:+ (replacing $prev)}?" || { log "aborted."; return 0; }

    [ -n "$autologin" ] && set_autologin "$autologin"

    mkdir -p "$STATE_DIR"
    if [ -n "$prev" ]; then
        echo "$prev" > "$PREV_DM_FILE"
        [ -f /etc/X11/default-display-manager ] && cp -a /etc/X11/default-display-manager "$PREV_DDM_BAK"
        log "remembering previous DM: $prev"
        systemctl disable "$prev.service" 2>/dev/null || true
    fi
    rm -f /etc/systemd/system/display-manager.service
    # Remove any previous DM's graphical.target want (e.g. one a prior `revert`
    # created) so graphical.target pulls BDM only — not two DMs.
    rm -f /etc/systemd/system/graphical.target.wants/display-manager.service
    systemctl enable bacak-display-manager.service
    systemctl daemon-reload
    echo /usr/bin/bacak-display-manager > /etc/X11/default-display-manager
    log "BDM enabled as the display manager."

    if [ "$now" = 1 ]; then
        warn "starting BDM now — this will end your current graphical session."
        confirm "Stop ${prev:-current DM} and start BDM right now?" || { log "enabled; reboot to apply."; return 0; }
        [ -n "$prev" ] && systemctl stop "$prev.service" 2>/dev/null || true
        loginctl terminate-seat seat0 2>/dev/null || true
        systemctl start bacak-display-manager.service
    else
        log "Reboot to apply:  sudo reboot"
    fi
}

cmd_revert() {
    need_root revert
    local prev; prev="$(cat "$PREV_DM_FILE" 2>/dev/null || true)"
    [ -n "$prev" ] || { prev=gdm3; warn "no saved previous DM; defaulting to gdm3"; }
    log "reverting to display manager: $prev"
    systemctl disable bacak-display-manager.service 2>/dev/null || true
    systemctl stop bacak-display-manager.service 2>/dev/null || true
    rm -f /etc/systemd/system/display-manager.service

    # Recreate the previous DM's display-manager.service and make graphical.target
    # pull it. This works even for 'static' units (e.g. gdm.service has no
    # [Install] section, so `systemctl enable` alone wouldn't start it at boot).
    local frag; frag="$(systemctl show "$prev.service" -p FragmentPath --value 2>/dev/null)"
    [ -n "$frag" ] && [ -f "$frag" ] || frag="/usr/lib/systemd/system/$prev.service"
    if [ -f "$frag" ]; then
        ln -sf "$frag" /etc/systemd/system/display-manager.service
        mkdir -p /etc/systemd/system/graphical.target.wants
        ln -sf "$frag" /etc/systemd/system/graphical.target.wants/display-manager.service
    else
        warn "could not find the $prev unit file ($frag) — recovery may need manual steps"
    fi
    systemctl enable "$prev.service" 2>/dev/null || true   # best-effort (alias units)
    systemctl daemon-reload

    if [ -f "$PREV_DDM_BAK" ]; then
        cp -a "$PREV_DDM_BAK" /etc/X11/default-display-manager
    else
        local p; p="$(command -v "$prev" || echo "/usr/sbin/$prev")"
        echo "$p" > /etc/X11/default-display-manager
    fi
    systemctl start "$prev.service" 2>/dev/null || warn "could not start $prev — reboot to recover"
    log "reverted to $prev. If you were at a black screen, log in again now."
}

cmd_status() {
    echo "installed:   $(command -v bacak-display-manager >/dev/null && echo yes || echo no)"
    echo "active DM:   $(current_dm || echo '<none>')"
    local en; en="$(systemctl is-enabled bacak-display-manager 2>/dev/null || true)"
    echo "BDM enabled: ${en:-no}"
    echo "compositor:  $(ls -l /usr/bin/bacak-compositor 2>/dev/null | awk '{print $5, "bytes"}' || echo missing) $(file -b /usr/bin/bacak-compositor 2>/dev/null | grep -qi 'shell script' && echo '(weston wrapper)' || echo '(binary)')"
    echo "session:     $([ -x /usr/bin/bacak-session ] && echo present || echo missing)"
    echo "saved prev:  $(cat "$PREV_DM_FILE" 2>/dev/null || echo '<none>')"
    grep -qE '^enabled[[:space:]]*=[[:space:]]*true' /etc/bacak-display-manager.conf 2>/dev/null \
        && echo "autologin:   on ($(grep -E '^user' /etc/bacak-display-manager.conf | head -1))" \
        || echo "autologin:   off"
}

cmd_uninstall() {
    need_root uninstall
    confirm "Revert to the previous DM and remove the BDM package?" || { log "aborted."; return 0; }
    cmd_revert || true
    apt-get remove -y bacak-display-manager 2>/dev/null || dpkg -r bacak-display-manager || true
    log "BDM removed. (Run 'apt purge bacak-display-manager' to drop config too.)"
}

case "${1:-}" in
    install)   shift; cmd_install "$@" ;;
    enable)    shift; cmd_enable "$@" ;;
    revert)    shift; cmd_revert "$@" ;;
    status)    shift; cmd_status "$@" ;;
    uninstall) shift; cmd_uninstall "$@" ;;
    ""|-h|--help)
        sed -n '2,24p' "$0" ;;
    *) die "unknown subcommand '$1' (try: install | enable | revert | status | uninstall)" ;;
esac
