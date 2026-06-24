#!/usr/bin/env bash
# smoke-test.sh — boot a throwaway Debian VM with a virtio-gpu seat, install the
# BDM .deb + weston, enable BDM, and assert (via loginctl, inside the VM) that an
# *active* logind 'greeter' session exists on a graphical seat0 and that weston
# started. Prints the guest's verdict and exits 0 (PASS) / 1 (FAIL) / 2 (setup
# error) / 3 (timeout).
#
# This is the bare-metal proof for docs/PROTOTYPE.md path B, run in a VM so it is
# safe and automatable. It needs a Debian cloud image and qemu on the host.
#
# Usage:
#   packaging/vm/smoke-test.sh [--deb PATH] [--image QCOW2] [--timeout SECONDS]
#
# Env overrides: BDM_DEB, BDM_IMAGE, BDM_MEM (MiB), BDM_TIMEOUT, BDM_WORKDIR.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"

DEB="${BDM_DEB:-}"
IMAGE="${BDM_IMAGE:-}"
MEM="${BDM_MEM:-2048}"
TIMEOUT="${BDM_TIMEOUT:-600}"
WORKDIR="${BDM_WORKDIR:-$(mktemp -d /tmp/bdm-vm.XXXXXX)}"
IMAGE_URL="https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --deb) DEB="$2"; shift 2 ;;
        --image) IMAGE="$2"; shift 2 ;;
        --timeout) TIMEOUT="$2"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

log() { printf '\033[1;34m[smoke]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[smoke] %s\033[0m\n' "$*" >&2; exit 2; }

# --- preflight: required host tools ----------------------------------------
need_apt=()
command -v qemu-system-x86_64 >/dev/null || need_apt+=("qemu-system-x86")
command -v qemu-img           >/dev/null || need_apt+=("qemu-utils")
SEED_TOOL=""
for t in cloud-localds xorriso genisoimage; do
    if command -v "$t" >/dev/null; then SEED_TOOL="$t"; break; fi
done
[ -n "$SEED_TOOL" ] || need_apt+=("cloud-image-utils  # or xorriso/genisoimage")
if [ "${#need_apt[@]}" -gt 0 ]; then
    die "missing host tools — install: sudo apt install ${need_apt[*]}"
fi

# --- locate / build the .deb -----------------------------------------------
if [ -z "$DEB" ]; then
    DEB="$(ls -t "$ROOT"/target/debian/bacak-display-manager_*_amd64.deb 2>/dev/null | head -n1 || true)"
fi
if [ -z "$DEB" ] || [ ! -f "$DEB" ]; then
    log "no .deb found; building one (system-pam daemon + gui greeter)…"
    command -v cargo >/dev/null || die "no .deb and cargo not installed"
    ( cd "$ROOT"
      cargo build --release -p bacak-display-manager --features system-pam
      cargo build --release -p bacak-greeter --features gui --bin bacak-greeter
      cargo deb --no-build -p bacak-display-manager )
    DEB="$(ls -t "$ROOT"/target/debian/bacak-display-manager_*_amd64.deb | head -n1)"
fi
log "package: $DEB"

# --- obtain the base cloud image -------------------------------------------
# An explicit --image/BDM_IMAGE that doesn't exist yet is downloaded to that
# path (so CI can point it at a cached location).
[ -z "$IMAGE" ] && IMAGE="$WORKDIR/base.qcow2"
if [ ! -f "$IMAGE" ]; then
    log "downloading Debian cloud image → $IMAGE"
    mkdir -p "$(dirname "$IMAGE")"
    if command -v curl >/dev/null; then curl -fL "$IMAGE_URL" -o "$IMAGE";
    elif command -v wget >/dev/null; then wget -O "$IMAGE" "$IMAGE_URL";
    else die "no curl/wget to fetch image; pass --image PATH"; fi
fi
[ -f "$IMAGE" ] || die "base image not found: $IMAGE"

# --- assemble VM inputs -----------------------------------------------------
log "workdir: $WORKDIR"
SHARE="$WORKDIR/share"; mkdir -p "$SHARE"; cp "$DEB" "$SHARE/"

