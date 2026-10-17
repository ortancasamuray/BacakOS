# BacakOsSDK

**Türkçe** · [English](README.md)

BacakOS uygulamaları için ortak geliştirme kiti. Her uygulamanın ayrı ayrı
yazdığı masaüstü entegrasyonunu tek yerde toplar ve yeni (üçüncü parti)
uygulamaların BacakOS'a uyumlu yazılmasını sağlar.

| Crate | Ne verir |
|---|---|
| [`bacak-sdk`](crates/bacak-sdk) | Masaüstü genelinde koyu mod, renk paleti, `.desktop` dosyaları, simge bulma, MIME/varsayılan uygulama, compositor IPC istemcisi |
| [`bacak-sdk-slint`](crates/bacak-sdk-slint) | Slint için `BacakTema` ve bileşenler (pencere, düğme, kart, anahtar, başlık, metin); koyu moda kendiliğinden uyar |
| [`bacak-ipc`](crates/bacak-ipc) | Compositor ↔ uygulama protokolü ve ortak ayar dosyası biçimleri (compositor da buna bağlıdır) |

Ayrıca: [`sablon/`](sablon) + [`araclar/yeni-uygulama.sh`](araclar/yeni-uygulama.sh)
(yeni uygulama iskeleti) ve [`ornekler/merhaba`](ornekler/merhaba) (her
özelliği kullanan örnek).

Lisans: **MIT OR Apache-2.0** (BacakOS'un geri kalanı GPL-3.0-or-later'dır; SDK,
kapalı kaynak uygulamalar da kullanabilsin diye serbest lisanslıdır).

## Hızlı başlangıç

```sh
BacakOsSDK/araclar/yeni-uygulama.sh not-defteri "Not Defteri"
cd not-defteri
cargo run            # çalıştır
cargo deb            # target/debian/not-defteri_0.1.0-1_amd64.deb
```

Oluşan proje: Slint arayüzü (`ui/app.slint`), `BacakTema`'ya bağlı ana pencere,
`.desktop` dosyası ve simge (`paket/`), `cargo deb` ayarları. Paket kurulunca
uygulama menüsünde ve dock'ta kendi simgesiyle görünür. `slint::set_xdg_app_id`
ile `.desktop` adı aynı olduğu için dock, pencereyi `.desktop` dosyasına eşler.

## bacak-sdk

```toml
[dependencies]
bacak-sdk = { path = "…/BacakOsSDK/crates/bacak-sdk" }
```

### Görünüm (koyu mod)

Koyu mod **masaüstü geneli** bir ayardır (`~/.config/bacak/gorunum.json`).
Compositor, Kontrol Merkezi ve Masaüstü Ayarları'ndaki anahtarla bu dosyayı
yazar ve değişikliği IPC ile duyurur. Uygulamalar ayrı bir koyu tema tercihi
**tutmamalıdır**: uygulama içindeki anahtar `ayarla`'yı çağırır, her şey birlikte
değişir.

```rust
use bacak_sdk::{gorunum, Gorunum, Palet};

let g = Gorunum::load();                 // compositor çalışmıyorsa da çalışır
let palet = Palet::icin(g);              // arka, yuzey, metin, vurgu, tehlike…
gorunum::izle(|g| println!("koyu: {}", g.koyu));   // değişince (arka plan iş parçacığı)
gorunum::ayarla(false)?;                 // tüm masaüstünü açık moda al
```

`izle` bağlıyken compositor olayını, bağlı değilken dosyayı yoklar; compositor
yeniden başlasa da çalışmaya devam eder.

### Uygulamalar (`.desktop`)

```rust
use bacak_sdk::masaustu;

for app in masaustu::uygulamalar() { println!("{} ({})", app.ad, app.id); }   // menüdekiler, ada göre
let belge = masaustu::uygulama("bacak-belge").unwrap();   // id, id.desktop ya da pencere sınıfı
belge.baslat(&[Path::new("/home/ali/rapor.pdf")])?;       // Terminal=true ise terminalde
```

