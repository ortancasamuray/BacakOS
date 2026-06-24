# Bacak OS — System Architecture

> A next-generation desktop environment: Rust core, Tauri shell, React + Tailwind frontend, async GPU-friendly compositor, virtual archive filesystem, and a hybrid floating + snap window manager.

---

## 1. High-level topology

```
┌─────────────────────────────────────────────────────────────────────┐
│                       Frontend (React + TS + Tailwind)              │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌──────────────┐   │
│  │ Compositor  │ │   Dock      │ │  On-screen  │ │ File Manager │   │
│  │  / WM UI    │ │             │ │  Keyboard   │ │              │   │
│  └─────────────┘ └─────────────┘ └─────────────┘ └──────────────┘   │
│                          ▲    Tauri IPC (commands + events)        │
└──────────────────────────┼──────────────────────────────────────────┘
                           ▼
┌─────────────────────────────────────────────────────────────────────┐
│                 Tauri Runtime  (Rust + WebView2/WKWebView/WebKitGTK)│
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │                    bacak-core   (binary crate)                │  │
│  │  Command dispatcher · Event bus · Permission broker           │  │
│  └───────────────────────────────────────────────────────────────┘  │
│  ┌────────────┬────────────┬────────────┬────────────┬───────────┐  │
│  │   bacak-   │   bacak-   │   bacak-   │   bacak-   │  bacak-   │  │
│  │     fs     │  archive   │   search   │    wm      │  preview  │  │
│  └────────────┴────────────┴────────────┴────────────┴───────────┘  │
│  ┌────────────┬────────────┬────────────┬─────────────────────────┐ │
│  │   bacak-   │   bacak-   │   bacak-   │       bacak-input       │ │
│  │   device   │  session   │  notify    │   (OSK · gestures)      │ │
│  └────────────┴────────────┴────────────┴─────────────────────────┘ │
│                           Tokio runtime (async)                     │
└─────────────────────────────────────────────────────────────────────┘
                           ▼
                    OS / Kernel surfaces
   (filesystem · libinput · pipewire · wpa_supplicant · upower · ...)
```

---

## 2. Cargo workspace layout

```
bacak/
├─ Cargo.toml                 # workspace
├─ src-tauri/
│  ├─ Cargo.toml              # bacak-core (bin)
│  ├─ tauri.conf.json
│  └─ src/
│     ├─ main.rs              # tauri::Builder, plugin & service registration
│     ├─ commands/            # #[tauri::command] surfaces, thin shims
│     ├─ events.rs            # typed event channel definitions
│     └─ permissions.rs       # capability tokens per window
├─ crates/
│  ├─ bacak-fs/               # async FS, watchers, VFS abstraction
│  ├─ bacak-archive/          # ZIP/RAR/7Z/TAR/GZ/ISO via libarchive-rs + sevenz-rust
│  ├─ bacak-search/           # tantivy index, content + metadata
│  ├─ bacak-wm/               # window state, snap zones, workspaces
│  ├─ bacak-preview/          # MIME sniffing, thumbnail pipeline (image, video, doc)
│  ├─ bacak-device/           # network (NM dbus), battery (upower), audio (pipewire)
│  ├─ bacak-session/          # workspace persistence, restore, multi-monitor
│  ├─ bacak-notify/           # notifications, badges, do-not-disturb
│  └─ bacak-input/            # OSK state machine, gesture recognizer
└─ ui/
   ├─ package.json            # vite + react + ts + tailwind + framer-motion
   └─ src/
      ├─ shell/               # Compositor, Dock, OSK, TaskSwitcher
      ├─ apps/                # Files, Firefox-shim, Terminal, Settings
      ├─ design/              # tokens.ts, motion.ts, primitives/
      └─ bridge/              # typed wrappers over tauri::invoke
```

Workspace `Cargo.toml` pins one tokio + one tracing version across crates and gates platform-specific deps behind cfg flags.

---

## 3. Service modules — surfaces

Each service crate exposes (1) a pure-Rust API consumable from Rust, and (2) a thin `#[tauri::command]` adapter registered in `bacak-core`. Frontend calls always go through the adapter; never through the raw FS or shell.

