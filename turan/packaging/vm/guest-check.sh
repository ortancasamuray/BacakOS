#!/bin/sh
# guest-check.sh — runs *inside* the VM to verify BDM end to end. Two phases,
# because the minimal Debian cloud kernel has no GPU/DRM driver:
#
#   PHASE 1 (cloud kernel, no /dev/dri): install the generic kernel (which has
#           virtio_gpu) + weston + the BDM .deb, drop the cloud kernel so GRUB
#           boots generic, arrange to re-run after reboot, then reboot.
#   PHASE 2 (generic kernel, /dev/dri present): make BDM the DM, start it, and
#           assert an *active graphical* logind greeter session on seat0 with
#           weston on the drm backend. Emit SMOKE_RESULT and power off.
#
# All output goes to the serial console (the host captures it). Uploaded into
# the VM by smoke-test.sh; not run on the host.

set -u
exec >/dev/console 2>&1
export SYSTEMD_PAGER=cat PAGER=cat SYSTEMD_COLORS=0 DEBIAN_FRONTEND=noninteractive

have_drm() { ls /dev/dri/card* >/dev/null 2>&1; }

# ===================== PHASE 1 =====================
if ! have_drm; then
    echo "=================== BDM SMOKE: PHASE 1 (provision + reboot) ==================="
    date
    echo "kernel: $(uname -r)  (no DRM yet)"

    mkdir -p /mnt/seed
    mount -o ro /dev/sr0 /mnt/seed 2>/dev/null \
        || mount -o ro /dev/cdrom /mnt/seed 2>/dev/null || true
    DEB="$(ls -t /mnt/seed/*.deb 2>/dev/null | head -n1 || true)"
    echo "package: ${DEB:-<none on seed ISO>}"

    apt-get update -y || true
    echo "--- installing generic kernel (DRM) + weston + BDM + compositor deps ---"
    # weston pulls most of it; the rest are the real compositor's runtime libs
    # (libseat/libinput/gbm/egl/gles/mesa-gl-driver/xkbcommon/drm) + Xwayland.
    # shellcheck disable=SC2086
    apt-get install -y --no-install-recommends \
        linux-image-amd64 weston xwayland \
        libseat1 libinput10 libgbm1 libegl1 libgles2 libgl1-mesa-dri \
        libxkbcommon0 libdrm2 libexpat1 fontconfig \
        ${DEB:+$DEB} \
        || echo "WARN: some installs failed"

    # Make GRUB boot the generic kernel by dropping the DRM-less cloud kernel.
    apt-get purge -y 'linux-image-*-cloud-amd64' linux-image-cloud-amd64 2>/dev/null || true
    update-grub 2>/dev/null || true

    # cloud-init runcmd only fires once, so re-run this script after reboot.
    cat >/etc/systemd/system/bdm-smoke.service <<'EOF'
[Unit]
Description=BDM smoke test phase 2
After=multi-user.target systemd-logind.service systemd-user-sessions.service
[Service]
Type=oneshot
ExecStart=/usr/local/bin/bdm-guest-check.sh
TimeoutStartSec=300
[Install]
WantedBy=multi-user.target
EOF
    systemctl enable bdm-smoke.service
    sync
    echo "--- rebooting into the generic kernel ---"
    systemctl reboot
    exit 0
fi

# ===================== PHASE 2 =====================
echo "=================== BDM SMOKE TEST (guest) ==================="
date
echo "kernel: $(uname -r)  (DRM present)"
systemctl disable bdm-smoke.service 2>/dev/null || true   # run once

PASS=1
note_fail() { echo "FAIL: $1"; PASS=0; }
check()     { if eval "$2"; then echo "PASS: $1"; else note_fail "$1"; fi; }

# Install the compositor at /usr/bin/bacak-compositor. The .deb no longer ships
# one there (that path belongs to the real compositor's own package), so the
# guest provides it: the real binary from the seed, else the bundled weston
# wrapper the .deb installs at /usr/share/bacak-display-manager/.
COMP=weston
mkdir -p /mnt/seed
mount -o ro /dev/sr0 /mnt/seed 2>/dev/null || mount -o ro /dev/cdrom /mnt/seed 2>/dev/null || true
if [ -f /mnt/seed/bacak-compositor ]; then
    install -m755 /mnt/seed/bacak-compositor /usr/bin/bacak-compositor
    COMP=real
    echo "installed real bacak-compositor ($(du -h /usr/bin/bacak-compositor | cut -f1))"
    ldd /usr/bin/bacak-compositor 2>&1 | grep -i "not found" && echo "WARN: missing libs above" || echo "compositor libs OK"
