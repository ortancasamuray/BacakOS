# Belgeler — Document & Picture Viewer

🌐 [Türkçe](README.tr.md) · **English**

`bacak-belge`, the BacakOS viewer for **PDF** (MuPDF, password-protected
ones too), **EPUB** (shown as text) and **pictures** (JPEG, PNG, GIF, WebP,
BMP, TIFF, ICO, QOI, HDR, PNM), with **pen/highlighter notes** on PDF pages
and pictures, saved per document. Written in Rust with
[Slint](https://slint.dev). See [ARCHITECTURE.md](ARCHITECTURE.md).

## Build & run

```sh
cargo run -p bacak-belge -- belge.pdf     # from Belgeler/
cargo test -p bacak-belge
```

## Packaging

```sh
cargo deb -p bacak-belge --no-build
sudo apt install ./target/debian/bacak-belge_*.deb
```

The package depends on `libmupdf25.1` plus the usual Wayland/EGL runtime
libraries (`$auto` picks these up via `cargo-deb`'s ldd scan).
`build-debs.sh` at the repository root builds it with the rest of BacakOS.

## License

GPL-3.0-or-later.
