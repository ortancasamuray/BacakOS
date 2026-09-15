# Bacak — Architecture

A native Rust Wayland compositor built on [Smithay](https://smithay.github.io/),
not a webview/Electron/Tauri shell. The desktop's panels, dock, and settings
UI are Rust code compiled straight into `bacak-compositor` and rendered by
its own GL renderer — there is no HTML/CSS/JS anywhere in the stack.

---

## 1. Workspace layout

```
bacak/
├─ Cargo.toml                         # workspace: 11 members
├─ crates/
│  ├─ bacak-compositor/               # the compositor binary — WM, input, render, panels
│  │  └─ src/
│  │     ├─ state.rs                  # BacakState — the god object, ~11.5k lines
│  │     ├─ udev_runtime.rs           # native DRM/KMS main loop (production backend)
│  │     ├─ runtime.rs                # winit backend (dev/nested-window backend)
│  │     ├─ main.rs                   # entry point, backend selection
│  │     ├─ wm.rs                     # window manager: tiling, workspaces, snap zones
│  │     ├─ render.rs                 # GL rendering of the whole scene incl. panels
│  │     ├─ input.rs / keyboard.rs    # pointer/touch/keyboard, OSK controller
│  │     ├─ gestures.rs               # swipe/pinch/long-press recognizers
│  │     ├─ bluetooth.rs              # bluetoothctl coprocess + OBEX receiver
│  │     ├─ animation.rs              # spring-physics animation curves
│  │     ├─ session.rs                # session save/restore
│  │     ├─ xwayland.rs               # Xwayland spawn + X11 WM duties
│  │     ├─ text.rs / text_input.rs / selection.rs / atspi.rs  # text layout, IME, selection, a11y
│  │     ├─ screencopy.rs / foreign_toplevel.rs / decoration.rs / hotplug.rs / grab.rs / signals.rs / focus.rs / carousel.rs / blur.rs / emoji.rs / launcher.rs / icons.rs / config.rs / handlers.rs
│  │     └─ plugins/                  # every desktop panel — see §3
│  ├─ bacak-services/                 # fs, archive, device, network — see §4
│  ├─ bacak-shell/                    # GTK4 layer-shell scaffold — see §5 (superseded)
│  ├─ bacak-cli/                      # terminal harness over bacak-services
│  ├─ bacak-plugin-network/           # packaging-only stub (real code: compositor plugins/network.rs)
│  ├─ bacak-plugin-audio/             # packaging-only stub (real code: compositor plugins/audio.rs)
│  ├─ bacak-plugin-desktop-settings/  # packaging-only stub (real code: compositor plugins/desktop_settings.rs)
│  ├─ bacak-plugin-uzakel/            # packaging-only stub (real code: compositor plugins/uzakel.rs)
│  ├─ bacak-icons/                    # icon theme (packaging-only)
│  ├─ bacak-desktop-defaults/         # default compositor.json + wallpaper + Firefox policy
│  ├─ bacak-grub-theme/               # GRUB theme (packaging-only)
│  ├─ bacak-plymouth-theme/           # Plymouth splash theme (packaging-only)
│  └─ bacakos-meta/                   # meta-package, no code
```

Workspace `Cargo.toml` pins shared dependency versions (`tokio`, `serde`,
`thiserror`, `anyhow`, `parking_lot`, `tracing`, archive backends, …) once
for every crate.

---

## 2. Compositor build profiles

`bacak-compositor` compiles three ways, chosen by cargo feature and dispatched
at runtime by `$BACAK_BACKEND` (see the doc comment at the top of
`src/main.rs`):

| Feature flags | Backend | Use case | System deps |
|---|---|---|---|
| *(none)* | skeleton — WM/OSK init only, no Wayland socket | fast `cargo check` | none |
| `--features runtime` | `winit` — nested Wayland window | dev, runs inside a host WM | libwayland, libinput, libxkbcommon, EGL |
| `--features udev` | `libseat` + DRM/KMS native session | real BacakOS session | + libdrm, libgbm, libseat |

`udev` is a superset of `runtime` (see the `udev = ["runtime", "smithay?/backend_udev", …]`
line in `crates/bacak-compositor/Cargo.toml`) — it adds the DRM/GBM/EGL/libinput/
libseat Smithay backends on top of everything `runtime` already pulls in.
XWayland support (spawning `Xwayland` and acting as its X11 window manager so
legacy X11 clients map as ordinary Bacak windows) is compiled into the
Smithay dependency itself, gated behind the same features.

Optional runtime pieces are individually feature-gated so a plain
`cargo check` on the workspace stays cheap: `fontdue`/`cosmic-text` (text
shaping), `zbus` (AT-SPI2 accessibility bridge), `image`/`resvg`/
`freedesktop-icons` (app icon loading), `notify` (filesystem watching),
`qrcode` (module data for the Uzakel pairing panel's QR, rasterised to RGBA
by `src/qr.rs` — see §3's `plugins/uzakel.rs` row).

---

## 3. `BacakState` and the plugin panels

`state.rs` holds `BacakState` — one struct carrying every Wayland protocol's
server-side state plus the Bacak-specific `WindowManager` and `OskController`.
Smithay tracks `WlSurface`s; the Bacak WM tracks abstract `wm::WindowId`s and
never touches a Wayland surface directly — the bridge is a
`HashMap<WlSurface, WindowId>` inside `BacakState`, populated when an
xdg-shell toplevel arrives and pruned on surface destruction.

Every desktop panel is a module under `src/plugins/`, each owning its own
state struct, rendered by `render.rs` and driven by a `*_poll()`/`build_*`
pair called from the compositor's tick loop:

| Module | Panel | Poll fn | Build fn |
|---|---|---|---|
| `plugins/control_center.rs` | Quick-settings box — Wi-Fi, Bluetooth, audio, brightness, dark mode, screenshot, power | `bt_poll()`, `wifi_poll()`, `audio` state | `build_bt_panel()`, `build_wifi_panel()` |
| `plugins/desktop_settings.rs` | Wallpaper, hostname, auto-login, password change | `ds_tick()` | `build_ds_panel()` |
| `plugins/uzakel.rs` | "Uzakel'e Bağlan" tile — QR code of the `uzakel-daemon`'s current pairing PIN + LAN address, so the Android app can scan instead of typing a PIN (see `../../uzakel/ARCHITECTURE.md` §2.3.1) | `read_uzakel_pairing_state()` (reads `~/.cache/uzakel/pairing.json`, written by the daemon) | `open_uzakel_panel()` |
| `plugins/remote_desktop.rs` | "Uzak Masaüstü" panel — pairs with a `bacak-remote-server` PC (see `../../uzakel/uzakel-windows/README.md`) and streams its screen full-panel, forwarding local pointer input back to it over the same PIN+X25519+ChaCha20-Poly1305 session. Logic lives in `src/remote_desktop.rs`, not `state.rs` — see that file's module doc for why. v1 pairs via `~/.config/bacak-remote/pair.json` (no on-screen text-entry dialog yet) | `remote_desktop_tick()` (polls the background pairing/receive thread) | `open_remote_desktop_panel()` |
| `plugins/dock.rs` | Dock — pinned apps, running apps, launcher | — | — |
| `plugins/apps_menu.rs` | Full app list | — | — |
| `plugins/network.rs` | Wi-Fi connection logic (`nmcli`) | `wifi_connect()` | — |
| `plugins/audio.rs` | Audio device selection (`pactl`) | `audio_connect()`, `list_sinks()` | — |
| `plugins/overview.rs` | Window/workspace overview | — | — |
| `plugins/screenshot.rs` | Screen capture | — | — |
| `plugins/keyboard.rs` | On-screen keyboard | — | — |
| `plugins/selection.rs` | Text selection/copy | — | — |
| `plugins/gestures.rs` | Gesture-to-action bindings | — | — |

Bluetooth is the compositor talking to a `bluetoothctl` coprocess
(`bluetooth.rs`) plus a Python OBEX agent (`start_obex_receiver()`,
`~/.cache/bacak/obex-agent.py`) for incoming file transfers into
`~/Downloads`. Wi-Fi shells out to `nmcli`; audio to `pactl`. None of this
runs as a separate process the user would see in a task list — it's all
inside the one `bacak-compositor` binary.

**Convention:** a new desktop feature is a new (or extended) `plugins/*.rs`
module wired into `state.rs` + `render.rs`, never a standalone application.

---

## 4. `bacak-services` — the non-graphical layer

```rust
pub mod archive;  // ZIP / TAR / TAR.GZ, uniform listing API
pub mod fs;       // virtual filesystem: native + archive-backed paths
pub mod device;   // audio / Wi-Fi / Bluetooth, pluggable provider trait
pub mod network;  // connectivity diagnostics (stub)
```

`bacak-cli`, `bacak-compositor`, and `bacak-shell` all consume this crate
instead of talking to the OS directly — the compositor never reaches inside
`fs`/`archive` on its own, and the CLI exercises the exact code path the
shell would use. This is the seam that keeps service logic testable outside
a running Wayland session (`cargo run -p bacak-cli`).

---

## 5. `bacak-shell` — GTK4 scaffold (largely superseded)

Three small GTK4 binaries (`bacak-panel`, `bacak-dock`, `bacak-launcher`)
meant to layer on the compositor via `wlr-layer-shell`, each independently
spawnable/restartable. This predates the decision to build every panel
directly into `bacak-compositor` (§3) and has seen no further work since the
initial monorepo merge — treat it as a reference/experiment, not the
shipping UI. The dock and control center that actually ship are
`plugins/dock.rs` and `plugins/control_center.rs` inside the compositor.

---

## 6. Packaging-only crates

`bacak-plugin-network`, `bacak-plugin-audio`, `bacak-plugin-desktop-settings`,
`bacak-plugin-uzakel`, `bacak-icons`, `bacak-desktop-defaults`, `bacak-grub-theme`,
`bacak-plymouth-theme`, and `bacakos-meta` carry no logic — each is a
`[package.metadata.deb]` manifest (asset files + Depends list) so `apt` can
install/version a piece of the desktop (icon theme, GRUB/Plymouth branding,
default `compositor.json` + wallpaper, or the whole desktop via the meta
package) independently of the compositor binary itself.

---

## 7. Session flow

```
display manager (turan/bacak-display-manager)
        │
        ▼
packaging/bacak-session          # session script, execs into the compositor
        │  start pipewire/wireplumber
        │  wait for the wayland-* socket
        │  spawn lxpolkit (polkit auth agent) in the background
        ▼
bacak-compositor (BACAK_BACKEND=udev)
        │  DRM master, libinput, GBM/EGL
        ▼
plugins/* render the dock, control center, apps menu, OSK
```

See `turan/docs/SESSION_STARTUP.md` for the display-manager side of this
handoff.

---

## 8. Testing

```bash
cargo test --workspace     # unit tests across every crate
```

`bacak-services` and `wm.rs`/`animation.rs` carry the bulk of the pure-logic
unit tests; anything touching the live Wayland/DRM path needs a real session
(see `turan/docs/PROTOTYPE.md` for VM-based smoke testing).
