# Bacak Display Manager (BDM)

🌐 [Türkçe](README.tr.md) · **English**

A modern, secure, lightweight, **Wayland-native** display manager for the Bacak
Desktop Environment — a replacement for LightDM, GDM and SDDM, designed
touch-first and multi-user from the start.

> Status: **working scaffold**. The core (config, user enumeration, session
> discovery, IPC protocol, PAM abstraction, session launching, privilege
> separation) is implemented, builds, and is unit/integration tested. The
> graphical greeter frontend and the real `libpam` linkage are feature-gated
> seams with reference implementations of everything they sit on.

## Why another display manager?

| Goal                | How BDM does it                                            |
|---------------------|-----------------------------------------------------------|
| Wayland first       | Greeter renders on `bacak-compositor`; no X required.     |
| Secure by default   | Greeter is fully unprivileged; only the daemon is root.   |
| Touch friendly      | Touchscreen detection drives an on-screen keyboard + large hit targets. |
| Multi-user          | logind seats/sessions; per-user `XDG_RUNTIME_DIR`.        |
| Lightweight         | Small Rust binaries, `panic=abort`, LTO release profile.  |

## Components

```
bacak-display-manager   privileged daemon  (root)  — PAM, seat, launching
bacak-greeter           login UI           (unprivileged) — IPC client + frontend
bacak-session-launcher  session exec stage (the user)     — sets env, execs session
bacak-common            shared library     — config, users, sessions, IPC, power
bacak-pam               PAM abstraction    — conversation model + backends
```

## System flow

```
boot → systemd → bacak-display-manager (root)
                      │  reads /etc/bacak-display-manager.conf
                      ├─ autologin? ─yes→ bacak-session-launcher → bacak-compositor
                      └─no→ spawn bacak-greeter (unprivileged) on bacak-compositor
                                 │  IPC over /run/bacak-display-manager/greeter.sock
                                 │  authenticate via daemon→PAM
                                 ↓ success
                            bacak-session-launcher (as the user)
                                 ↓
                            bacak-compositor → desktop session
```

## Repository layout

```
Cargo.toml                     workspace
crates/
  bacak-common/                config, users, sessions, ipc, power  (+15 unit tests)
  bacak-pam/                   Authenticator/Conversation; mock + system-pam backends
  bacak-display-manager/       daemon: main, seat, ipc, auth, launch, power
  bacak-greeter/               GreeterClient core + reference TTY frontend (+ e2e test)
  bacak-session-launcher/      Exec-line parser + session exec
config/bacak-display-manager.conf   annotated example config
systemd/bacak-display-manager.service
pam/bacak-display-manager           PAM service (interactive)
pam/bacak-autologin                 PAM service (autologin)
sessions/bacak.desktop              example Wayland session entry
docs/                          design documents (see below)
```

