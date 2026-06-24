# BDM VM smoke test

Automated proof for [`docs/PROTOTYPE.md`](../../docs/PROTOTYPE.md) **path B**: it
boots a throwaway Debian VM with a **virtio-gpu seat**, installs the BDM `.deb` +
weston, makes BDM the display manager, and asserts — from inside the VM via
`loginctl` — that an **active logind `greeter` session** exists on a graphical
`seat0` and that **weston** started as the greeter user.

This runs in a VM on purpose: it needs root, a real seat/VT and a DRM device,
none of which a container or the dev box provides, and it must not touch the
host's display manager.

## Prerequisites (host)

```sh
sudo apt install qemu-system-x86 qemu-utils cloud-image-utils
# (xorriso or genisoimage also work instead of cloud-image-utils)
```

A Debian cloud image is downloaded on first run (or pass `--image`). KVM is used
automatically when `/dev/kvm` exists; otherwise it falls back to (slow) TCG.

## Run

```sh
# Uses the newest target/debian/*.deb, or builds one (system-pam + gui) if none.
packaging/vm/smoke-test.sh

# Or be explicit:
packaging/vm/smoke-test.sh \
    --deb target/debian/bacak-display-manager_0.1.0-1_amd64.deb \
    --image /path/to/debian-13-genericcloud-amd64.qcow2 \
    --timeout 600
```

Exit codes: `0` PASS · `1` FAIL · `2` setup error (missing tools/image) · `3`
timeout. The guest's full check output (and the `SMOKE_RESULT:` line) is printed
at the end and saved in the run's `serial.log`.

### Compositor (real by default)

The smoke test uses the **real `bacak-compositor`** by default and asserts it
takes DRM master and hosts the greeter via `$BACAK_STARTUP`. It is resolved as:

1. `BDM_COMPOSITOR` — path to a prebuilt `bacak-compositor` binary, else
2. `BDM_COMPOSITOR_SRC/target/release/bacak-compositor` (default
   `BDM_COMPOSITOR_SRC=../bacak`), else
3. built from `$BDM_COMPOSITOR_SRC` via `cargo build --release -p bacak-compositor
   --features udev`.

If none is available it falls back to the bundled **weston** wrapper (still a
valid logind-session test). Set `BDM_ALLOW_WESTON_FALLBACK=0` to require the real
compositor and fail otherwise.

```sh
# Force a specific compositor binary:
BDM_COMPOSITOR=/path/to/bacak-compositor packaging/vm/smoke-test.sh
# Point at the compositor source workspace to build it:
BDM_COMPOSITOR_SRC=/path/to/bacak packaging/vm/smoke-test.sh
```

Because the real compositor needs a GL context, the VM is launched with
`virtio-vga-gl` + qemu `egl-headless` (virgl 3D via the host's
`libvirglrenderer`).

## What it checks (inside the VM)

| Check | Expectation |
|-------|-------------|
| `systemctl is-active bacak-display-manager` | active |
| `loginctl show-seat seat0 -p CanGraphical` | `yes` (virtio-gpu DRM) |
| a session with `Class=greeter` exists | yes |
| that session: `Seat` / `Active` / `Type` / `Name` | `seat0` / `yes` / `wayland` / `bacak-greeter` |
| compositor running as `bacak-greeter` | running |
| daemon journal | contains "registering logind …" |
| **real compositor:** process running as `bacak-greeter` | yes |
| **real compositor:** holds a DRM device (`/proc/PID/fd` → `/dev/dri/…`) | DRM master |
| **real compositor:** hosts the greeter client (`$BACAK_STARTUP`) | greeter running |

The last three run only when the real compositor is used (the default); with the
weston fallback they are skipped.

## Files

- `smoke-test.sh` — host orchestrator (qemu, cloud-init seed, verdict capture).
- `guest-check.sh` — runs inside the VM (install, enable, assert, power off).

## Troubleshooting

The guest writes its verdict to the 9p **share** (`<workdir>/share/smoke-result.txt`
and `smoke.log`), so a verdict no longer depends on the VM's serial console. If
the run is **inconclusive / timeout**:

- **Only GRUB lines in `serial.log`, no `smoke.log` on the share** → the VM
  didn't reach/run cloud-init. Check:
  - `/dev/kvm` is usable (`ls -l /dev/kvm`); without it TCG is *very* slow —
    raise `BDM_TIMEOUT=1800` or enable KVM.
  - the base image is a Debian **genericcloud** qcow2 (NoCloud `cidata` seed is
    attached as a CD-ROM; some images need cloud-init present).
- **`smoke.log` exists but no PASS** → read it; a `FAIL:` line names the failed
  assertion (e.g. `seat0 is graphical` ⇒ no DRM: ensure `-device virtio-gpu-pci`
  and a kernel with `virtio_gpu`).
- Inspect manually: `BDM_TIMEOUT=2400 BDM_WORKDIR=$PWD/vmrun packaging/vm/smoke-test.sh`
  then watch `vmrun/share/smoke.log`.

## Two-phase (full graphics path)

The minimal Debian *cloud* kernel ships no GPU/DRM driver, so on first boot the
seat is not graphical. `guest-check.sh` handles this in two phases:

1. **Phase 1** (cloud kernel): installs the **generic** kernel (`linux-image-amd64`,
   which has `virtio_gpu`) + weston + the `.deb`, drops the cloud kernel so GRUB
   boots generic, then reboots.
2. **Phase 2** (generic kernel, `/dev/dri` present): starts BDM and asserts an
   active **graphical** logind greeter session on `seat0` with weston on the
   **drm backend**.

(The qemu invocation uses `-vga virtio`, giving a VGA-compatible virtio-gpu that
GRUB can boot *and* that the generic kernel exposes as `/dev/dri/card0`.)

## Status

> **Verified.** Executed end-to-end on a Debian 13 (trixie) genericcloud VM under
> qemu/KVM: `SMOKE_RESULT: PASS` with the generic kernel — `/dev/dri` present,
> `seat0` graphical, an active `wayland`/`greeter` logind session owned by
> `bacak-greeter`, and weston on the drm backend. This closes
> [docs/PROTOTYPE.md](../../docs/PROTOTYPE.md) path B.
>
> Surfaced and fixed a real bug along the way: std applies `Command::uid/gid`
> *before* `pre_exec`, so the greeter spawn dropped privileges too early and
> `initgroups`/PAM failed with EPERM — now all privilege steps run in one
> correctly-ordered `pre_exec`.
