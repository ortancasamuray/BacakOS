# Belgeler — Mimari Özeti

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Tek bir Rust + Slint uygulaması, `bacak-belge`: BacakOS'un PDF, EPUB ve resim
görüntüleyicisi; kalem/fosforlu notlarla.

## Yerleşim

```
Belgeler/
├─ Cargo.toml                       # [workspace] members = ["crates/bacak-belge"]
├─ crates/bacak-belge/
│  ├─ src/main.rs                   # üç görüntüleme kipi (PDF, EPUB, resim)
│  ├─ src/cizim.rs                  # notlar: vektör çizgiler, geri al, kayıt
│  ├─ src/metin.rs                  # EPUB (X)HTML → metin
│  ├─ src/yerlesim.rs               # sığdırma / yakınlaştırma / kaydırma hesabı
│  └─ ui/main.slint
├─ desktop/bacak-belge.desktop
└─ icons/hicolor/scalable/apps/bacak-belge.svg
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

## Paketleme

Crate'in `Cargo.toml`'undaki `[package.metadata.deb]` ile `cargo-deb`
(ikili + `.desktop` + simge); `build-debs.sh` amd64, arm64 ve riscv64 için
derler. `libmupdf25.1` ve Wayland/EGL çalışma zamanı kütüphanelerine bağlıdır.

## Test

`bacak-belge`'nin pencere gerektirmeyen mantığı birim testlidir (`cargo
test -p bacak-belge`: notlar, EPUB metni, sığdırma/kaydırma). Çizim ve arayüz
gerçek PDF/EPUB/resim dosyaları açılarak denenir.
