# Belgeler — Architecture

🌐 [Türkçe özet](ARCHITECTURE.tr.md) · **English**

One Rust + Slint application, `bacak-belge`: the BacakOS viewer for PDF,
EPUB and pictures, with pen/highlighter notes.

---

## 1. Layout

```
Belgeler/
├─ Cargo.toml                       # [workspace] members = ["crates/bacak-belge"]
├─ crates/bacak-belge/
│  ├─ src/main.rs                   # the three viewing modes (PDF, EPUB, picture)
│  ├─ src/cizim.rs                  # notes: vector strokes, undo, saving
│  ├─ src/metin.rs                  # EPUB (X)HTML → text
│  ├─ src/yerlesim.rs               # fit / zoom / pan arithmetic
│  └─ ui/main.slint
├─ desktop/bacak-belge.desktop
└─ icons/hicolor/scalable/apps/bacak-belge.svg
```

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

## 3. Packaging

`cargo-deb` through `[package.metadata.deb]` in the crate's `Cargo.toml`
(binary + `.desktop` + icon); `build-debs.sh` builds it for amd64, arm64 and
riscv64. It depends on `libmupdf25.1` and the Wayland/EGL runtime libraries.

---

## 4. Testing

`bacak-belge`'s window-free logic is unit-tested (`cargo test -p
bacak-belge`: notes, EPUB text, fit/pan). Rendering and the UI are checked
by opening real PDF/EPUB/image files.
