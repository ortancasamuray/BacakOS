# Belgeler — Belge ve Resim Görüntüleyici

🌐 **Türkçe** · [English](README.md)

`bacak-belge`: BacakOS'un **PDF** (MuPDF, parolalı belgeler dahil), **EPUB**
(metin olarak) ve **resim** (JPEG, PNG, GIF, WebP, BMP, TIFF, ICO, QOI, HDR,
PNM) görüntüleyicisi; PDF sayfalarına ve resimlere alınan, belge başına
kaydedilen **kalem/fosforlu notlarla**. Rust ve [Slint](https://slint.dev)
ile yazıldı. Mimari için [ARCHITECTURE.tr.md](ARCHITECTURE.tr.md).

## Derleme ve çalıştırma

```sh
cargo run -p bacak-belge -- belge.pdf     # Belgeler/ içinden
cargo test -p bacak-belge
```

## Paketleme

```sh
cargo deb -p bacak-belge --no-build
sudo apt install ./target/debian/bacak-belge_*.deb
```

Paket `libmupdf25.1` ile alışılagelmiş Wayland/EGL çalışma zamanı
kütüphanelerine bağlıdır (`$auto`, `cargo-deb`'in ldd taramasıyla yakalar).
Depo kökündeki `build-debs.sh` onu BacakOS'un geri kalanıyla birlikte derler.

## Lisans

GPL-3.0-or-later.
