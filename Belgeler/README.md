# Belgeler — Document & Image Viewers

🌐 [Türkçe](README.tr.md) · **English**

Four small Rust + [Slint](https://slint.dev) viewer binaries for BacakOS. The
one that actually ships as *the* document viewer is `bacak-belge`, a unified
PDF + EPUB + image + text viewer with pen/marker annotation tools; the other
three (`bacak-pdf`, `bacak-epub`, `bacak-resim`) are focused single-format
viewers, built and packaged independently. See
[ARCHITECTURE.md](ARCHITECTURE.md) for the module/crate boundary.

## Binaries

| Binary | Formats | Renderer |
|---|---|---|
| `bacak-belge` | PDF, EPUB, image, text — **plus pen/marker annotation** | MuPDF (PDF) + `epub` crate + `image` crate |
| `bacak-pdf` | PDF only | MuPDF |
| `bacak-epub` | EPUB only | `epub` crate + a small in-house HTML-to-text stripper |
| `bacak-resim` | JPEG, PNG, GIF, WebP, BMP, TIFF, ICO, QOI, HDR, PNM | `image` crate |

## Build & run

`bacak-belge` is the only workspace member (`Cargo.toml`'s `[workspace]`
lists just it); the other three each declare their own standalone
`[workspace]` and build independently:

```sh
cargo run -p bacak-belge          # from the Belgeler/ workspace root
cd crates/bacak-pdf   && cargo run   # standalone crates: cd in first
cd crates/bacak-epub  && cargo run
cd crates/bacak-resim && cargo run
```

## Packaging

Each binary ships its own `.deb` via `cargo-deb`, wired to its own
`desktop/*.desktop` entry and `icons/hicolor/scalable/apps/*.svg`:

```sh
cargo deb -p bacak-belge --no-build
sudo dpkg -i target/debian/bacak-belge_*.deb
```

`bacak-belge`'s package depends on `libmupdf25.1` plus the usual
Wayland/EGL runtime libs (`$auto` picks these up via `cargo-deb`'s
ldd scan).

## License

GPL-3.0-or-later.