`Name[tr]` gibi yerelleştirilmiş adlar `LANG`'e göre seçilir. `Exec` satırı
standarda göre ayrıştırılır: tırnaklı argümanlar bölünmez, `%f %F %u %U %c %k`
açılır.

### Simgeler

```rust
let yol = bacak_sdk::simge::bul("folder", 48);                 // PNG ya da SVG yolu
let yol = bacak_sdk::simge::uygulama_simgesi(&app, 64);
let yol = bacak_sdk::simge::mime_simgesi("application/pdf", 32);
```

Arama sırası dock'unkiyle aynıdır: Masaüstü Ayarları/GTK'da seçili tema →
BacakOS → Breeze/Adwaita/Papirus… → diğer kurulu temalar → hicolor → pixmaps.

### MIME ve "Birlikte Aç"

```rust
use bacak_sdk::mime;
let tur = mime::tur(dosya);                       // xdg-mime, olmazsa uzantıdan
let varsayilan = mime::varsayilan("application/pdf");
let secenekler = mime::uygulamalar_icin("application/pdf");   // varsayılan başta
mime::varsayilan_yap("application/pdf", "bacak-belge.desktop")?;
mime::ac(dosya)?;  mime::birlikte_ac(dosya, "altay.desktop")?;
```

### Compositor IPC

```rust
use bacak_sdk::{bildir, Istemci};

bildir("Dışa aktarıldı", "rapor.pdf")?;          // BacakOS dışında sessizce bir şey yapmaz
let mut c = Istemci::baglan()?;                   // oturum yoksa ErrorKind::NotFound
c.kontrol_merkezi(Some(true))?;
for p in c.pencereler()? { println!("{} {}", p.uygulama, p.baslik); }
for e in c.ekranlar()? { println!("{}×{} @{}x", e.genislik, e.yukseklik, e.olcek); }
for olay in Istemci::baglan()?.abone_ol()? { println!("{olay:?}"); }   // bloklar
```

Her istek en çok 2 sn bekler (`ipc::ZAMAN_ASIMI`); arayüz iş parçacığında uzun
iş yapmayın.

## bacak-sdk-slint

```toml
[dependencies]
bacak-sdk-slint = { path = "…/crates/bacak-sdk-slint" }
slint = { version = "1.8", … }
[build-dependencies]
bacak-sdk-slint = { path = "…/crates/bacak-sdk-slint", features = ["derleme"] }
```

```rust
// build.rs
fn main() { bacak_sdk_slint::derle("ui/app.slint").unwrap(); }
```

```slint
// ui/app.slint
import { BacakTema, BacakPencere, BacakDugme, BacakKart, BacakAnahtar } from "@bacak";
export { BacakTema }
export component Uygulama inherits BacakPencere {
    BacakKart {
        BacakAnahtar { metin: "Koyu mod"; acik: BacakTema.koyu; degisti(v) => { /* Rust'a */ } }
        BacakDugme { metin: "Kaydet"; birincil: true; tiklandi => { … } }
    }
}
```

```rust
// main.rs
slint::include_modules!();
let ui = Uygulama::new()?;
bacak_sdk_slint::tema_bagla!(ui, BacakTema);          // koyu modu izler
ui.on_koyu_degisti(bacak_sdk_slint::koyu_mod_ayarla);  // masaüstü geneli
```

| Bileşen | Özellikler |
|---|---|
| `BacakTema` (global) | `koyu`; renkler `arka yuzey yuzey2 cizgi metin soluk vurgu vurgu-metin tehlike uyari basari`; ölçüler `yaricap yaricap-kucuk bosluk dolgu dokunma yazi yazi-kucuk yazi-baslik` |
| `BacakPencere` | Ana pencere; arka plan ve yazı boyu temadan, std-widgets'ın renk düzeni (`Palette.color-scheme`) de koyu moda uyar |
| `BacakDugme` | `metin`, `birincil`, `tehlike`, `etkin`, `tiklandi()` — en az 44 px dokunma hedefi |
| `BacakKart` | Çerçeveli yüzey, içi dikey yerleşim |
| `BacakAnahtar` | `metin`, `acik` (iki yönlü), `etkin`, `degisti(bool)` |
| `BacakBaslik`, `BacakMetin` | Başlık; gövde metni (`soluk: true` ikincil) |