### 3.1 `bacak-fs` — File System Service

```rust
pub trait Vfs: Send + Sync {
    async fn read_dir(&self, path: &VfsPath) -> Result<Vec<DirEntry>>;
    async fn open(&self, path: &VfsPath) -> Result<VfsFile>;
    async fn metadata(&self, path: &VfsPath) -> Result<Metadata>;
    async fn watch(&self, path: &VfsPath, cb: WatchSink) -> Result<WatchHandle>;
    async fn copy(&self, from: &VfsPath, to: &VfsPath, opts: CopyOpts) -> Result<JobId>;
}

pub enum VfsPath {
    Native(PathBuf),
    Archive { archive: PathBuf, inside: PathBuf },  // arch.tar.gz!/inside/foo.txt
    Remote { scheme: String, url: String },         // sftp://, smb://, ...
}
```

`watch` uses `notify` + a debouncer; events are coalesced and emitted on the Tauri event bus as `vfs:changed`. Heavy ops (`copy`, `move`, `delete`) return a `JobId`; progress is streamed on `vfs:job-progress`.

### 3.2 `bacak-archive` — Virtual archive filesystem

Archives behave like directories. Backed by:

| Format          | Backend                                |
| --------------- | -------------------------------------- |
| ZIP             | `zip` crate                            |
| TAR / .tar.gz   | `tar` + `flate2`                       |
| 7Z              | `sevenz-rust`                          |
| RAR             | `unrar` (read-only, dynamic link)      |
| ISO             | `iso9660` crate                        |
| Generic         | `compress-tools` (libarchive) fallback |

```rust
pub struct ArchiveHandle { /* lazily decoded directory tree */ }

impl ArchiveHandle {
    pub async fn open(path: &Path) -> Result<Self>;
    pub fn list(&self, inside: &Path) -> &[Entry];      // O(1), cached
    pub async fn stream(&self, inside: &Path) -> Result<AsyncRead>;
    pub async fn extract(&self, inside: &Path, to: &Path) -> Result<JobId>;
}
```

The VFS layer routes any `VfsPath::Archive` through this handle; the file manager UI never knows whether the directory it's rendering is on disk or inside a `.7z`.

### 3.3 `bacak-search` — Indexing & query

`tantivy`-backed inverted index, one segment per user-mounted root. Documents carry `(path, name, ext, mtime, size, mime, content_excerpt, archive_parent)` fields. The indexer is a Tokio task pool that dequeues from the FS watcher stream and respects a configurable I/O budget.

Frontend issues `search.query { q, scope, filters }` → ranked stream of results delivered as `search:hit` events with a `query_id`.

### 3.4 `bacak-wm` — Window Management

Authoritative window state lives in Rust. The frontend renders, but state mutations (move, resize, snap, focus, workspace assignment) round-trip through the WM crate so multi-window invariants hold even when several React roots are alive.

```rust
pub struct Window {
    pub id: WindowId,
    pub app: AppId,
    pub geom: Rect,         // logical CSS pixels
    pub state: WinState,    // Floating | Snapped(Zone) | Maximized | Minimized | Fullscreen
    pub workspace: WorkspaceId,
    pub z: u32,
    pub focused: bool,
}

pub enum SnapZone { Left, Right, Top, BottomLeft, BottomRight, TopLeft, TopRight }
```

Snap zones are computed against the monitor's *work area* (screen minus dock + reserved struts). The 24 px edge threshold and the spring curve (`cubic-bezier(0.34, 1.56, 0.64, 1)`) live in the crate so OSK and WM agree on motion.

### 3.5 `bacak-preview` — MIME & thumbnails

- MIME via magic bytes (`infer`) with extension fallback.
- Image thumbs: `image` crate, downscaled with Lanczos, cached at `~/.cache/bacak/thumbs/`.
- Video: `ffmpeg-next` or `gstreamer` for a single representative frame at 10% mark.
- PDF / Office: `pdfium-render` for PDFs; office docs deferred to LibreOffice headless when available.

Frontend asks for a thumbnail by VFS path + target size; the crate returns a stable URL (`asset://thumb/<hash>.webp`) that Tauri serves from cache.

