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

`src/main.rs` açılan dosyanın uzantısına göre üç görüntüleme kipini
barındırır; pencere gerektirmeyen mantık birim testli küçük modüllerdedir
(`cargo test -p bacak-belge`):

| Modül | Görevi |
|---|---|
| `cizim.rs` | Kalem/fosforlu notlar (vektör çizgiler), geri al, kayıt |
| `metin.rs` | EPUB bölümü (X)HTML → okunur metin |
| `yerlesim.rs` | Sığdırma/yakınlaştırma/kaydırma hesabı, doğal dosya sırası |

- **PDF** — MuPDF sayfayı `zoom × pencere ölçeği × 96/72` ile çizer; HiDPI
  ekranda da keskindir, 40 MP'yi aşacak çizim küçültülüp büyütülür. Sayfa
  pencereye sığdırılmış açılır ("Sayfaya sığdır" ↔ "Genişliğe sığdır");
  parolalı PDF parola sorar; okunamayan dosya pencerede hata gösterir.
- **EPUB** — kapsayıcı/omurga/içindekiler için `epub` crate; bölüm adları
  içindekilerden gelir. `metin::html_to_text` paragrafları, başlıkları,
  liste maddelerini ve satır sonlarını korur, adlı ve sayısal karakter
  kodlarını çözer, `<head>`, `<style>`, `<script>` ve yorumları atar. Her
  bölüm başından açılır.
- **Resim** — EXIF yönüne göre dik açılır, küçük resimler büyütülmeden
  pencereye sığdırılır; ◀ ▶ klasördeki resimleri doğal sırayla gezer.

Tekerlek kaydırır (PDF sayfasının sonunda sayfa çevirir), Ctrl+tekerlek
yakınlaştırır, sürükleme kaydırır (sayfa görünümden çıkamaz), iki parmak
sıkıştırır. Tuşlar: PageDown/→/Boşluk, PageUp/←, Home/End, `+` `-` `0`,
↑ ↓, Esc, Ctrl+Z.

### Notlar (`cizim.rs`)

Kalem ve fosforlu çizgiler **sayfa koordinatlarında** (sayfanın eni/boyuna
oran olarak) saklanır ve sayfa dikdörtgeninin içinde Slint `Path` olarak
çizilir; yakınlaştırma, kaydırma ya da pencere boyutu ne olursa olsun
işaretledikleri yerde kalırlar, her PDF sayfasının ve resmin notu kendine
aittir. Fosforlu çizgi tek bir yarı saydam yoldur, kendi üstünden geçtiği
yerde koyulaşmaz. Geri al (düğme ya da Ctrl+Z) son çizgiyi ya da "Notları
sil"i geri alır. Notlar her değişiklikte
`$XDG_DATA_HOME/bacak-belge/notlar/<belge yolunun özeti>.json` dosyasına
yazılır ve belge yeniden açılınca geri gelir.

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

`bacak-belge`'nin pencere gerektirmeyen mantığı birim testlidir (`cargo
test -p bacak-belge`: notlar, EPUB metni, sığdırma/kaydırma). Çizim ve arayüz
gerçek PDF/EPUB/resim dosyaları açılarak denenir. Üç bağımsız görüntüleyicinin
testi yoktur ve `build-debs.sh` onları derlemez.
