# Running BDM on weston (working prototype)

The real `bacak-compositor` isn't written yet. Until it is, BDM can use **weston**
as a drop-in compositor to host the greeter on a real Wayland seat. A small
wrapper, [`packaging/prototype/bacak-compositor`](../packaging/prototype/bacak-compositor),
makes weston behave the way the daemon expects.

## How the wrapper works

The daemon launches its compositor as:

```
bacak-compositor --greeter /usr/bin/bacak-greeter
```

The wrapper translates that into a weston launch that:

1. generates a weston config using the **kiosk shell** (greeter fills the screen);
2. uses weston's **`[autolaunch]`** to start the greeter, with `watch=true` so
   weston exits when the greeter exits — letting the daemon proceed to the
   session;
3. picks a backend: drm on real hardware (default), or a nested/headless backend
   via `WESTON_BACKEND` for demos.

Generated config:

```ini
[core]
shell=kiosk-shell.so
idle-time=0
require-input=false

[autolaunch]
path=/usr/bin/bacak-greeter
watch=true
```

## Using the real `bacak-compositor`

Once the native compositor is built and installed at `/usr/bin/bacak-compositor`
(replacing the weston wrapper), BDM drives it with no changes. The launch
contract is the **`$BACAK_STARTUP`** environment variable: the compositor binds
its Wayland socket, exports `WAYLAND_DISPLAY`, then spawns `$BACAK_STARTUP` as
its hosted client. BDM's `spawn_greeter` sets

```
BACAK_STARTUP=/usr/bin/bacak-greeter
```

(and still passes `--greeter …` for the weston wrapper, so either compositor
works unchanged). On the compositor side this is one small hook,
`launcher::spawn_startup()`, called from both the udev (DRM/KMS) and winit
(nested) backends right after `WAYLAND_DISPLAY` is exported.

**Verified locally:** running the real `bacak-compositor` (nested) with
`BACAK_STARTUP` pointing at the greeter, the compositor logs
`spawning session startup client ($BACAK_STARTUP)` and the greeter connects to
the BDM daemon and drives the login protocol (`Hello` → `ListUsers` →
`ListSessions`). On a real seat the compositor's udev backend takes DRM master
via the greeter logind session (path B) and renders the greeter directly.

## A. Nested demo (no root, no PAM, no seat) — works today

The reliable way to see the whole greeter-on-weston experience inside your
current desktop. Uses the bundled mock daemon (password: **bacak**).

```sh
sudo apt install weston            # one-time
packaging/prototype/run-weston-demo.sh
```

This builds the GUI greeter + mock daemon, starts the mock daemon, then launches
weston (nested in your X11/Wayland session) hosting the greeter fullscreen. Pick
a user, type `bacak`, choose a session — on "success" the greeter closes and
weston shuts down, exactly as it would after a real login.

Headless box? It falls back to weston's headless backend; capture a frame with
`weston-screenshooter` (bind a key in the config) to verify rendering.

## B. Real seat via the BDM daemon (logind session — bare metal)

This path now registers a **logind session** for the greeter, so weston's drm
backend can take DRM master and open input on `seat0`. The package already ships
everything; the steps below assume the `.deb` is installed (with a `system-pam`
build) and weston is present.

```sh
sudo apt install weston       # pulled in via Recommends
# Shipped by the package:
#   /usr/bin/bacak-compositor                       (weston wrapper)
#   /etc/bacak-display-manager/weston-greeter.ini   (kiosk config)
#   /etc/pam.d/bacak-greeter                         (logind session service)
#   /etc/bacak-display-manager.conf  →  register_greeter_session = true
```

### How the gap was closed

`pam_systemd` registers a logind session with the **calling process** as the
session leader. So the daemon opens the session **inside the forked child that
becomes weston**, via `Command::pre_exec` (still root, before `setuid`):

```
fork ─▶ child (root):
          setsid · initgroups
          pam_start("bacak-greeter", "bacak-greeter")
          pam_putenv XDG_SESSION_CLASS=greeter / TYPE=wayland / SEAT / VTNR
          pam_acct_mgmt · pam_setcred(ESTABLISH) · pam_open_session   ← child is leader
          setgid · setuid  (drop to bacak-greeter)
        exec /usr/bin/bacak-compositor  (weston)
```