elif [ -x /usr/share/bacak-display-manager/bacak-compositor-weston ]; then
    install -m755 /usr/share/bacak-display-manager/bacak-compositor-weston /usr/bin/bacak-compositor
    echo "no real compositor on seed — using the bundled weston wrapper"
else
    echo "WARN: no compositor available"
fi

# The .deb no longer ships a session entry (desktop packages own it), so create
# the Bacak session here: Exec=bacak-session → the compositor.
mkdir -p /usr/share/wayland-sessions
cat >/usr/share/wayland-sessions/bacak.desktop <<'EOF'
[Desktop Entry]
Name=Bacak Desktop
Comment=The Bacak Wayland desktop session
Exec=bacak-session
Type=Application
DesktopNames=Bacak
EOF
cat >/usr/bin/bacak-session <<'EOF'
#!/bin/sh
export GTK_IM_MODULE=wayland QT_IM_MODULE=wayland XMODIFIERS=@im=none
exec /usr/bin/bacak-compositor "$@"
EOF
chmod 0755 /usr/bin/bacak-session

echo "--- enabling bacak-display-manager (compositor=$COMP) ---"
systemctl disable gdm3 2>/dev/null || true
systemctl enable bacak-display-manager || note_fail "enable bacak-display-manager"
systemctl start  bacak-display-manager || note_fail "start bacak-display-manager"

echo "--- waiting for greeter session (up to 60s) ---"
GS=""
i=0
while [ "$i" -lt 60 ]; do
    for s in $(loginctl --no-legend list-sessions 2>/dev/null | awk '{print $1}'); do
        if [ "$(loginctl show-session "$s" -p Class --value 2>/dev/null)" = "greeter" ]; then
            GS="$s"; break
        fi
    done
    [ -n "$GS" ] && break
    i=$((i + 1)); sleep 1
done
echo "greeter session id: ${GS:-<none>}"

echo "--- checks ---"
check "/dev/dri present (DRM device)" \
      'ls /dev/dri/card* >/dev/null 2>&1'
check "seat0 is graphical (DRM present)" \
      '[ "$(loginctl show-seat seat0 -p CanGraphical --value 2>/dev/null)" = "yes" ]'
check "service is active" \
      'systemctl is-active --quiet bacak-display-manager'
check "greeter logind session exists" \
      '[ -n "$GS" ]'
if [ -n "$GS" ]; then
    check "greeter session is on seat0" \
          '[ "$(loginctl show-session "$GS" -p Seat --value)" = "seat0" ]'
    check "greeter session is active" \
          '[ "$(loginctl show-session "$GS" -p Active --value)" = "yes" ]'
    check "greeter session type is wayland" \
          '[ "$(loginctl show-session "$GS" -p Type --value)" = "wayland" ]'
    check "greeter session user is bacak-greeter" \
          '[ "$(loginctl show-session "$GS" -p Name --value)" = "bacak-greeter" ]'
fi
check "compositor runs as bacak-greeter" \
      'pgrep -u bacak-greeter -f "bacak-compositor|weston" >/dev/null'
check "daemon logged greeter-session registration" \
      'journalctl -u bacak-display-manager -b | grep -q "registering logind"'

if [ "$COMP" = "real" ]; then
    # Full real path, verified FUNCTIONALLY (not via log strings): BDM greeter
    # logind session → bacak-compositor takes DRM master → hosts the greeter via
    # $BACAK_STARTUP. The compositor only reaches the socket/startup stage after
    # it has opened the seat + GPU, so a running greeter client proves the chain.
    # NB: "bacak-compositor" is 16 chars, so its /proc comm is truncated to 15
    # ("bacak-composito") — match on the full command line instead.
    CPID="$(pgrep -u bacak-greeter -f '/usr/bin/bacak-compositor' | head -n1)"
    echo "compositor pid: ${CPID:-<none>}"
    check "real compositor running as bacak-greeter" \
          '[ -n "$CPID" ]'
    check "real compositor holds a DRM device (DRM master)" \
          '[ -n "$CPID" ] && ls -l /proc/$CPID/fd 2>/dev/null | grep -q /dev/dri/'
    check "compositor hosts the greeter client (\$BACAK_STARTUP)" \
          'pgrep -u bacak-greeter -x bacak-greeter >/dev/null'
    # Now that RUST_LOG surfaces them, also confirm from the compositor's logs.
    if journalctl -u bacak-display-manager -b | grep -qi "opened libseat session"; then
        echo "INFO: compositor logged: opened libseat session (seat granted)"
    fi
    if journalctl -u bacak-display-manager -b | grep -qi "spawning session startup"; then
        echo "INFO: compositor logged: spawning session startup client (\$BACAK_STARTUP)"
    fi
