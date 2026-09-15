//! **SKELETON — does not build.** A macOS-native pairing window sketch,
//! mirroring what `../../uzakel-windows/bacak-remote-server/src/gui.rs`
//! does on Windows (small always-visible window: PIN entry, a status
//! label, an "Eşleşmeyi Bitir"/end button) — but as a *separate* binary
//! that launches the real server as a subprocess, rather than a
//! `#[cfg(target_os = "macos")]` module inside that shared, cross-platform
//! crate. See `../README.md` for why that separation was chosen over
//! extending the shared crate directly.
//!
//! ## Why a subprocess instead of an in-process module (like Windows)
//!
//! Windows' `gui.rs` runs the pairing window and `run_session` on two
//! threads *in the same process*, talking over `std::sync::mpsc` +
//! `tokio::sync::watch` — see that file's module doc. Doing the same here
//! would mean adding a macOS GUI module to the shared crate, gated by
//! `#[cfg(target_os = "macos")]` alongside the existing
//! `#[cfg(windows)]` one, and wiring it into `main()`'s branch that picks
//! console vs. GUI mode. Nobody here can build or run that crate for
//! macOS at all yet (see `../README.md`'s "Known gap") — extending it
//! blind risks a broken `cfg` boundary breaking the *working* Windows
//! build, for a change that can't be tested either way. A subprocess
//! launcher needs none of that: it only needs to invoke the existing
//! `--pin <n> --no-gui` console path, already proven to work on Windows,
//! probably fine as-is on macOS. Revisit merging this into the shared
//! crate once it actually builds and someone can verify the merge.
//!
//! ## Intended structure (not implemented below — see the `todo!()`s)
//!
//! - One `NSWindow` (via `objc2-app-kit`) with an `NSTextField` (PIN,
//!   numeric, 6 digits — mirror `gui.rs`'s `limit: 6`), an "Eşleştir"
//!   `NSButton`, a status `NSTextField` (read-only label), and an
//!   "Eşleşmeyi Bitir" `NSButton` (disabled until paired).
//! - On "Eşleştir": spawn `bacak-remote-server --hardware-encode --pin
//!   <n> --no-gui` (`std::process::Command`, piping its stdout so this
//!   window can parse `tracing`'s log lines for "paired successfully" /
//!   "gave wrong PIN" — see that binary's `network.rs` for the exact
//!   strings — and update the status label; no structured
//!   `SessionStatus` channel exists across a process boundary, so this
//!   is deliberately cruder than `gui.rs`'s `StatusChannel`).
//!   `--hardware-encode` is very likely wrong for macOS as a *default*
//!   (no VideoToolbox backend exists in `encode_h264.rs` — it only tries
//!   NVENC/AMF/QSV, none of which exist on Apple Silicon or most Intel
//!   Macs — so it would just fall through to the `libx264` software
//!   path every time; decide deliberately whether that's worth the extra
//!   CPU over plain `RawZstd` once this can actually be tested, don't
//!   just carry the flag over from Windows by default).
//! - On "Eşleşmeyi Bitir": kill the child process (there's no `Bye`-style
//!   graceful shutdown available across a bare `Command` — see
//!   `network::send_bye` in the shared crate for what a graceful one
//!   would need: access to the live paired session's address/cipher,
//!   which only exists inside that process). A real implementation
//!   should probably send the child SIGTERM and give it a moment before
//!   `.kill()`, not jump straight to `.kill()`.
//! - macOS-specific permission reality (untested — see `../README.md`
//!   and `../packaging/build_mac.sh`'s TODOs): the *child* process is
//!   the one that actually calls into `scrap` (CoreGraphics capture) and
//!   `enigo` (CGEvent injection), so it's the child, not this launcher,
//!   that macOS will gate behind Screen Recording / Accessibility TCC
//!   permissions and prompt the user for on first use.

fn main() -> anyhow::Result<()> {
    todo!(
        "objc2-app-kit NSApplication/NSWindow setup — nothing here has been \
         compiled or run against real AppKit, see this file's module doc \
         and ../README.md's \"Known gap\" before writing real code here."
    )
}
