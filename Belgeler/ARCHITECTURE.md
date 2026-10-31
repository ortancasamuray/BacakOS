# Belgeler — Architecture

🌐 [Türkçe özet](ARCHITECTURE.tr.md) · **English**

Four independent Rust + Slint binaries, not a shared library with four thin
frontends — each crate owns its full render pipeline. What they share is a
common `[workspace.dependencies]` block (mainly the `slint` version/features)
declared in the root `Cargo.toml`, and, for `bacak-belge`, an in-crate module
that does everything the other three do combined.

---

## 1. Crate layout

```
Belgeler/
├─ Cargo.toml                       # [workspace] members = ["crates/bacak-belge"]
│                                   # (bacak-pdf/epub/resim opt out with their own [workspace])
├─ crates/
│  ├─ bacak-belge/                  # the shipping unified viewer
│  │  ├─ src/main.rs                # PDF (MuPDF) + EPUB (epub crate) + image (image crate)
│  │  │                             # + pen/marker annotation compositing
│  │  └─ ui/main.slint
│  ├─ bacak-pdf/                    # standalone PDF-only viewer (MuPDF)
│  ├─ bacak-epub/                   # standalone EPUB-only viewer
│  └─ bacak-resim/                  # standalone image-only viewer
├─ desktop/                         # *.desktop entries, one per binary
└─ icons/hicolor/scalable/apps/     # one SVG icon per binary
```

`bacak-belge` is the only workspace member and the only one `build-debs.sh`
builds and packages; the other three declare their own `[workspace]` and
have not had `bacak-belge`'s fixes (see §3).

---

## 2. `bacak-belge` — the unified viewer

`src/main.rs` holds the three viewing modes, selected by the opened file's
extension; the logic that doesn't need a window lives in small modules with
unit tests (`cargo test -p bacak-belge`):

| Module | What |
|---|---|
| `cizim.rs` | Pen/highlighter notes as vector strokes, undo, saving |
| `metin.rs` | EPUB chapter (X)HTML → readable text |
| `yerlesim.rs` | Fit/zoom/pan arithmetic, natural file order |

- **PDF** — MuPDF renders the page at `zoom × window scale factor × 96/72`,
  so one image pixel is one physical pixel (sharp on HiDPI); a render that
  would exceed 40 MP is made smaller and scaled up instead. The page opens
  fitted to the window ("Sayfaya sığdır" ↔ "Genişliğe sığdır"); a password
  protected PDF asks for its password; a file MuPDF can't read shows an
  error in the window instead of exiting.
- **EPUB** — the `epub` crate for container/spine/TOC; chapter titles come
  from the table of contents. `metin::html_to_text` keeps paragraphs,
  headings, list items and line breaks, decodes named and numeric entities,
  and drops `<head>`, `<style>`, `<script>` and comments. Each chapter starts
  at its top.
- **Image** — decoded upright (EXIF orientation), fitted to the window
  without enlarging small pictures; ◀ ▶ walk the folder's pictures in
  natural order ("2" before "10").

Mouse wheel scrolls (at a PDF page's edge it turns the page), Ctrl+wheel
zooms, a drag pans (the page can't leave the view), two fingers pinch.
Keys: PageDown/→/Space, PageUp/←, Home/End, `+` `-` `0`, ↑ ↓, Esc, Ctrl+Z.

### Notes (`cizim.rs`)

Pen and highlighter strokes are stored as points in **page coordinates**
(fractions of the page's width/height) and drawn by the UI as Slint `Path`
elements inside the page rectangle, so they stay on the words they mark at
any zoom, pan or window size, and each PDF page or picture has its own. A
highlighter stroke is one translucent path, so it doesn't darken where it
crosses itself. Undo (button or Ctrl+Z) reverts the last stroke or a
"Notları sil". After every change the notes are written to
`$XDG_DATA_HOME/bacak-belge/notlar/<hash of the document path>.json` and
come back when the document is opened again.

---

## 3. The three standalone viewers

Each is a small, single-purpose Slint app with no shared code beyond the
workspace's pinned `slint` dependency version:

| Crate | Renderer | Notes |
|---|---|---|
| `bacak-pdf` | Same MuPDF pixmap path as `bacak-belge`'s PDF renderer, duplicated rather than shared — no internal `bacak-belge`-core library exists to depend on. |
| `bacak-epub` | Same `epub` + `html_to_text` approach as `bacak-belge`. |
| `bacak-resim` | `image::open` → `DynamicImage::into_rgba8` → `SharedPixelBuffer`; supports every format the `image` crate does (JPEG, PNG, GIF, WebP, BMP, TIFF, ICO, QOI, HDR, PNM). |

Because none of the three import from `bacak-belge` (or vice versa), a
change to the PDF/EPUB/image rendering logic has to be made in both places
if it should apply to the standalone viewer and the unified one — there is
currently no shared rendering library extracted from the duplication.

---

## 4. Packaging

Each binary is packaged independently via `cargo-deb`'s
`[package.metadata.deb]` block in its own `Cargo.toml`: its own `assets`
list (binary + `.desktop` + icon), its own `depends` line, and its own
version. `bacak-belge`'s is the most complete
(`libmupdf25.1` + Wayland/EGL runtime libs); the standalone crates mirror the
subset of that they actually need (e.g. `bacak-resim` never links MuPDF).

---

## 5. Testing

`bacak-belge`'s window-free logic is unit-tested (`cargo test -p
bacak-belge`: notes, EPUB text, fit/pan). Rendering and the UI are checked
by opening real PDF/EPUB/image files. The three standalone viewers have no
tests.