else
    if journalctl -b 2>/dev/null | grep -qiE "drm-backend|DRM: |using GPU"; then
        echo "INFO: weston is on the drm backend (real seat)"
    fi
fi

# --- autologin sub-test: BDM autologin → real user session → compositor on seat
# This verifies the post-login handoff (the piece the greeter test doesn't reach):
# the user's bacak-session → bacak-compositor takes DRM master via a logind
# 'user' session. Only meaningful with the real compositor.
if [ "$COMP" = "real" ]; then
    echo "=================== AUTOLOGIN SUB-TEST ==================="
    AUSER=bacaktest
    systemctl stop bacak-display-manager || true
    if ! id "$AUSER" >/dev/null 2>&1; then
        useradd -m -s /bin/bash "$AUSER" || note_fail "create $AUSER"
        echo "$AUSER:test123" | chpasswd || true
    fi
    # bacak-session + bacak.desktop were created earlier in phase 2.
    # Enable autologin for the test user.
    sed -i \
        -e 's/^enabled[[:space:]]*=[[:space:]]*false/enabled = true/' \
        -e "s/^#[[:space:]]*user[[:space:]]*=.*/user = \"$AUSER\"/" \
        -e 's/^#[[:space:]]*session[[:space:]]*=.*/session = "bacak"/' \
        /etc/bacak-display-manager.conf
    echo "--- [autologin] config ---"; grep -A4 '^\[autologin\]' /etc/bacak-display-manager.conf
    systemctl start bacak-display-manager || note_fail "start BDM (autologin)"

    echo "--- waiting for the autologin user session (up to 40s) ---"
    US=""
    i=0
    while [ "$i" -lt 40 ]; do
        for s in $(loginctl --no-legend list-sessions 2>/dev/null | awk '{print $1}'); do
            if [ "$(loginctl show-session "$s" -p Name --value 2>/dev/null)" = "$AUSER" ] &&
               [ "$(loginctl show-session "$s" -p Class --value 2>/dev/null)" = "user" ]; then
                US="$s"; break
            fi
        done
        [ -n "$US" ] && break
        i=$((i + 1)); sleep 1
    done
    echo "autologin user session id: ${US:-<none>}"

    check "autologin: user logind session exists" '[ -n "$US" ]'
    if [ -n "$US" ]; then
        check "autologin: session is on seat0" \
              '[ "$(loginctl show-session "$US" -p Seat --value)" = "seat0" ]'
        check "autologin: session is active" \
              '[ "$(loginctl show-session "$US" -p Active --value)" = "yes" ]'
        check "autologin: session type is wayland" \
              '[ "$(loginctl show-session "$US" -p Type --value)" = "wayland" ]'
    fi
    APID="$(pgrep -u "$AUSER" -f '/usr/bin/bacak-compositor' | head -n1)"
    echo "autologin compositor pid: ${APID:-<none>}"
    check "autologin: compositor runs as $AUSER" \
          '[ -n "$APID" ]'
    check "autologin: compositor holds a DRM device (seat handoff)" \
          '[ -n "$APID" ] && ls -l /proc/$APID/fd 2>/dev/null | grep -q /dev/dri/'

    echo "----- autologin loginctl -----"; loginctl --no-legend 2>&1 || true
    echo "----- autologin bdm journal -----"; journalctl -u bacak-display-manager -b --no-pager 2>&1 | tail -n 20 || true
fi

