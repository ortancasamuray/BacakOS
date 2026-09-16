# Altay

🌐 [Türkçe](README.tr.md) · **English**

A modern, **touch-friendly, sandboxed** Linux file manager — Wayland-first,
written in Rust with a [Slint](https://slint.dev) GPU-rendered UI. It runs
entirely as a normal user and **never requires root**.

## Highlights

- **Sandboxed by design** — only your home, mounted removable/external devices,
  and network shares are reachable; system directories are off-limits. Symlink-
  and `..`-escape safe. (See [ARCHITECTURE.md](ARCHITECTURE.md).)
- **Archives** — zip/jar/apk, tar(.gz/.bz2/.xz/.zst), gz/bz2/xz/zst, 7z
  (read+write), RAR (incl. RAR5 + multi-volume), DEB, RPM, and split
  `.001/.002…` volumes; password extraction; zip-slip protection.
- **Devices** — live udisks2 hotplug, mount/unmount of removable media.
- **Network** — SMB/SFTP/FTP/WebDAV/NFS via gvfs, with a credential dialog and
  passwords stored in the system keyring.
- **Preview** — images (cached thumbnails), PDF first page, video frame, text.
- **Transfers** — background copy/move queue with pause/resume/cancel.
- **Search** — instant on-disk-cached filename index + optional content search.
- **Touch & keyboard** — tap, double-tap, long-press menu, drag-to-move, grid
  pinch-substitute zoom, plus full keyboard shortcuts.
- **Dark/light themes**, clickable breadcrumb, context menu, trash with restore.

## Build & run

```sh
cargo run            # debug
cargo build --release
cargo test           # 37 unit tests
```

## Install

```sh
sudo make install        # system-wide (/usr/local)
make install-user        # current user (~/.local), no root
```

Or build a Debian package (`target/debian/altay_*.deb`):

```sh
scripts/build-deb.sh            # builds the .deb (installs cargo-deb if needed)
scripts/build-deb.sh --lint     # also run lintian
scripts/build-deb.sh --install  # build and install via apt
make deb                        # equivalent to: build-deb.sh --lint
```

The package declares its linked libraries automatically plus the dlopen-ed
Wayland/GL libs as **Depends**, and the optional helper tools (gvfs, udisks2,
polkit, portal, poppler, ffmpeg, keyring, wl-clipboard) as **Recommends/Suggests**.
It ships a man page (`altay(1)`) and a changelog, and is **lintian-clean**.

## Optional runtime helpers

Features degrade gracefully when their helper is missing:

| Feature | Needs |
|---|---|
| PDF preview | `poppler-utils` (pdftoppm/pdftocairo) or `ghostscript` |
| Video thumbnails | `ffmpegthumbnailer` or `ffmpeg` |
| Network mounts | `gvfs` (`gio`) |
| Permission elevation | `pkexec` (PolicyKit) |
| Cross-app file copy | `wl-clipboard` (or `xclip`) |
| Device mount/unmount, hotplug | `udisks2` |
| Saved network passwords | a Secret Service keyring |
| Import (file chooser) | `xdg-desktop-portal` |

## Project

- Website: <https://anadolupanteri.org.tr>
- Contact: <bilgi@anadolupanteri.org.tr>

## License

GPL-3.0-or-later — see [COPYING](COPYING).
