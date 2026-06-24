# Altay — Architecture & Roadmap

🌐 [Türkçe özet](ARCHITECTURE.tr.md) · **English**

A modern, touch-friendly, **sandboxed** Linux file manager. Wayland-first,
written in Rust with a [Slint](https://slint.dev) GPU-rendered UI. Runs entirely
as a normal user — **never root**.

## Security model (the core idea)

Every path that crosses the UI→backend boundary is funnelled through
`security::Sandbox`. There is no other way to touch the disk.

* **Allow-list of roots**, discovered at startup (`security::roots`):
  * the user's home directory,
  * removable/external mounts (`/media/$USER`, `/run/media/$USER`, `/mnt`),
  * network mounts exposed by gvfs (`$XDG_RUNTIME_DIR/gvfs`).
* **Deny-list of system prefixes** checked even against canonical paths:
  `/root /etc /usr /var /boot /proc /sys /dev /bin /sbin /lib …`
* **Symlinks are resolved before checking** (`std::fs::canonicalize`), so a
  symlink inside `~` that points at `/etc` cannot be used to escape. Covered by
  `symlink_escape_is_blocked`.
* **`..` traversal** collapses during canonicalisation; the synthetic tail used
  for *not-yet-existing* paths (`resolve_for_create`) rejects `.`/`..`
  components. Covered by `dotdot_escape_is_blocked` /
  `create_with_dotdot_tail_is_blocked`.
* A validated path is represented by `SafePath`, which **only the sandbox can
  construct** — "validated" is therefore un-forgeable elsewhere in the code.

Run the guarantees: `cargo test` (7 tests, security-critical ones included).

## Module map (`src/`)

| Module        | Status   | Responsibility |
|---------------|----------|----------------|
| `security`    | ✅ done   | Sandbox, allow/deny policy, root discovery, `SafePath`. |
| `filesystem`  | ✅ done   | Listing, natural sort, copy/move/rename/create/duplicate with progress. |
| `trash`       | ✅ done   | freedesktop Trash via `trash` crate; trash / list / **restore to original** (sandbox-checked target) / **empty**, with a Trash sidebar view + Restore/Empty actions. |
| `search`      | ✅ done   | Live in-memory filename **index** (background build + `notify` incremental updates) **persisted to disk** for instant next-launch startup; **content search** (read text-like files, size-capped, binary-skipping); name/ext/size/date/category filters; recursive-walk fallback. |
| `permissions` | ✅ done   | Read & change POSIX mode bits, shown in preview with a chmod control; **PolicyKit elevation** (`pkexec chmod`) when the user doesn't own the file — still sandbox-bounded, app never runs as root. |
| `desktop`     | ✅ done   | reveal-in-folder + cross-app clipboard (file launching now goes through `exo`). |
| `exo`         | ✅ done   | **exo-utils**: application-association & file-opener infrastructure — MIME detection (`xdg-mime` + extension fallback), desktop-entry parsing, `Exec` field-code expansion, apps-for-a-type, default open / open-with / set-default. Powers both double-click open **and the "Open With…" dialog** (context menu → app list **with icons** resolved from the freedesktop icon theme, default marked, optional set-as-default). |
| `portal`      | ✅ done   | xdg-desktop-portal **FileChooser** via `ashpd` — "Import…" lets the user pick files outside the sandbox (portal-authorized) and copies them into the current folder. |
| drag & drop   | ✅ done   | In-app: press-drag lifts item(s) with a floating ghost; tap a folder/place to drop (move). Cross-app: Copy/Cut/Paste exchange files with other file managers via the system clipboard (`x-special/gnome-copied-files` over `wl-copy`/`wl-paste`, `xclip` fallback). Targeting uses tap + has-hover (avoids Slint's absolute-position binding loop). |
| `devices`     | ✅ done   | **live udisks2 hotplug** over zbus rebuilds the sidebar; **mount/unmount** of removable/external volumes via typed UDisks2 D-Bus proxies (`Filesystem.Mount`/`Unmount`), surfaced as a DEVICES section. System partitions (`HintSystem`) are never shown. |
| `network`     | ✅ done   | gvfs mount/unmount via `gio`; URI/path-bar connect + **credential dialog**; passwords in the **system keyring** (Secret Service, never in config); saved connections in a NETWORK sidebar section. |
| `archive`     | ✅ done   | zip/jar/apk, tar(.gz/.bz2/.xz/.zst), single gz/bz2/xz/zst, 7z (read+write); **RAR** (unrar, incl. RAR5 + multi-volume), **DEB** (ar+tar), **RPM** (header parse + cpio); **split multi-volume** `.001/.002…` auto-combined; password extract; magic detection; zip-slip guard. |
| `preview`     | ✅ done   | Preview pane: image (freedesktop thumbnail cache) + **PDF first-page** (pdftoppm→pdftocairo→gs) + **video frame** (ffmpegthumbnailer→ffmpeg) + text + info, all cached; tap=preview, double-tap=open. |
| `transfer`    | ✅ done   | Background copy/move queue: pause/resume/cancel, live progress, copy/cut/paste clipboard, sandbox-validated. |
| `ui` (`ui/main.slint`) | ✅ done | Sidebar (places/devices/network), **clickable breadcrumb** (root-relative segments; ✎ toggles an editable path/URI field), grid/list/compact, **zoomable grid** (Ctrl+wheel & ＋/－, persisted), touch selection, preview pane, transfer panel, context menu (**right-click or touch long-press**) with open/copy/cut/rename/compress/extract/trash + rename dialog; **keyboard shortcuts** (FocusScope); **icon-only toolbar buttons with hover tooltips** (custom `IconButton` + `Tip` global overlay); **settings** dialog (show-hidden, **dark/light theme**, default view, **language: English/Türkçe/Español**) persisted to config. UI strings (chrome via the `L` Slint global, and status-bar messages via a Rust `sx()` helper) are localized in **English, Turkish and Spanish** — live switch, auto-detected from the system locale (`LANG`/`LC_*`). |

## UI

`ui/main.slint` — custom `Theme` global with a runtime **dark/light** palette,
rounded corners, 48px touch targets, checkbox selection mode, grid/list/compact
views. The controller (`src/main.rs`) owns `AppState` and bridges Slint callbacks
to the backend. Preferences (hidden files, theme, default view, grid zoom) live
in `$XDG_CONFIG_HOME/altay/config.toml`.

Touch/gesture mapping: **single tap** = select + preview, **double tap** = open,
**long-press** (500 ms `Timer`) = context menu, **press-drag** = move (with a
floating ghost), **Ctrl+wheel / ＋－** = grid zoom. Two-finger pinch and OS-level
cross-app drag aren't exposed by Slint 1.16 (see note under the roadmap).

Keyboard shortcuts (a `FocusScope` wrapping the main pane, `forward-focus` from
the window): **Delete** trash, **Backspace** up, **F2** rename, **F5** refresh,
**Esc** cancel/clear selection, **Ctrl+C/X/V** copy/cut/paste, **Ctrl+A** select
all, **Ctrl+L** edit path, **Ctrl+F** focus search.

## Roadmap (phased) — all eight phases landed

1. **✅ Foundation** — sandbox + tests, filesystem core, trash, search,
   permissions, module scaffolds, working Slint GUI (grid/list/compact,
   selection mode, new folder, trash).
2. **✅ Archive backends** — zip/jar/apk, tar(.gz/.bz2/.xz/.zst), single
   compressors, 7z (read+write), **RAR** (RAR5 + multi-volume), **DEB**, **RPM**,
   **split multi-volume** (`.001/.002…`); password extract; magic detection;
   zip-slip guard.
3. **✅ Devices & hotplug** — live udisks2 monitoring over `zbus` rebuilds the
   sidebar; **mount/unmount** of removable/external volumes via UDisks2 D-Bus.
4. **✅ Network** — gvfs mount/unmount via `gio`; URI connect + **credential
   dialog**; passwords in the **system keyring**; saved connections sidebar.
5. **✅ Preview & thumbnails** — preview pane (image w/ freedesktop thumbnail
   cache, **PDF first-page via poppler/gs**, **video frame via
   ffmpegthumbnailer/ffmpeg**, text, info).
6. **✅ Transfers UI** — pause/resume/cancel queue, copy/cut/paste clipboard,
   live progress bars.
7. **✅ Desktop integration** — `xdg-open` launch via portal, permissions in
   preview with chmod + **PolicyKit elevation** (`pkexec`), in-app **drag & drop**
   (lift + drop-on-folder/place), **cross-app file exchange** via the system
   clipboard (gnome-copied-files), and the **file-chooser portal** ("Import…"
   pulls portal-authorized external files into the sandbox). *Note:* OS-level
   drag *gestures* across apps aren't exposed by Slint/winit; clipboard is the
   portable substitute.
8. **✅ Indexed search** — live in-memory filename index, background build,
   `notify` incremental updates, walk fallback, **on-disk persistence** (instant
   next-launch start), and **content search** (in-file text matching).

Every phase ships with unit tests (**37 total**); the security-critical and
archive/transfer/index/preview/desktop/rename behaviours are all covered.

Two spec gestures aren't exposed by Slint 1.16 and use portable substitutes:
**OS-level cross-app drag** (no external DnD → system clipboard /
gnome-copied-files) and **two-finger pinch-zoom** (no multitouch/pinch handler →
Ctrl+wheel and ＋/－ zoom buttons drive the same `grid-scale`). Several features need a runtime
helper: poppler/ghostscript (PDF), ffmpeg/ffmpegthumbnailer (video), `pkexec`
(elevation), wl-clipboard/xclip (cross-app clipboard), udisks2 (mount), a Secret
Service keyring (network passwords), and xdg-desktop-portal (import) — each
degrades gracefully when absent.

## Build, run & install

```sh
cargo test               # verify security + logic (37 tests)
cargo run                # launch (Wayland native, or X11 via DISPLAY)
sudo make install        # system-wide: binary + altay.desktop + hicolor icon
make install-user        # per-user install under ~/.local (no root)
cargo deb                # build a .deb (needs cargo-deb)
```

Packaging files: `altay.desktop` (FileManager entry, handles `inode/directory`),
`assets/altay.png` (256×256 logo icon — also the window icon; regenerate from the
source `assets/Altay.png` with `cargo run --example mkicon`), `Makefile`
(PREFIX/DESTDIR-aware install/uninstall), and `[package.metadata.deb]` in
`Cargo.toml`. See `README.md`.