# --- interactive sub-test: real PASSWORD login via the headless test-greeter
# Verifies the greeter password path: headless client → daemon PAM authenticate
# (pam_unix, real password) → user logind session opened in the child pre_exec →
# user's compositor takes the seat. The headless greeter replaces the GUI greeter
# (it can't be typed into in CI) and is spawned by the compositor via $BACAK_STARTUP.
if [ "$COMP" = "real" ] && [ -f /mnt/seed/headless_login ]; then
    echo "=================== INTERACTIVE (password) SUB-TEST ==================="
    IUSER=bacaktest
    id "$IUSER" >/dev/null 2>&1 || useradd -m -s /bin/bash "$IUSER"
    echo "$IUSER:test123" | chpasswd || true
    systemctl stop bacak-display-manager || true
    # Compositors (greeter and user) escape into their own logind session scopes,
    # so they survive `systemctl stop` and keep holding the seat. Nuke every
    # session on seat0 and wait until no compositor remains, so the seat is truly
    # free for a fresh password login. (Real-world: switching DM off a live
    # session needs the session terminated.)
    loginctl terminate-seat seat0 2>/dev/null || true
    pkill -f /usr/bin/bacak-compositor 2>/dev/null || true
    j=0
    while pgrep -f /usr/bin/bacak-compositor >/dev/null 2>&1 && [ "$j" -lt 20 ]; do
        sleep 1; j=$((j + 1))
    done
    echo "seat cleared after ${j}s; compositors left: $(pgrep -cf /usr/bin/bacak-compositor || echo 0)"
    # Back to greeter mode (disable autologin).
    sed -i 's/^enabled[[:space:]]*=[[:space:]]*true/enabled = false/' /etc/bacak-display-manager.conf
    grep -E '^enabled' /etc/bacak-display-manager.conf
    # Replace the GUI greeter with the headless one; it reads creds from this file
    # (must be readable by the unprivileged greeter user).
    install -m755 /mnt/seed/headless_login /usr/bin/bacak-greeter
    printf '%s\n%s\n%s\n' "$IUSER" "test123" "bacak" > /etc/bdm-test-login
    chmod 644 /etc/bdm-test-login
    systemctl start bacak-display-manager || note_fail "start BDM (interactive)"

    echo "--- waiting for the password-login user session (up to 45s) ---"
    IS=""
    i=0
    while [ "$i" -lt 45 ]; do
        for s in $(loginctl --no-legend list-sessions 2>/dev/null | awk '{print $1}'); do
            if [ "$(loginctl show-session "$s" -p Name --value 2>/dev/null)" = "$IUSER" ] &&
               [ "$(loginctl show-session "$s" -p Class --value 2>/dev/null)" = "user" ]; then
                IS="$s"; break
            fi
        done
        [ -n "$IS" ] && break
        i=$((i + 1)); sleep 1
    done
    echo "interactive user session id: ${IS:-<none>}"

    check "interactive: password authenticated via PAM (pam_unix)" \
          'journalctl -u bacak-display-manager -b | grep -q "authentication succeeded for"'
    check "interactive: user logind session exists" '[ -n "$IS" ]'
    if [ -n "$IS" ]; then
        check "interactive: session on seat0 / active / wayland" \
              '[ "$(loginctl show-session "$IS" -p Seat --value)" = seat0 ] && [ "$(loginctl show-session "$IS" -p Active --value)" = yes ] && [ "$(loginctl show-session "$IS" -p Type --value)" = wayland ]'
    fi
    IPID="$(pgrep -u "$IUSER" -f '/usr/bin/bacak-compositor' | head -n1)"
    echo "interactive compositor pid: ${IPID:-<none>}"
    check "interactive: compositor runs as $IUSER" '[ -n "$IPID" ]'
    check "interactive: compositor holds a DRM device (seat handoff)" \
          '[ -n "$IPID" ] && ls -l /proc/$IPID/fd 2>/dev/null | grep -q /dev/dri/'

    echo "interactive: service active? $(systemctl is-active bacak-display-manager 2>&1)"
    echo "----- interactive loginctl -----"; loginctl --no-legend 2>&1 || true
    echo "----- interactive bdm journal -----"; journalctl -u bacak-display-manager -b --no-pager 2>&1 | tail -n 45 || true
fi

# --- diagnostics ---
echo "----- /dev/dri -----";       ls -l /dev/dri 2>&1 || true
echo "----- drm modules -----";    lsmod | grep -iE 'drm|virtio_gpu' || echo "none"
echo "----- loginctl -----";       loginctl --no-legend 2>&1 || true
echo "----- seat-status seat0 -----"; loginctl seat-status seat0 2>&1 | head -n 20 || true
echo "----- weston journal -----"; journalctl -b _COMM=weston --no-pager 2>&1 | tail -n 15 || true
echo "----- bdm journal -----";    journalctl -u bacak-display-manager -b --no-pager 2>&1 | tail -n 20 || true

echo "============================================================="
if [ "$PASS" -eq 1 ]; then
    echo "SMOKE_RESULT: PASS"
else
    echo "SMOKE_RESULT: FAIL"
fi
echo "============================================================="
echo "SMOKE_DONE"
sync
poweroff -f 2>/dev/null || systemctl poweroff -f 2>/dev/null || halt -f
