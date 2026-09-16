# Belgeler — Mimari Özeti

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Bu, [ARCHITECTURE.md](ARCHITECTURE.md) dosyasının kısa Türkçe özetidir.
Dört bağımsız Rust + Slint ikilisi — ortak bir kütüphaneyi paylaşan dört
ince arayüz değil, her crate kendi tam render hattını sahiplenir. Paylaşılan
tek şey kök `Cargo.toml`'daki ortak `[workspace.dependencies]` (esas olarak
`slint` sürüm/özellikleri) ve `bacak-belge` için, diğer üçünün topluca
yaptığını tek crate içinde yapan bir modül.

## Crate yerleşimi

```
Belgeler/
├─ Cargo.toml                       # [workspace] members = ["crates/bacak-belge"]
│                                   # (bacak-pdf/epub/resim kendi [workspace]'iyle ayrı durur)
├─ crates/
│  ├─ bacak-belge/                  # dağıtılan birleşik görüntüleyici
│  │  ├─ src/main.rs                # PDF (MuPDF) + EPUB (epub crate) + resim (image crate)
│  │  │                             # + kalem/marker not alma bindirmesi
│  │  └─ ui/main.slint
│  ├─ bacak-pdf/                    # bağımsız, sadece-PDF görüntüleyici (MuPDF)
│  ├─ bacak-epub/                   # bağımsız, sadece-EPUB görüntüleyici
│  └─ bacak-resim/                  # bağımsız, sadece-resim görüntüleyici
├─ desktop/                         # ikili başına bir *.desktop girdisi
└─ icons/hicolor/scalable/apps/     # ikili başına bir SVG ikon
```

## `bacak-belge` — birleşik görüntüleyici

`src/main.rs` (~670 satır, tek dosya), açılan dosyanın uzantısına göre
seçilen üç render motorunu yan yana barındırır:

- **PDF** — `mupdf::Document::load_page` → `zoom * 96/72` DPI'de
  `Matrix::new_scale` → `page.to_pixmap` → `SharedPixelBuffer<Rgba8Pixel>`
  üzerinden bir Slint `Image`'a dönüştürülür.
- **EPUB** — konteyner/spine ayrıştırması için `epub` crate; bölüm HTML'i,
  tam bir HTML render motoru yerine kendi yazdığımız `html_to_text`
  temizleyicisinden (etiket kaldırma + yaygın entity çözme) geçirilir.
- **Resim** — `image` crate'in `DynamicImage::into_rgba8`'i, PDF
  render'ının kullandığı aynı `SharedPixelBuffer<Rgba8Pixel>` yoluna
  kopyalanır.

Üçü de aynı Slint `Image` tipinde buluşur, bu yüzden `ui/main.slint`
içindeki görünüm alanı, yakınlaştırma ve sayfa gezinme arayüzü formattan
bağımsızdır.

### Not alma katmanı

`AnnotationState` (`main.rs`'nin başında) sayfa başına üç RGBA buffer
tutar: `base_buf` (tamamlanmış vuruşlar), `stroke_buf` (devam eden vuruş,
tam opaklıkta), `composite` (ikisinin toplamı, ekrana çizilen). Kalem ve
marker aynı kod yolunu farklı alfa ile kullanır: kalem vuruşları 220
alfa'da, marker vuruşları ise 110 alfa'da (`marker_alpha`) — bu sayede
üst üste binen fosforlu vuruşlar opak yığılmak yerine doğal şekilde
koyulaşır. Bir vuruş, işaretçi bırakıldığında (`base_buf`'a düzleştirilir)
kesinleşir; o ana kadar yalnızca `composite` dokunulur, böylece devam eden
bir vuruş tam sayfa yeniden çizim maliyeti getirmez.

## Üç bağımsız görüntüleyici

Her biri, workspace'in sabitlenmiş `slint` bağımlılık sürümü dışında hiçbir
kod paylaşmayan, küçük, tek amaçlı bir Slint uygulamasıdır: `bacak-pdf`
aynı MuPDF pixmap yolunu tekrar eder (paylaşılan bir çekirdek kütüphane
yoktur), `bacak-epub` aynı `epub` + `html_to_text` yaklaşımını kullanır,
`bacak-resim` `image` crate'in desteklediği her formatı (JPEG, PNG, GIF,
WebP, BMP, TIFF, ICO, QOI, HDR, PNM) destekler. Hiçbiri `bacak-belge`'den
(veya tersi) import etmediği için, render mantığındaki bir değişikliğin
hem bağımsız görüntüleyiciye hem birleşik olana uygulanması gerekiyorsa iki
yerde de yapılmalıdır — şu an tekrardan çıkarılmış paylaşılan bir render
kütüphanesi yoktur.

## Paketleme

Her ikili, kendi `Cargo.toml`'undaki `[package.metadata.deb]` bloğu
üzerinden bağımsız olarak `cargo-deb` ile paketlenir: kendi `assets`
listesi, kendi `depends` satırı, kendi sürümü. En eksiksiz olanı
`bacak-belge`'ninkidir (`libmupdf25.1` + Wayland/EGL kütüphaneleri);
bağımsız crate'ler yalnızca gerçekten ihtiyaç duydukları alt kümeyi
yansıtır (ör. `bacak-resim` hiçbir zaman MuPDF'e bağlanmaz).

## Test

Şu an ayrı bir test paketi yok — doğrulama, her ikiliye gerçek
PDF/EPUB/resim dosyaları açılarak manuel yapılıyor. Dördü için de faydalı
bir sonraki adım, render fonksiyonlarını (`render_page`, `html_to_text`,
`load_slint_image`) bir Slint olay döngüsü olmadan birim test edilebilir
hale getirmek için saf byte-girdi/piksel-veya-metin-çıktı fonksiyonlarına
ayırmak olur — `kur`'un `backend/`'i kurulumcu için zaten bunu yapıyor.
