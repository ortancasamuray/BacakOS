#!/usr/bin/env bash
# Builds the shared bacak-remote-server (../../uzakel-windows) AND
# gui-launcher (../gui-launcher) for macOS and bundles both into one .app
# — CFBundleExecutable is the launcher (so Finder/double-click runs the
# PIN window, not the console `--pin` path), with the real server sitting
# right next to it in Contents/MacOS/ where `gui-launcher`'s
# `find_server_binary()` already looks first.
#
# Verified end-to-end on a real Mac, 2026-09-15: this exact script's
# bundle, launched via `open` (simulating a Finder double-click), shows
# the pairing window, spawns the bundled server as a sibling, and streams
# real video/input to BacakOS — see ../README.md for the full writeup,
# including the one real snag (TCC's responsible-process inheritance to
# the spawned child needing the bundle to actually be code-signed, even
# just ad-hoc — see the `codesign` step below).
#
# Cross-compiling *from Linux* (osxcross) is still unverified — the
# shared crate's `zstd-sys` C dependency fails against a plain Linux `cc`
# (see ../README.md's "Known gap"). Run this ON a real Mac.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server_dir="$script_dir/../../uzakel-windows"
launcher_dir="$script_dir/../gui-launcher"
app_name="Bacak Remote Server"
server_bin="bacak-remote-server"
launcher_bin="bacak-remote-server-gui-launcher"
out_dir="$script_dir/dist"

# Universal binary (Apple Silicon + Intel) — TODO: confirm both targets are
# actually desired before shipping; an Apple-Silicon-only build is simpler
# (skip the `lipo` step, ship the aarch64-apple-darwin binaries directly)
# if nobody needs to support an Intel Mac.
targets=(aarch64-apple-darwin x86_64-apple-darwin)

for target in "${targets[@]}"; do
    rustup target add "$target" >/dev/null 2>&1 || true
    ( cd "$server_dir" && cargo build --release --target "$target" -p "$server_bin" )
    ( cd "$launcher_dir" && cargo build --release --target "$target" )
done

rm -rf "$out_dir"
bundle_dir="$out_dir/$app_name.app"
mkdir -p "$bundle_dir/Contents/MacOS" "$bundle_dir/Contents/Resources"

cp "$script_dir/app.icns" "$bundle_dir/Contents/Resources/app.icns"

# TODO: verify `lipo` is actually available in whatever environment
# finally runs this (it ships with Xcode Command Line Tools on real macOS;
# osxcross also provides it) — was available and worked on the real Mac
# this was verified on.
lipo -create -output "$bundle_dir/Contents/MacOS/$server_bin" \
    "$server_dir/target/aarch64-apple-darwin/release/$server_bin" \
    "$server_dir/target/x86_64-apple-darwin/release/$server_bin"
chmod +x "$bundle_dir/Contents/MacOS/$server_bin"

lipo -create -output "$bundle_dir/Contents/MacOS/$launcher_bin" \
    "$launcher_dir/target/aarch64-apple-darwin/release/$launcher_bin" \
    "$launcher_dir/target/x86_64-apple-darwin/release/$launcher_bin"
chmod +x "$bundle_dir/Contents/MacOS/$launcher_bin"

sed "s/__BIN_NAME__/$launcher_bin/g" "$script_dir/Info.plist.template" > "$bundle_dir/Contents/Info.plist"

# Ad-hoc signing (no real Developer ID needed) turned out to be required,
# not optional, for TCC to work at all here — found on the real Mac,
# 2026-09-15: with the bundle left unsigned, granting "Bacak Remote
# Server" Screen Recording in System Settings didn't stop `capture.rs`
# failing (the actual CoreGraphics call happens in the *child*
# `bacak-remote-server` process gui-launcher spawns, and TCC's
# responsible-process inheritance from parent to child apparently needs
# the parent to have a real code identity — an entirely unsigned bundle
# doesn't give it one). Re-signing with `--sign -` fixed it immediately
# (after re-granting Screen Recording once more under the new identity).
codesign --sign - --deep --force "$bundle_dir"

# TODO, before this is a real distributable .app:
# - *Real* code signing (an actual Developer ID, `--options runtime`) and
#   notarization (`xcrun notarytool submit`) — ad-hoc (`--sign -`, above)
#   was enough to fix TCC locally, but an ad-hoc-signed .app downloaded
#   from the internet still gets Gatekeeper-blocked on a real Mac by
#   default (unlike a properly Developer-ID-signed and notarized one).
# - Screen-recording + accessibility (input injection) permission prompts:
#   macOS gates both `scrap`'s CoreGraphics capture and `enigo`'s CGEvent
#   injection behind user-granted TCC permissions (System Settings ->
#   Privacy & Security). `Info.plist.template` already carries
#   `NSScreenCaptureUsageDescription`; whether that's enough for THIS
#   bundle (launcher process spawning the actual capturing process as a
#   child) or whether the child needs its own entry too hasn't been
#   checked against a real permission prompt yet.

echo "Bundled and ad-hoc signed: $bundle_dir"
