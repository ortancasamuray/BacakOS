# Belgeler — Belge ve Resim Görüntüleyicileri

🌐 **Türkçe** · [English](README.md)

BacakOS için dört küçük Rust + [Slint](https://slint.dev) görüntüleyici
ikilisi (binary). Asıl *belge görüntüleyici* olarak dağıtılan
`bacak-belge`'dir — kalem/marker ile not alma araçlarına sahip, birleşik
PDF + EPUB + resim + metin görüntüleyicisi; diğer üçü (`bacak-pdf`,
`bacak-epub`, `bacak-resim`) tek formata odaklı görüntüleyicilerdir,
bağımsız olarak derlenir ve paketlenir. Modül/crate sınırı için
[ARCHITECTURE.md](ARCHITECTURE.md) (İngilizce) /
[ARCHITECTURE.tr.md](ARCHITECTURE.tr.md) (Türkçe özet) dosyalarına bakın.

## İkili dosyalar

| İkili | Formatlar | Render motoru |
|---|---|---|
| `bacak-belge` | PDF, EPUB, resim, metin — **artı kalem/marker not alma** | MuPDF (PDF) + `epub` crate + `image` crate |
| `bacak-pdf` | Sadece PDF | MuPDF |
| `bacak-epub` | Sadece EPUB | `epub` crate + basit, kendi yazdığımız HTML-metne çevirici |
| `bacak-resim` | JPEG, PNG, GIF, WebP, BMP, TIFF, ICO, QOI, HDR, PNM | `image` crate |

## Derleme ve çalıştırma

`bacak-belge` tek workspace üyesidir (`Cargo.toml`'daki `[workspace]`
yalnızca onu listeler); diğer üçü kendi bağımsız `[workspace]`'ini bildirir
ve ayrı ayrı derlenir:

```sh
cargo run -p bacak-belge          # Belgeler/ workspace kökünden
cd crates/bacak-pdf   && cargo run   # bağımsız crate'ler: önce cd
cd crates/bacak-epub  && cargo run
cd crates/bacak-resim && cargo run
```

## Paketleme

Her ikili, `cargo-deb` ile kendi `.deb`'ini üretir, kendi
`desktop/*.desktop` girdisine ve `icons/hicolor/scalable/apps/*.svg`
ikonuna bağlıdır:

```sh
cargo deb -p bacak-belge --no-build
sudo dpkg -i target/debian/bacak-belge_*.deb
```

`bacak-belge`'nin paketi `libmupdf25.1` ile birlikte alışılagelmiş
Wayland/EGL çalışma zamanı kütüphanelerine bağımlıdır (`$auto`, bunları
`cargo-deb`'in ldd taramasıyla otomatik yakalar).

## Lisans

GPL-3.0-or-later.