## Documentation (design deliverables)

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — architecture diagram, module structure, login workflow.
- [docs/SECURITY.md](docs/SECURITY.md) — security model & privilege separation.
- [docs/PAM_INTEGRATION.md](docs/PAM_INTEGRATION.md) — PAM conversation design.
- [docs/SYSTEMD.md](docs/SYSTEMD.md) — service design & VT/seat handling.
- [docs/SESSION_STARTUP.md](docs/SESSION_STARTUP.md) — session start/stop sequence.
- [docs/GREETER_UI.md](docs/GREETER_UI.md) — login UI, touch, virtual keyboard, theming, multi-monitor, accessibility.
- [docs/PROTOTYPE.md](docs/PROTOTYPE.md) — running BDM on **weston** today (the real compositor isn't written yet).

## Build & test

```sh
cargo build                 # default: builds everything with the mock auth backend
cargo test                  # 24 tests across the workspace

# Production build with real PAM (links libpam.so.0 directly — no libpam0g-dev,
# no bindgen, no libclang needed; just the runtime libpam package):
cargo build --release -p bacak-display-manager --features system-pam
# Graphical greeter (egui/eframe; needs wayland/xkbcommon/fontconfig dev libs):
cargo build --release -p bacak-greeter --features gui
# Drive it without root/PAM/compositor via the bundled mock daemon:
#   BDM_GREETER_SOCKET=/tmp/bdm.sock cargo run -p bacak-greeter --example mock_daemon &
#   BDM_GREETER_SOCKET=/tmp/bdm.sock cargo run -p bacak-greeter --features gui
```

### Install on a real machine

The package ships a setup helper, **`bacak-dm-setup`** (also runnable from a
source tree as `packaging/install.sh`), that bundles install / switch / recovery
/ uninstall:

```sh
sudo apt install ./target/debian/bacak-display-manager_*_amd64.deb   # gives you `bacak-dm-setup`
sudo bacak-dm-setup install                  # compositor + session + deps (safe; no DM change)
sudo bacak-dm-setup enable                    # make BDM the display manager (confirms; reboot to apply)
sudo bacak-dm-setup enable --autologin <user> # …with autologin (the most-verified mode)
sudo bacak-dm-setup status                    # what's installed / active
sudo bacak-dm-setup revert                     # RECOVERY: re-enable the previous DM (run from Ctrl+Alt+F3)
sudo bacak-dm-setup uninstall                  # revert + remove the package
```

`install` never touches your current display manager; only `enable` does, and it
prints the recovery steps and remembers the previous DM so `revert` can restore
it. Compositor: pass `BDM_COMPOSITOR=/path` or `BDM_COMPOSITOR_SRC=/path/to/bacak`
(else the bundled weston wrapper is kept).

### Try the protocol without root

The reference TTY greeter speaks the exact same protocol as the GUI. With a
running daemon it connects via `$BDM_GREETER_SOCKET`; the
`bacak-greeter/tests/protocol.rs` integration test exercises the full login
conversation against a scripted daemon over a temp socket — no root, no PAM, no
compositor required.

## Continuous integration

CI is **not tied to GitHub** — `packaging/vm/smoke-test.sh` is a provider-agnostic
shell script; any runner with KVM can drive it. The workflows live under
`.forgejo/workflows/` (Forgejo/Gitea Actions, FOSS, self-hostable; the same YAML
runs on Gitea). Rust is installed via `rustup` in-step, so the only action used
is `actions/checkout`.

- **`ci.yml`** — on every push/PR: `rustfmt --check`, `clippy -D warnings`,
  `cargo test`, the `system-pam`/`gui` feature builds, and a `.deb` build +
  `lintian`. Runs on any Debian/Ubuntu runner (`runs-on: ubuntu-latest`).
- **`vm-smoke-test.yml`** — on demand / weekly / on prototype changes: boots a
  Debian VM with a virtio-gpu (virgl 3D) seat and asserts the bare-metal logind
  `greeter` session hosted by the **real `bacak-compositor`** (DRM master +
  `$BACAK_STARTUP`; see [docs/PROTOTYPE.md](docs/PROTOTYPE.md)). Needs a
  **KVM-capable runner** (`runs-on: self-hosted`; host-mode, or a container with
  `--device /dev/kvm`). The compositor is built from a separate repo — configure
  `vars.BACAK_COMPOSITOR_REPO` (default `bacak-os/bacak`), optional
  `vars.BACAK_COMPOSITOR_REF` / `secrets.BACAK_REPO_TOKEN`.

A **Woodpecker CI** variant is also provided under `.woodpecker/` (`ci.yml` +
`vm-smoke-test.yml`), for those running Woodpecker instead of Forgejo Actions:
- `ci.yml` — build/test/lint/package in `rust:bookworm` containers (no KVM).
- `vm-smoke-test.yml` — the VM job; needs KVM via either a *trusted* repo with
  `privileged: true` (docker backend) or a **local-backend** agent on a KVM host.
  The compositor is cloned from a single secret URL (`bacak_compositor_clone_url`,
  token embeddable for private repos).

Not on either? The script runs anywhere: locally (`packaging/vm/smoke-test.sh`,
optionally on a systemd timer/cron), or under GitLab CI / Drone / Jenkins — each
just needs to call that script on a KVM host. (The previous GitHub Actions
workflows were moved to `.forgejo/`; ask to regenerate them if you want both.)

## Feature flags

| Crate                   | Feature       | Effect                                         |
|-------------------------|---------------|------------------------------------------------|
| `bacak-pam`             | `system-pam`  | Real `libpam` via a small hand-written FFI.    |
| `bacak-display-manager` | `system-pam`  | Daemon uses the real PAM backend.              |
| `bacak-greeter`         | `gui`         | Builds the egui/eframe graphical frontend.     |

Default builds use a **mock** authenticator (accepts password `bacak`) so the
flow runs end-to-end on any machine for development. The daemon logs a loud
warning when built without `system-pam`; never deploy that build.

## License

GPL-3.0-or-later.
