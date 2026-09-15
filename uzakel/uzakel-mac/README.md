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

## `packaging/build_mac.sh` — verified on the real Mac, 2026-09-15

Run natively *on* the real Mac (not cross-compiled from Linux — no
osxcross toolchain exists in this environment, so that path is still
unverified; building directly on a real Mac, as done here, is simpler
anyway and doesn't need one). Both `cargo build --release --target
{aarch64,x86_64}-apple-darwin`, `lipo -create` into a universal binary,
and the `.app` bundle (`Info.plist` correctly filled in) all worked —
`lipo -info` on the result confirms both `x86_64` and `arm64` slices.

**But the `.app` doesn't actually run when double-clicked (`open
'Bacak Remote Server.app'`, simulating that): no process appears at
all.** Root cause (not yet fixed): a double-click launches
`bacak-remote-server` with *no arguments*, so `main()` falls to the
console `--pin` path's `prompt_pin()`, which reads `stdin` — but a
Finder/`open`-launched app has no `stdin` to read, so it fails
immediately and exits with nothing visible (no attached console either,
same class of problem `uzakel-windows/bacak-remote-server/src/gui.rs`'s
module doc describes for Windows before it got a real GUI). This is
*exactly* the gap `gui-launcher/` exists to fill — until it's a real,
linked, running program, the `.app` this script produces is not
double-click-usable, only useful launched with `--pin <n> --no-gui`
from a terminal (which does work — see the shared crate's own
verification above).

## Known gap — this directory's own pieces are still unverified

- `gui-launcher/` does `cargo check --target aarch64-apple-darwin` /
  `--target x86_64-apple-darwin` clean (`objc2`/`objc2-app-kit` are pure
  Rust bindings — `check` needs no C compiler or Apple frameworks, just
  the target's `std`). That's real signal the *shape* of the code
  type-checks, but its `main()` is a bare `todo!()`: nothing has been
  linked (needs the real frameworks) or run (needs a real Mac). Building
  and wiring this in is now the concrete blocker for a double-click-
  usable `.app` (see directly above), not a hypothetical nice-to-have.
- Screen Recording (TCC) permission and its "doesn't carry over between
  launch contexts" behavior (see the linked writeup) means `build_mac.sh`'s
  own TODO about it is real and unresolved: a launch via `open`/Finder is
  yet another launch context, not obviously the same as the SSH or
  Terminal.app contexts already tested — worth checking once
  `gui-launcher` makes the `.app` actually reach the point of trying to
  capture the screen at all.
