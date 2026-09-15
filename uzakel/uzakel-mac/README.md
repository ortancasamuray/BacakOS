# uzakel-mac

macOS-specific pieces for the "Uzak Masaüstü" remote-desktop feature. **The
actual remote-desktop server is not duplicated here** — it lives in
[`../uzakel-windows/bacak-remote-server`](../uzakel-windows/bacak-remote-server),
and per its own doc comments is already written OS-generically:

- `capture.rs` — `scrap` (DXGI on Windows, **CoreGraphics/Quartz on macOS**,
  X11 on Linux)
- `input_inject.rs` — `enigo` (Win32 `SendInput` on Windows, **CGEvent on
  macOS**, uinput on Linux)
- `encode.rs`, `network.rs` — plain Rust/tokio, no OS-specific code at all

Only two things in that crate are Windows-only (`#[cfg(windows)]`): the
graphical pairing window (`gui.rs`) and hardware H.264 encode
(`encode_h264.rs`, needs `ffmpeg-next` + Intel/NVIDIA/AMD vendor SDKs that
don't apply on Mac anyway — see `../uzakel-windows/HARDWARE_ENCODE_PLAN.md`).
Without those, the crate already falls through to its cross-platform
default: the console `--pin` path, `RawZstd` encode. That default should
build and run on macOS *as-is*, from the existing crate — **this has not
been verified**, see "Known gap" below.

This directory exists for the macOS-only pieces that don't belong in that
shared, cross-platform crate:

- `packaging/build_mac.sh` — cross-compiles the shared server for
  `aarch64-apple-darwin`/`x86_64-apple-darwin` and bundles it into a minimal
  `.app` (skeleton/untested — see the script's own header).
- `gui-launcher/` — a **skeleton, not a working implementation** for a
  native macOS pairing window equivalent to the Windows crate's `gui.rs`,
  built as a *separate* small binary rather than by adding a
  `#[cfg(target_os = "macos")]` GUI module to the shared server crate (the
  Windows GUI is already threaded through `main.rs`/`run_session` in a
  fairly involved way — see `gui.rs`'s own module doc — and duplicating
  that wiring for a platform nobody can build or test here yet isn't worth
  the risk of subtly breaking the working Windows path). It's meant to
  `exec`/spawn the real `bacak-remote-server --pin <n> --no-gui` once a PIN
  is submitted, the same relationship the Windows GUI has with `run_session`
  — just out-of-process instead of in-process.

## Known gap — nothing here has been built or run

There is no macOS machine and no working macOS cross-toolchain (osxcross)
in this environment. `rustup target add aarch64-apple-darwin` succeeds
(Rust ships precompiled `std` for it), but `cargo check --target
aarch64-apple-darwin -p bacak-remote-server` from `uzakel-windows` already
fails *before* reaching any of this crate's own code — `zstd-sys`'s C
shim needs a real Apple `cc` (understands `-arch`/`-mmacosx-version-min`),
and the Linux host's `cc` doesn't. So:

- Whether the shared `bacak-remote-server` crate actually builds clean for
  macOS is **unverified**, not just "the GUI/packaging part."
- `gui-launcher/` does `cargo check --target aarch64-apple-darwin` /
  `--target x86_64-apple-darwin` clean (`objc2`/`objc2-app-kit` are pure
  Rust bindings — `check` needs no C compiler or Apple frameworks, just
  the target's `std`). That's real signal the *shape* of the code
  type-checks, but its `main()` is a bare `todo!()`: nothing has been
  linked (needs the real frameworks) or run (needs a real Mac), so it's
  still a starting sketch, not working code.

Whoever picks this up next, on a real Mac (or with osxcross set up
properly): first get `cargo build --target
<aarch64|x86_64>-apple-darwin -p bacak-remote-server` green from
`uzakel-windows` with the console `--pin` path, *then* come back to this
directory's packaging/GUI pieces.
