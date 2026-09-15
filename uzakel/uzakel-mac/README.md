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
Without those, the crate falls through to its cross-platform default: the
console `--pin` path, `RawZstd` encode.

**Update, 2026-09-15 — verified on real Apple Silicon hardware (M4
MacBook Air).** That default builds and runs there, and a full session —
pairing, video (BacakOS decoding the Mac's actual screen), and input
(BacakOS-side pointer movement landing on the Mac's real cursor) — was
confirmed end-to-end. Getting there found and fixed two real bugs in the
shared crate/its vendored `scrap` fork (not `#[cfg(windows)]`-gated, so
they'd have hit any macOS build): `enigo`'s macOS backend isn't `Send`
(broke `tokio::spawn`ing the input task), and `scrap`'s quartz capture
derived the wrong per-row stride (produced a sheared/striped image). See
`../uzakel-windows/README.md`'s "Real macOS end-to-end pass" section for
the full writeup — both fixes live in that shared crate, not here.

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

## Known gap — this directory's own pieces are still unverified

The shared server crate is verified (see above). What's *not*:

- `packaging/build_mac.sh` cross-compiles *from Linux* (`osxcross`), a
  different path than the native `cargo build` that was actually run on
  the real Mac to get the verification above — no osxcross toolchain
  exists in this environment, so the script itself has still never run.
  If a real Mac is available (as it was for the verification above),
  building natively *on* it (no cross-compile, no osxcross) is simpler
  and already known to work — reach for the script only if the goal is
  specifically building from Linux/CI.
- `gui-launcher/` does `cargo check --target aarch64-apple-darwin` /
  `--target x86_64-apple-darwin` clean (`objc2`/`objc2-app-kit` are pure
  Rust bindings — `check` needs no C compiler or Apple frameworks, just
  the target's `std`). That's real signal the *shape* of the code
  type-checks, but its `main()` is a bare `todo!()`: nothing has been
  linked (needs the real frameworks) or run (needs a real Mac), so it's
  still a starting sketch, not working code.
- Screen Recording (TCC) permission and its "doesn't carry over between
  launch contexts" behavior (see the linked writeup) means `build_mac.sh`'s
  own TODO about it is real and unresolved: a `.app` a user downloads and
  double-clicks is yet another launch context, not obviously the same as
  either the SSH or Terminal.app contexts tested so far — worth
  re-verifying specifically once the `.app` bundling itself is real.