Renkler `bacak_sdk::Palet` ile aynıdır, birini değiştirirken diğerini de güncelleyin.

## IPC protokolü

Soket: `$BACAK_IPC` (compositor çocuk süreçlere verir), yoksa
`$XDG_RUNTIME_DIR/bacak-ipc-<WAYLAND_DISPLAY>.sock`. Satır başına bir JSON nesnesi
(UTF-8, `\n`, en çok 64 KiB). İstekte istemcinin seçtiği `id` yanıtta geri
gelir; olayların `id`'si yoktur. Bilinmeyen alanlar yok sayılır.

| İstek | Alanlar | Yanıt `veri` |
|---|---|---|
| `surum` | — | `{"protokol":1,"compositor":"0.1.0"}` |
| `bildirim` | `baslik`, `ayrinti`? | — (başlık boşsa hata; metinler 200 karakterde kesilir) |
| `gorunum_al` | — | `{"koyu":true}` |
| `koyu_mod_ayarla` | `koyu` | — (kaydedilir, abonelere duyurulur) |
| `kontrol_merkezi` | `ac`? (`true`/`false`, yoksa aç/kapa) | — |
| `pencereler` | — | `[{"id","uygulama","baslik","calisma_alani","odakli"}]` |
| `ekranlar` | — | `[{"id","x","y","genislik","yukseklik","olcek"}]` |
| `abone` | — | — ; sonra olaylar: `{"olay":"gorunum","koyu":…}`, `{"olay":"odak","pencere":id\|null}` |

```sh
# Kabuktan deneme:
printf '{"id":1,"istek":"bildirim","baslik":"Merhaba"}\n' | socat - UNIX-CONNECT:"$BACAK_IPC"
```

Hata yanıtı: `{"id":N,"tamam":false,"hata":"…"}`.

**Güvenlik.** Soket oturum kullanıcısının `0700` çalışma dizinindedir ve `0600`
izinlidir; yalnızca aynı kullanıcının süreçleri bağlanabilir. Protokol pencere
içeriği, tuş girdisi ya da komut çalıştırma **sunmaz**. Sınırlar: en çok 64 eş
zamanlı bağlantı, satır başına 64 KiB, yanıtlarını okumayan istemci 1 MiB
birikmeden düşürülür.

## Mevcut uygulamalardaki durum

| Uygulama | SDK kullanımı |
|---|---|
| bacak-compositor | IPC sunucusu (`src/ipc.rs`); koyu modu `gorunum.json`'a yazar/okur, abonelere duyurur |
| altay | Koyu mod masaüstü geneli (kendi tercihi kullanılmıyor); `.desktop`/`Exec`/MIME/varsayılan uygulama `bacak_sdk` üzerinden (simge araması şimdilik altay'ın kendi kodu) |
| bacak-belge | EPUB okuma modu açılışta masaüstünün koyu modundan başlar (☀ düğmesi yalnızca o pencere için) |

Sıradaki adaylar: compositor'ın `icons.rs`'indeki `.desktop`/simge kodu,
altay'ın simge araması, kur ve tahta'nın renkleri.

## Derleme ve test

```sh
cd BacakOsSDK
cargo test                       # bacak-ipc, bacak-sdk (IPC istemcisi sahte sunucuyla)
cargo run -p bacak-merhaba       # örnek uygulama (BacakOS oturumunda)
```

Compositor tarafının testleri: `cd bacak && cargo test -p bacak-compositor --features udev ipc`.
