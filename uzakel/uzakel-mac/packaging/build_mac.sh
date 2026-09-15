#!/usr/bin/env bash
# Cross-compiles the shared bacak-remote-server (../../uzakel-windows) for
# macOS and bundles it into a minimal .app.
#
# SKELETON — NEVER RUN SUCCESSFULLY, NOT EVEN ONCE. Written from reading
# `cargo`/`lipo`/`.app` bundle conventions, not verified against a real
# build. Known-missing piece: a real Apple `cc` (osxcross, or run this on
# an actual Mac) — the shared crate's `zstd-sys` C dependency already
# fails to compile with the Linux host's `cc` (see ../README.md's "Known
# gap"), before this script's own steps are even reached. Fix that first;
# this script's `cargo build` line is where it will start passing.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server_dir="$script_dir/../../uzakel-windows"
app_name="Bacak Remote Server"
bin_name="bacak-remote-server"
out_dir="$script_dir/dist"

# Universal binary (Apple Silicon + Intel) — TODO: confirm both targets are
# actually desired before shipping; an Apple-Silicon-only build is simpler
# (skip the `lipo` step, ship the aarch64-apple-darwin binary directly) if
# nobody needs to support an Intel Mac.
targets=(aarch64-apple-darwin x86_64-apple-darwin)

for target in "${targets[@]}"; do
    rustup target add "$target" >/dev/null 2>&1 || true
    ( cd "$server_dir" && cargo build --release --target "$target" -p "$bin_name" )
done

rm -rf "$out_dir"
bundle_dir="$out_dir/$app_name.app"
mkdir -p "$bundle_dir/Contents/MacOS"

# TODO: verify `lipo` is actually available in whatever environment
# finally runs this (it ships with Xcode Command Line Tools on real macOS;
# osxcross also provides it). Not available to test here.
lipo -create -output "$bundle_dir/Contents/MacOS/$bin_name" \
    "$server_dir/target/aarch64-apple-darwin/release/$bin_name" \
    "$server_dir/target/x86_64-apple-darwin/release/$bin_name"
chmod +x "$bundle_dir/Contents/MacOS/$bin_name"

sed "s/__BIN_NAME__/$bin_name/g" "$script_dir/Info.plist.template" > "$bundle_dir/Contents/Info.plist"

# TODO, before this is a real distributable .app:
# - Code signing (`codesign --sign <identity> --options runtime`) and
#   notarization (`xcrun notarytool submit`) — an unsigned .app downloaded
#   from the internet gets Gatekeeper-blocked on a real Mac by default.
# - Screen-recording + accessibility (input injection) permission prompts:
#   macOS gates both `scrap`'s CoreGraphics capture and `enigo`'s CGEvent
#   injection behind user-granted TCC permissions (System Settings ->
#   Privacy & Security), which an app bundle needs an `Info.plist`
#   usage-description entry for and the OS will prompt for on first use —
#   neither investigated here.
# - An actual app icon (.icns) — see ../../uzakel-windows/bacak-remote-server
#   /resources/app.ico's sibling for the Windows equivalent; needs a
#   macOS-appropriate design pass, not just format-converting that one.

echo "Bundled (unsigned, untested): $bundle_dir"
