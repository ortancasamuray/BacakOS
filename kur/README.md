# kur — BacakOS System Installer

🌐 [Türkçe](README.tr.md) · **English**

`kur` ("install" in Turkish) is BacakOS's graphical system installer for
Debian 13 (Trixie): a Slint UI wizard — locale, timezone, disk partitioning,
account creation, install — over a pure-Rust backend with no shell-out for
anything safety-critical. See [ARCHITECTURE.md](ARCHITECTURE.md) for the
module map and threading model.

## Why a software renderer

The UI uses Slint's software renderer exclusively — the only one guaranteed
to work in a live image with no GPU driver loaded, which is exactly where an
installer runs. It shares its compiled Slint dependency with `altay`.

## Build & run

```sh
cargo build --release
cargo test                    # backend is unit-tested independent of any UI/display
```

```sh
KUR_HEADLESS=1 ./target/release/kur   # preseed-style unattended install, env-var driven — no window
```

## Launching on a live image

Run via **`kur-baslat`**, never the binary directly:

```sh
/usr/bin/kur-baslat
```

`kur` partitions disks, so it needs root — but `pkexec` doesn't work here:
its `auth_admin` polkit policy wants a real PAM password, and the live
session's autologin user has none. `kur-baslat` re-execs through `sudo`
instead (the live user already has passwordless sudo), forwarding
`WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`/`DISPLAY` as arguments since `sudo`
strips them, and `kur-root` (its root-side counterpart) opens the user's
Wayland socket in `/run/user/<uid>` using root's ability to reach it
regardless of file permissions.

## Testing against a real disk

```sh
scripts/test-vm-boot.sh       # boots the live image in a VM
scripts/test-vm-install.sh    # runs a full install against a VM loop device
```

Unit tests cover validation and planning logic (hostname/account rules,
locale/timezone parsing, partition planning) without touching a block
device; the VM scripts are what actually exercise `backend::install` against
real (virtual) hardware.

## Packaging

```sh
dpkg-buildpackage -us -uc -b
```

See `debian/changelog` and `debian/lintian-overrides`.

## License

GPL-3.0-or-later.
