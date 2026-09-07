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

`bacak-belge` is the only workspace member; the other three intentionally
declare their own `[workspace]` at the top of their `Cargo.toml` so they
build and version independently of the unified viewer.

---

## 2. `bacak-belge` — the unified viewer

`src/main.rs` (≈670 lines, single file) holds three renderers side by side,
selected by the opened file's extension:

- **PDF** — `mupdf::Document::load_page` → `Matrix::new_scale` at
  `zoom * 96/72` DPI → `page.to_pixmap` → converted into a Slint `Image` via
  `SharedPixelBuffer<Rgba8Pixel>`.
- **EPUB** — the `epub` crate for container/spine parsing; chapter HTML is
  run through an in-house `html_to_text` stripper (tag removal + common
  entity decoding) rather than a full HTML rendering engine.
- **Image** — the `image` crate's `DynamicImage::into_rgba8`, copied into the
  same `SharedPixelBuffer<Rgba8Pixel>` path the PDF renderer uses.

All three converge on the same Slint `Image` type, so the viewport, zoom, and
page-navigation UI in `ui/main.slint` is format-agnostic.

### Annotation layer

`AnnotationState` (top of `main.rs`) keeps three RGBA buffers per page:

- `base_buf` — completed strokes, committed.
- `stroke_buf` — the in-progress stroke at full opacity.
- `composite` — `base_buf` + `stroke_buf`, what's actually drawn to screen.

Pen and marker are the same code path with different alpha: pen strokes
commit at alpha 220, marker strokes at a translucent 110
(`marker_alpha`) so overlapping highlighter strokes darken naturally instead
of stacking to opaque. A stroke is finalized (flattened into `base_buf`) on
pointer-up; until then only `composite` is touched, so an in-progress stroke
never costs a full-page re-render of committed ink.

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

No dedicated test suite currently — verification is manual, opening real
PDF/EPUB/image files through each binary. A useful next step for any of the
four would be extracting the render functions (`render_page`, `html_to_text`,
`load_slint_image`) into pure functions over bytes-in/pixels-or-text-out so
they're unit-testable without a Slint event loop, as `kur`'s `backend/`
already does for the installer.
