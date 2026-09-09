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
(cd "$script_dir" && makensis installer.nsi)

echo "Installer ready: $script_dir/bacak-remote-server-setup.exe"