OVERLAY="$WORKDIR/overlay.qcow2"
qemu-img create -f qcow2 -F qcow2 -b "$(readlink -f "$IMAGE")" "$OVERLAY" 12G >/dev/null

# cloud-init NoCloud seed: embed the guest check script (base64) into user-data.
GUEST_B64="$(base64 -w0 "$HERE/guest-check.sh")"
cat > "$WORKDIR/meta-data" <<EOF
instance-id: bdm-smoke-$(date +%s)
local-hostname: bdm-smoke
EOF
cat > "$WORKDIR/user-data" <<EOF
#cloud-config
ssh_pwauth: false
growpart: { mode: auto }
write_files:
  - path: /usr/local/bin/bdm-guest-check.sh
    permissions: '0755'
    encoding: b64
    content: ${GUEST_B64}
runcmd:
  - [ sh, -c, "/usr/local/bin/bdm-guest-check.sh" ]
EOF

# Build the NoCloud seed ISO and embed the .deb on it (read in-guest from the
# same disc cloud-init boots from — avoids 9p, which minimal cloud kernels lack).
SEED="$WORKDIR/seed.iso"
cp "$DEB" "$WORKDIR/bdm.deb"
ISO_FILES="$WORKDIR/user-data $WORKDIR/meta-data $WORKDIR/bdm.deb"

# The real bacak-compositor is the DEFAULT compositor for the smoke test. Resolve
# it in order: explicit BDM_COMPOSITOR binary → prebuilt in the source workspace
# (BDM_COMPOSITOR_SRC, default ../bacak) → build it from that source. The weston
# wrapper is only a fallback, and only when BDM_ALLOW_WESTON_FALLBACK=1 (default).
COMPOSITOR=""
if [ -n "${BDM_COMPOSITOR:-}" ] && [ -f "${BDM_COMPOSITOR}" ]; then
    COMPOSITOR="$BDM_COMPOSITOR"
else
    COMP_SRC="${BDM_COMPOSITOR_SRC:-$(cd "$ROOT/.." 2>/dev/null && pwd)/bacak}"
    if [ -f "$COMP_SRC/target/release/bacak-compositor" ]; then
        COMPOSITOR="$COMP_SRC/target/release/bacak-compositor"
    elif [ -f "$COMP_SRC/Cargo.toml" ] && command -v cargo >/dev/null; then
        log "building real bacak-compositor from $COMP_SRC (udev feature)…"
        if ( cd "$COMP_SRC" && cargo build --release -p bacak-compositor --features udev ); then
            COMPOSITOR="$COMP_SRC/target/release/bacak-compositor"
        fi
    fi
fi

# Embed the headless test-greeter (drives interactive password login over IPC),
# built alongside the workspace, for the interactive sub-test.
HEADLESS="$ROOT/target/release/examples/headless_login"
if [ -f "$HEADLESS" ]; then
    cp "$HEADLESS" "$WORKDIR/headless_login"
    ISO_FILES="$ISO_FILES $WORKDIR/headless_login"
fi

if [ -n "$COMPOSITOR" ] && [ -f "$COMPOSITOR" ]; then
    cp "$COMPOSITOR" "$WORKDIR/bacak-compositor"
    ISO_FILES="$ISO_FILES $WORKDIR/bacak-compositor"
    log "compositor: real bacak-compositor — $COMPOSITOR ($(du -h "$COMPOSITOR" | cut -f1))"
elif [ "${BDM_ALLOW_WESTON_FALLBACK:-1}" = "1" ]; then
    log "WARNING: real bacak-compositor not found — falling back to the weston wrapper."
    log "         Set BDM_COMPOSITOR=/path/to/bacak-compositor or BDM_COMPOSITOR_SRC=/path/to/bacak."
else
    die "real bacak-compositor not found; set BDM_COMPOSITOR or BDM_COMPOSITOR_SRC (or BDM_ALLOW_WESTON_FALLBACK=1)"