### 3.6 `bacak-device` — Network · battery · audio

| Concern  | Linux backend           | macOS / Windows                    |
| -------- | ----------------------- | ---------------------------------- |
| Network  | NetworkManager via dbus | `SCNetworkConfiguration` / WMI     |
| Battery  | `upower` via dbus       | `IOPMCopyBatteryInfo` / WMI        |
| Audio    | PipeWire / PulseAudio   | CoreAudio / WASAPI                 |

Each surface is exposed as a stream of typed events (`net:status`, `bat:level`, `audio:volume`); the dock subscribes once at boot.

### 3.7 `bacak-session` — Workspaces & persistence

Persists, per user:

- Workspace layout (windows, geometry, focused, z-order)
- Pinned dock apps
- Recent files / archive bookmarks
- OSK preferences (layout, predictive, position)

Stored as TOML at `~/.config/bacak/session.toml`; written atomically through a tempfile + rename.

### 3.8 `bacak-notify` — Notifications

Bridges to `org.freedesktop.Notifications` on Linux and the OS-native equivalents elsewhere. Apps push via Tauri command; the dock receives badge updates via `notify:badge { app_id, count }`.

### 3.9 `bacak-input` — OSK & gestures

- **OSK state machine.** States: `Closed → Opening → Open → Closing`. Triggers: input focus events (from the WebView), explicit toggle, hardware keyboard detected (auto-close). Layout is layout-aware: when the OSK opens, it emits `wm:reserve-area` to the WM, which reduces the focused window's effective viewport so content above the keyboard stays visible.
- **Gestures.** `libinput` taps for trackpad swipes (3-finger horizontal → workspace, 4-finger up → task overview). Touchscreen long-press → multi-select (file manager). Edge-reveal hit zones are 6 px tall along the bottom.

---

## 4. IPC — Tauri command catalog (excerpt)

Commands are named `<service>.<verb>`; events are `<service>:<noun>`. All payloads are `serde_json` and validated at the Rust boundary.

```rust
// fs
#[tauri::command] async fn fs_read_dir(path: VfsPath) -> Result<Vec<DirEntry>, FsError>;
#[tauri::command] async fn fs_copy(from: VfsPath, to: VfsPath) -> Result<JobId, FsError>;

// archive
#[tauri::command] async fn arc_open(path: PathBuf) -> Result<ArchiveSnapshot, ArcError>;
#[tauri::command] async fn arc_extract(path: PathBuf, inside: PathBuf, to: PathBuf) -> Result<JobId, ArcError>;

// wm
#[tauri::command] async fn wm_move(id: WindowId, rect: Rect) -> Result<(), WmError>;
#[tauri::command] async fn wm_snap(id: WindowId, zone: SnapZone) -> Result<(), WmError>;
#[tauri::command] async fn wm_focus(id: WindowId) -> Result<(), WmError>;

// input
#[tauri::command] async fn osk_open(target_window: WindowId, target_rect: Rect) -> Result<(), InputError>;
#[tauri::command] async fn osk_close() -> Result<(), InputError>;
```

Long-running commands return a `JobId`; the caller subscribes to `job:progress { id, pct, eta }` and `job:done { id, result }`.

---

## 5. Frontend architecture

### 5.1 Stack

- **Vite + React 18 + TypeScript** (Svelte is a swap-in option — components are framework-light)
- **TailwindCSS** with a custom token preset (see `DESIGN_SYSTEM.md`)
- **Framer Motion** for spring physics; matches the WM crate's curve
- **Zustand** for client-side ephemeral state; **Tauri events** for server-of-record state

### 5.2 Module boundaries

```
ui/src/
├─ shell/
│  ├─ Compositor.tsx       # mounts <WindowFrame> per backend Window
│  ├─ WindowFrame.tsx      # title bar, glow, resize handle, snap preview hook
│  ├─ Dock.tsx             # magnification, indicators, tray
│  ├─ OnScreenKeyboard.tsx # layout-aware, requests wm:reserve-area
│  ├─ TaskSwitcher.tsx     # alt-tab card view
│  └─ Workspaces.tsx       # horizontal pager
├─ apps/                   # each app is a route inside its WindowFrame
├─ design/                 # tokens, primitives, motion curves
└─ bridge/
   ├─ invoke.ts            # typed wrapper: invoke<TCmd, TArgs, TResp>(...)
   └─ events.ts            # subscribe<TEvt>(...); cleanup on unmount
```

