# Bacak — Wayland Compositor

The desktop core of BacakOS: a native, [Smithay](https://smithay.github.io/)-based
Wayland compositor written in Rust, with window management, input, and every
desktop panel (dock, control center, Wi-Fi, Bluetooth, audio, desktop
settings) built directly into the compositor process. See
[ARCHITECTURE.md](ARCHITECTURE.md) for the full module map and
[DESIGN_SYSTEM.md](DESIGN_SYSTEM.md) for the visual language.

> Rule of thumb for this workspace: **if a feature belongs on the desktop, it
> lives inside `bacak-compositor`.** Don't add a separate app for
> Wi-Fi/Bluetooth/audio/settings — extend the relevant `src/plugins/*.rs`
> module instead.

## Workspace crates

| Crate | Kind | Purpose |
|-------|------|---------|
| `bacak-compositor` | binary | The compositor itself — WM, input, rendering, all desktop panels (`src/plugins/`). |
| `bacak-services` | lib | Non-graphical service layer: virtual filesystem, archive backends, device (audio/Wi-Fi/Bluetooth) providers, network diagnostics. Shared by the compositor, CLI, and shell. |
| `bacak-shell` | binaries | `bacak-panel` / `bacak-dock` / `bacak-launcher` — standalone GTK4 surfaces over `wlr-layer-shell`. Early scaffold; the compositor's built-in panels (`plugins/dock.rs`, `plugins/control_center.rs`) are what actually ships. |
| `bacak-cli` | binary | Demo/debug CLI that exercises every `bacak-services` surface from a terminal. |
| `bacak-plugin-network`, `bacak-plugin-audio`, `bacak-plugin-desktop-settings` | packaging-only | Empty crates whose `.deb` metadata declares the corresponding compositor plugin as a package; no code — the real implementation is `bacak-compositor/src/plugins/{network,audio,desktop_settings}.rs`. |
| `bacak-icons` | packaging-only | BacakOS icon theme (freedesktop icon theme layout). |
| `bacak-desktop-defaults` | packaging-only | Default `compositor.json`, wallpaper, and Firefox policy shipped on first login. |
| `bacak-grub-theme` | packaging-only | GRUB bootloader theme. |
| `bacak-plymouth-theme` | packaging-only | Plymouth boot-splash theme. |
| `bacakos-meta` | packaging-only | Meta-package — `apt install ./bacakos_*.deb` pulls in the whole desktop with one dependency list. |

## Build & run

`bacak-compositor` has three build profiles, gated by cargo features and
selected at runtime via `$BACAK_BACKEND`:

```bash
# Skeleton — fast `cargo check`, no display server, no Wayland/DRM headers needed
cargo run -p bacak-compositor

# Dev — nested Wayland window via winit, runs inside any host WM
cargo run -p bacak-compositor --features runtime

# Production — native session: libseat + DRM/KMS + libinput, no host compositor
cargo build --release -p bacak-compositor --features udev
BACAK_BACKEND=udev ./target/release/bacak-compositor
```

`runtime`/`udev` pull in Smithay, libinput, libudev, libdrm, libxkbcommon,
libgbm, libegl, and libseat — install the matching `-dev` packages before
building with either feature (see the feature comments in
`crates/bacak-compositor/Cargo.toml`).

```bash
cargo test --workspace          # unit tests across all crates
cargo run -p bacak-cli          # exercise the service layer from a terminal
```

## Packaging

```bash
cargo deb -p bacak-compositor --no-build
sudo dpkg -i target/debian/bacak-compositor_0.1.0-1_amd64.deb
sudo pkill bacak-compositor
```

`packaging/bacak-session` is the session script a display manager execs into:
it starts PipeWire/WirePlumber, waits for the Wayland socket, launches a
polkit agent (`lxpolkit`), then hands off to `bacak-compositor`.

## License

MIT OR Apache-2.0.
