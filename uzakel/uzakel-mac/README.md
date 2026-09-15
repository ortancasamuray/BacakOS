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
- `gui-launcher/` — a native macOS pairing window equivalent to the
  Windows crate's `gui.rs`, built as a *separate* small binary rather than
  by adding a `#[cfg(target_os = "macos")]` GUI module to the shared
  server crate (the Windows GUI is already threaded through
  `main.rs`/`run_session` in a fairly involved way — see `gui.rs`'s own
  module doc — and duplicating that wiring risked subtly breaking the
  working Windows path for no real benefit). It spawns the real
  `bacak-remote-server --pin <n> --no-gui` as a subprocess once a PIN is
  submitted, the same relationship the Windows GUI has with
  `run_session` — just out-of-process instead of in-process. **Built,
  compiled, and run successfully on the real Mac, 2026-09-15** — see
  below.

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

## `gui-launcher/` — built, compiled, and run on the real Mac, 2026-09-15

`objc2`/`objc2-app-kit` 0.6/0.3, `define_class!` for the delegate object
(app delegate + window delegate + the three button actions), following
the pattern in `objc2`'s own `hello_world_app.rs` example. Compiled clean
(`cargo build --release`, zero warnings after removing a handful of
now-unneeded `unsafe` blocks the compiler flagged) and run via `open` on
the real Mac:

- The window renders (title, PIN field, "Eşleştir"/"Eşleşmeyi Bitir"/
  "Kapat" buttons, status label) — confirmed visually by the person at
  the Mac's actual screen (this environment can't screenshot over SSH,
  same Screen Recording/TCC story as everywhere else in this doc).
- Submitting a PIN spawns the real `bacak-remote-server --pin <n>
  --no-gui` as a child process (`find_server_binary()` looks next to
  itself first, then falls back to the dev-checkout path this was tested
  against) and it genuinely pairs with BacakOS and streams video/input —
  full session confirmed end-to-end through this launcher, not just
  through a bare terminal invocation.
- One real snag along the way, not a `gui-launcher` bug: a stale
  `bacak-remote-server` process from earlier manual testing was still
  holding the UDP ports, so BacakOS's `PairRequest` kept hitting *that*
  process (with its old PIN) instead of the freshly-spawned one — looked
  like a rejection, was actually a port squatter. Worth remembering when
  testing this repeatedly: `pkill -f bacak-remote-server` (or a full
  reboot) before each attempt if pairing rejects a PIN that looks right.

Not yet done: tailing the child's output for real pairing status (see
this file's own module doc for why — no channel across a process
boundary yet, so the window only ever shows "PIN gönderildi…", never
"eşleşti"/"reddedildi"), and the double-click-from-Finder path this was
built to enable (still needs bundling *into* `build_mac.sh`'s `.app`
alongside the server — not done in this pass, which ran the launcher
directly from its own `target/release/`).

## Known gap

- The `.app` `build_mac.sh` produces still only contains
  `bacak-remote-server`, not `gui-launcher` — double-clicking it from
  Finder is therefore still the `prompt_pin()`/no-`stdin` failure
  described above; `gui-launcher` was verified as its own standalone
  binary, launched directly, not through that `.app`. Bundling both
  binaries together (and pointing `gui-launcher` at its bundled sibling
  — `find_server_binary()` already looks there first) is the remaining
  step for a real double-click experience.
- Screen Recording (TCC) permission and its "doesn't carry over between
  launch contexts" behavior (see the linked writeup) hasn't been
  re-checked for an `open`/Finder-launched `.app` specifically (only for
  bare-binary SSH vs. Terminal.app launches) — worth checking once the
  bundling above is done.