### 5.3 Compositor invariant

The frontend never owns window geometry. On every drag/resize tick it:

1. Optimistically updates `transform: translate3d(...)` locally for 60 fps response.
2. Throttle-emits `wm.move`/`wm.resize` to the backend (16 ms).
3. Reconciles against the backend's authoritative `wm:state` event on `pointerup`.

This keeps state coherent across multi-monitor and prevents drift when the OSK reserves area mid-drag.

---

## 6. Virtual archive filesystem — example data flow

User double-clicks `~/Downloads/release.tar.gz` in the file manager:

1. **UI** calls `fs.read_dir({ Archive: { archive: ".../release.tar.gz", inside: "/" } })`.
2. **`bacak-fs`** sees the `Archive` variant and delegates to **`bacak-archive`**.
3. **`bacak-archive`** lazy-decodes the central directory (no full extraction). Returns `Vec<DirEntry>`.
4. **UI** renders the directory listing with the same component used for native folders — no special case in the file manager.
5. User drags `release/bin/bacak` to `~/Apps/`.
6. **UI** calls `fs.copy(from: Archive{...}, to: Native("~/Apps/bacak"))`.
7. **`bacak-fs`** opens an async read stream from `bacak-archive` and pipes it to a native write — zero intermediate temp file.

---

## 7. Async, concurrency, performance notes

- **Tokio current-thread** for IPC dispatch (low latency), **multi-thread Tokio** for `bacak-fs`, `bacak-archive`, `bacak-search`.
- **Backpressure.** Search and watch streams use bounded channels (`tokio::sync::mpsc` capacity 256); when full, older events are coalesced rather than dropped on the floor.
- **Virtualized lists.** The file manager renders huge directories with `@tanstack/virtual`. The Rust side returns directory entries in pages (`offset`, `limit`) and only sends `(name, ext, size, mtime)` until a thumbnail is actually requested.
- **GPU.** WebView is the renderer; the Aegean background + glass blur stay on the compositor by avoiding `filter: blur` on large scroll areas (we blur a static layer beneath, not the moving content).
- **Memory.** Archive directory trees are cached in an LRU keyed by `(path, mtime, size)` with a global cap (~50 MB).

---

## 8. Security model

- **Capability tokens.** Each Tauri command checks a per-window capability set declared in `permissions.rs`. The file manager has FS + archive + preview; the Firefox shim has only network + clipboard.
- **VFS path normalization.** All `VfsPath` values are normalized and confined to user-mounted roots before any syscall — prevents `..` traversal out of an archive sandbox.
- **No shell-out for archives.** All formats use in-process libraries; no `unzip`/`7z` subprocess. Removes a class of command-injection issues.
- **Notifications can't execute.** Notification action handlers are command IDs the originating app pre-registered; no arbitrary code injection from the notify bus.

---

## 9. Build, test, distribution

- `cargo test --workspace` — unit + integration across crates; `bacak-fs` and `bacak-archive` have golden-fixture tests for each archive format.
- `cargo bench` — `bacak-search` indexing throughput; `bacak-archive` cold-open latency.
- `vitest` + `@testing-library/react` for UI; storybook for primitives.
- **Distribution.** `tauri build` produces a deb/rpm/AppImage on Linux, a notarized `.app`/`.dmg` on macOS, an MSI on Windows. CI fans out via `tauri-action` on GitHub Actions.

---

## 10. Open questions / deferred

- Compositor mode on Linux: stay inside Tauri's WebView for v1, or graduate to a Wayland compositor (`smithay`) for v2? v2 unlocks true GPU window borders and shadow casting between windows.
- Wayland vs X11 input grab semantics for the OSK reserve-area trick — needs prototyping under both.
- ISO 9660 + UDF dual-layer images: pick a single backend or compose.
- Predictive-text model for the OSK: ship a small on-device n-gram in v1, leave hooks for an ONNX language model later.
