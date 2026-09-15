#!/usr/bin/env bash
# Cross-compiles bacak-remote-server for Windows (mingw-w64) and packages it
# into a real NSIS installer .exe. Run from anywhere; paths are relative to
# this script. Needs: `rustup target add x86_64-pc-windows-gnu`, the
# `mingw-w64` and `nsis` system packages, and the workspace's
# `.cargo/config.toml` (already checked in) pointing the linker at
# `x86_64-w64-mingw32-gcc-posix` with `crt-static` so the resulting .exe
# needs no MinGW runtime DLLs on the target machine.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$script_dir/../../.."

cd "$workspace_root"
cargo build --release --target x86_64-pc-windows-gnu -p bacak-remote-server

cp "target/x86_64-pc-windows-gnu/release/bacak-remote-server.exe" "$script_dir/"

# `--hardware-encode` (see ../../../HARDWARE_ENCODE_PLAN.md) links these
# dynamically — unlike the mingw runtime, they're not `crt-static`-able,
# so they have to ship next to the .exe or the installed app instantly
# exits with no error message the moment that flag's encoder path runs
# (this exact failure was hit and diagnosed testing on real hardware).
cp vendor/ffmpeg-n9.0-latest-win64-gpl-shared-9.0/bin/*.dll "$script_dir/"

(cd "$script_dir" && makensis installer.nsi)

echo "Installer ready: $script_dir/bacak-remote-server-setup.exe"