fi
if command -v genisoimage >/dev/null; then ISO_TOOL=genisoimage
elif command -v mkisofs >/dev/null; then ISO_TOOL=mkisofs
elif command -v xorriso >/dev/null; then ISO_TOOL=xorriso
else die "need genisoimage/mkisofs/xorriso to build the seed ISO"; fi
# shellcheck disable=SC2086
case "$ISO_TOOL" in
    genisoimage|mkisofs)
        "$ISO_TOOL" -output "$SEED" -volid cidata -joliet -rock $ISO_FILES >/dev/null 2>&1 ;;
    xorriso)
        xorriso -as mkisofs -output "$SEED" -volid cidata -joliet -rock $ISO_FILES >/dev/null 2>&1 ;;
esac
log "seed ISO built via $ISO_TOOL"

# --- launch qemu ------------------------------------------------------------
# `virtio-vga-gl` + `egl-headless` give the guest a VGA-compatible virtio-gpu
# with **virgl 3D** (real GL/EGL/GLES via the host's libvirglrenderer) — GRUB
# boots, the generic kernel exposes /dev/dri/card0, and the compositor gets a
# hardware GL context. Falls back automatically to plain `-vga virtio` (2D, KMS
# only) if virgl isn't usable. The guest reports its verdict on serial (ttyS0).
SERIAL="$WORKDIR/serial.log"; : > "$SERIAL"
ACCEL=(); [ -e /dev/kvm ] && ACCEL=(-enable-kvm -cpu host) || log "no /dev/kvm — using slow TCG (boot can take minutes)"

GPU=(-display egl-headless -device virtio-vga-gl)
if ! qemu-system-x86_64 -display help 2>/dev/null | grep -q egl-headless; then
    log "no egl-headless — falling back to 2D virtio-gpu (compositor may lack GL)"
    GPU=(-display none -vga virtio)
fi

log "booting VM (timeout ${TIMEOUT}s)… serial → $SERIAL ; gpu: ${GPU[*]}"
qemu-system-x86_64 \
    "${ACCEL[@]}" \
    -m "$MEM" -smp 2 \
    -drive file="$OVERLAY",if=virtio,format=qcow2 \
    -cdrom "$SEED" \
    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
    "${GPU[@]}" -serial file:"$SERIAL" -monitor none \
    & QEMU_PID=$!

cleanup() { kill "$QEMU_PID" 2>/dev/null || true; }
trap cleanup EXIT

# --- wait for the guest verdict (printed to the serial console) -------------
strip() { tr -d '\000' | sed 's/\r//g'; }
RESULT=""
elapsed=0
while kill -0 "$QEMU_PID" 2>/dev/null; do
    if strip < "$SERIAL" | grep -q "SMOKE_RESULT:" 2>/dev/null; then
        RESULT="$(strip < "$SERIAL" | grep -m1 "SMOKE_RESULT:")"; break
    fi
    if [ "$elapsed" -ge "$TIMEOUT" ]; then
        log "TIMEOUT after ${TIMEOUT}s"; break
    fi
    sleep 3; elapsed=$((elapsed + 3))
done

echo
log "------- guest output -------"
if strip < "$SERIAL" | grep -q "BDM SMOKE TEST (guest)"; then
    strip < "$SERIAL" | sed -n '/BDM SMOKE TEST (guest)/,/SMOKE_DONE/p' | sed 's/^/    /'
else
    log "(guest checks did not run; serial tail:)"
    strip < "$SERIAL" | tail -n 20 | sed 's/^/    /' || true
    log "Hints: ensure /dev/kvm is usable, the base image is a Debian genericcloud"
    log "qcow2, and cloud-init ran (NoCloud 'cidata')."
fi
echo

case "$RESULT" in
    *PASS*) log "RESULT: PASS ✅"; exit 0 ;;
    *FAIL*) log "RESULT: FAIL ❌ (see $SERIAL)"; exit 1 ;;
    *)      log "RESULT: inconclusive / timeout (see $SERIAL)"; exit 3 ;;
esac