Because the exec'd weston is the session leader, libseat/logind grant it the
seat; `sd_pid_get_session` finds the session with no env plumbing needed. The PAM
handle is intentionally not closed — logind reaps the session when the leader
(weston) exits, which is exactly when the greeter is done. The implementation is
[`open_session_preexec`](../crates/bacak-pam/src/system.rs) +
[`spawn_greeter`](../crates/bacak-display-manager/src/launch.rs); toggle it with
`[daemon] register_greeter_session`.

### Requirements / caveats

- Must be a **`system-pam`** build (the PAM/logind code is feature-gated):
  `cargo build --release -p bacak-display-manager --features system-pam`.
- The daemon service owns a VT (`TTYPath=/dev/tty1`, `XDG_VTNR=1`); logind marks
  the greeter session active when that VT is foreground.
- weston ≥ uses **libseat**; on a logind system that's automatic.
- **Verified in a VM** — `packaging/vm/smoke-test.sh` boots a Debian 13 VM with a
  virtio-gpu (virgl 3D) seat (generic kernel) and confirms `SMOKE_RESULT: PASS`:
  `/dev/dri` present, `seat0` graphical, an active `wayland`/`greeter` logind
  session owned by `bacak-greeter`, the **real `bacak-compositor`** holding DRM
  master and hosting the greeter via `$BACAK_STARTUP`. An **autologin sub-test**
  additionally verifies the post-login handoff: BDM opens a `bacak-autologin`
  logind session (class `user`) and the user's `bacak-session → bacak-compositor`
  takes the seat as the logged-in user. (Opening the PAM session in `pre_exec`
  does post-fork D-Bus work — the standard fork→PAM→exec login pattern.)
- **Interactive password login is verified too** — an interactive sub-test drives
  the greeter protocol with a real password (via a headless test client), and
  confirms the daemon authenticates via `pam_unix`, opens a `bacak-display-manager`
  logind `user` session in `pre_exec`, and the user's compositor takes the seat.
  Both login modes (greeter password + autologin) now pass end-to-end with the
  real compositor on a real DRM seat. The only part not exercised in CI is literal
  keystroke entry into the egui GUI (the GUI greeter itself is verified to open,
  connect, and drive the protocol separately).

### Trying path B safely (VM)

**Automated:** [`packaging/vm/smoke-test.sh`](../packaging/vm/smoke-test.sh)
boots a throwaway Debian VM with a virtio-gpu seat, installs the `.deb` + weston,
enables BDM, and asserts via `loginctl` that an *active* `greeter` session exists
on a graphical `seat0` (see [`packaging/vm/README.md`](../packaging/vm/README.md)):

```sh
sudo apt install qemu-system-x86 qemu-utils cloud-image-utils
packaging/vm/smoke-test.sh          # exit 0 = PASS
```

**Manual**, in a VM with systemd + a GPU/virtio-gpu seat:

```sh
sudo apt install ./bacak-display-manager_*_amd64.deb weston
sudo systemctl disable gdm3 2>/dev/null || true
sudo systemctl enable bacak-display-manager
sudo systemctl start  bacak-display-manager     # or reboot
journalctl -u bacak-display-manager -b          # watch the greeter session register
loginctl                                         # should list a 'greeter' session on seat0
```

Recovery (from a TTY, Ctrl+Alt+F3): `sudo systemctl disable bacak-display-manager
&& sudo systemctl enable gdm3 && sudo systemctl start gdm3`.

**Path A (nested weston) remains the zero-risk demo** and exercises the full
greeter ↔ daemon protocol, GUI, and kiosk/autolaunch integration without a seat.

## Customizing weston

Edit [`config/weston-greeter.ini`](../config/weston-greeter.ini) (outputs,
resolution, scale, keymap) and point the wrapper at it:

```sh
BDM_WESTON_CONFIG=/etc/bacak-display-manager/weston-greeter.ini \
    bacak-compositor --greeter /usr/bin/bacak-greeter
```
