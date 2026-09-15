# Donanım Video Kodlama Planı (H.264 + Intel QSV) — 2026-09-13

Bu dosya, "Uzak Masaüstü" özelliğinin şu anki **ham BGRA + zstd, kare-başı
bağımsız sıkıştırma** pipeline'ını gerçek bir donanım video codec'ine
(Sunshine/Moonlight mantığıyla) taşıma planını kaydeder. Bu, tek oturumda
bitecek bir iş değil — burada bırakılan net bir yol haritası, sonraki bir
oturumda sıfırdan araştırmaya gerek kalmadan devam edilebilmesi için.

## Neden gerekiyor

Mevcut pipeline (`bacak-remote-server/src/{capture,encode,network}.rs`)
her kareyi ham BGRA olarak yakalayıp `zstd::stream::encode_all` ile
bağımsız sıkıştırıyor, `MAX_CHUNK_BYTES=1200`'lük UDP parçalarına bölüp
gönderiyor — delta/hareket kodlaması yok, gerçek bir video codec değil.
Gerçek donanımda test edildi: fps/zstd seviyesi ayarlamalarıyla (bu
oturumda denenen watch-channel "en yeni kazanır" düzeltmesi dahil) kısmen
iyileşme sağlansa da, Sunshine gibi donanım-kodlamalı bir çözümün hız ve
kararlılığına yaklaşamıyor — kullanıcı gerçek donanımda doğrudan
karşılaştırdı.

## Karar: FFmpeg + Intel QSV (`h264_qsv`)

Windows makinesinde **Intel dahili grafik (Quick Sync Video destekli)**
var. Değerlendirilen seçenekler:

1. ~~`windows` crate + Media Foundation doğrudan~~ — daha az bağımlılık
   ama çok daha fazla el ile COM/MF kodu.
2. **FFmpeg (`h264_qsv` encoder), önceden derlenmiş mingw build'i ile —
   SEÇİLEN YOL.** Daha zengin/olgun API, zaten var olan cross-compile
   toolchain'imizle (mingw) birebir uyumlu bir prebuilt kaynağı var.
3. ~~RustDesk'in `hwcodec` crate'i~~ — crates.io'da YOK, sadece RustDesk'in
   kendi git deposunda, kendi FFmpeg/vendor-SDK indirme betiklerine sıkı
   bağlı bir iç kütüphane. Kullanmak RustDesk'in build karmaşıklığının
   önemli bir kısmını da içeri almak demek — bu oturumda ele alınmadı.

## Bu oturumda doğrulanan/hazırlanan temel

- **`uzakel/uzakel-windows/vendor/ffmpeg-n9.0-latest-win64-gpl-shared-9.0/`**
  altında BtbN'in [FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds)
  projesinden **win64-gpl-shared, n9.0** sürümü indirilip açıldı:
  ```sh
  curl -sL -o /tmp/ffmpeg.zip \
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip"
  unzip /tmp/ffmpeg.zip -d uzakel/uzakel-windows/vendor/
  ```
  (Bu dizin `.gitignore`'a eklendi — ~200MB, commit'lenmedi. Yeni bir
  oturumda yukarıdaki komutla yeniden indirilebilir; `latest` tag'i
  hareketli olduğundan farklı bir `n9.x` sürümü gelebilir, önemli değil.)
- **Doğrulandı**: `strings bin/ffmpeg.exe` çıktısında `--enable-libvpl`
  (Intel oneVPL/QSV desteği) var; `--cross-prefix=x86_64-w64-mingw32-`
  ile derlenmiş — bizim `.cargo/config.toml`'daki linker ayarlarıyla
  birebir aynı toolchain. `bin/*.dll` ve `bin/ffmpeg.exe` `file` ile
  kontrol edildi: `PE32+ ... x86-64`, hedef mimariyle uyumlu.
- `lib/` altında hem klasik `.lib`/`.def` hem de mingw'in beklediği
  `.dll.a` import kütüphaneleri var — `ffmpeg-sys-next`/`ffmpeg-next`
  crate'lerinin `FFMPEG_DIR` ortam değişkeniyle bulabileceği standart
  bir `include/`+`lib/` düzeni.

## Kalan adımlar (sırayla)

1. **Cargo entegrasyonu**: `bacak-remote-server/Cargo.toml`'a
   `ffmpeg-next` (ya da doğrudan `ffmpeg-sys-next`) Windows-only bağımlılık
   olarak eklenip, `.cargo/config.toml`'da ya da build script'inde
   `FFMPEG_DIR=<workspace>/vendor/ffmpeg-n9.0-latest-win64-gpl-shared-9.0`
   ortam değişkeni cross-compile için ayarlanmalı. İlk hedef: sadece
   `cargo build --target x86_64-pc-windows-gnu` ile bağlanabildiğini
   (linker hatası almadan) doğrulamak — henüz gerçek kodlama kodu
   yazmadan.
2. **Encode modülü** (`bacak-remote-server/src/encode.rs`'nin yerini
   alacak/yanına eklenecek yeni bir modül, örn. `encode_h264.rs`):
   `capture.rs`'ten gelen `CapturedFrame` (ham BGRA) alıp `h264_qsv`
   kodlayıcıya besleyen, H.264 NAL birimleri üreten bir sarmalayıcı.
   - v1 (basit/düşük risk): sistem belleğinden (system memory) besleme —
     `scrap`'in verdiği ham BGRA buffer'ı doğrudan encoder'a kopyalamak.
   - v2 (asıl performans kazancı): `scrap`'in DXGI yüzeyini doğrudan
     QSV'ye sıfır-kopya aktarmak (D3D11 texture paylaşımı) — bu, `scrap`
     crate'inin genişletilmesini ya da DXGI capture'ın kendi elimizle
     yazılmasını gerektirebilir; v1 çalışır hale gelmeden denenmemeli.
   - Anahtar kare (keyframe) periyodu ve bitrate hedefi ayarlanabilir
     olmalı (CLI arg olarak, mevcut `--fps`/`--zstd-level` deseniyle
     tutarlı).
3. **Protokol değişikliği** (`bacak-remote-proto/src/lib.rs`):
   `Codec::RawZstd`'nin yanına (ya da yerine) `Codec::H264` eklenmeli;
   `FrameInfo`'nun `payload_len`/`chunk_count` alanları H.264 NAL
   birimleri için de çalışır zaten (formattan bağımsız), ama NAL
   birimlerinin anahtar-kare/delta-kare ayrımını taşıyan bir alan
   (`is_keyframe: bool` gibi) eklenmesi gerekebilir — alıcı tarafın bir
   delta kareyi, henüz bir anahtar kare almadan decode etmeye
   çalışmaması için.
4. **Paket kaybı toleransı — ÖNEMLİ FARK**: zstd'nin aksine, H.264'te
   **bir paket kaybı sonraki anahtar kareye kadar görsel bozulmaya yol
   açar** (delta kareler önceki kareye bağımlı). v1 için en basit çözüm:
   sık anahtar kare aralığı (örn. her 1-2 saniyede bir) + bir kare
   eksik/bozuksa onu atlayıp bir sonraki anahtar kareyi beklemek
   (`FrameReassembler`'daki mevcut "bayat kareyi at" mantığına benzer,
   ama "eksik anahtar kareyi at, düzelene kadar don" şeklinde). Gerçek
   bir çözüm (NACK/retransmit ya da FEC) çok daha büyük bir iş, v1'de
   gerekmeyebilir.
5. **BacakOS (Linux) tarafında H.264 çözücü**: `bacak-compositor`'ın
   `remote_desktop.rs`'i şu an `zstd::stream::decode_all` ile decode
   ediyor (`FrameReassembler::add_chunk`). Bunun yerine bir H.264 decoder
   gerekiyor:
   - En basit: Linux için `ffmpeg-next`/`ffmpeg-sys-next`'i **native**
     (mingw değil, doğrudan Linux) hedefte kullanmak — dağıtımın kendi
     `libavcodec`'i üzerinden, ekstra vendor gerekmez.
   - Donanım hızlandırma isteniyorsa (Vega 6 iGPU, VAAPI destekliyor)
     `ffmpeg-next`'in VAAPI decode yolunu denemek — ama yazılım decode
     (H.264, encode'a göre çok daha ucuz) muhtemelen v1 için yeterli.
   - Çözülen kareler zaten BGRA/RGBA çıkacak şekilde ayarlanmalı ki
     `render_remote_desktop_panel`'deki `MemoryRenderBuffer` yolu
     değişmeden kullanılabilsin.
6. **`remote_desktop.rs` render yolu**: `FrameReassembler`'ın
   `zstd::decode_all` çağrısı yeni H.264 decoder'a yönlendirilecek;
   `render_remote_desktop_panel`'deki `src`/`dst` gerdirme mantığı (bu
   oturumda düzeltilen kısım) DEĞİŞMEDEN kalabilir — decode çıktısı
   yine native `(w, h)` boyutunda bir BGRA buffer olacağından.
7. **Uçtan uca test**: gerçek donanımda (Windows Intel iGPU + BacakOS'un
   AMD Vega 6'sı) — encode'un gerçekten QSV'yi kullandığını (yazılım
   fallback'e düşmediğini) doğrulamak (`ffmpeg`'in `-v verbose` çıktısı ya
   da Görev Yöneticisi'ndeki "Video Encode" GPU sayacı ile), gecikmeyi
   ölçmek, paket kaybı senaryolarını (Wi-Fi'de gerçekçi) test etmek.

## Riskler / bilinmeyenler

- Intel oneVPL **runtime**'ının (GPU sürücüsüyle gelen dispatcher/driver)
  hedef Windows makinesinde kurulu olduğu varsayılıyor — `h264_qsv`
  encoder'ı FFmpeg'de listelense bile, çalışma anında oneVPL runtime
  bulunamazsa encoder init hatası verir. İlk gerçek test bunu ortaya
  çıkaracak; gerekirse Intel'in "oneVPL GPU Runtime" redistributable'ı
  kurulum paketine eklenmeli.
- DXGI (scrap) → QSV sıfır-kopya yüzey aktarımı (plandaki v2) gerçek bir
  ekstra araştırma/mühendislik konusu — v1 (sistem belleği üzerinden
  kopya) önce çalışır hale getirilmeli, performans kazancı v2'de.
- H.264'ün paket-kaybına hassasiyeti (madde 4), zstd'nin "bağımsız kare"
  modelinden çok daha kırılgan bir davranış — Wi-Fi gibi kayıplı
  ağlarda görsel donma/bozulma riski, iyi bir anahtar-kare stratejisi
  olmadan zstd'den bile kötü hissettirebilir. Bu yüzden madde 4 atlanacak
  bir detay değil, v1'in parçası olmalı.
- `bacak-remote-client` (Windows istemcisi, BacakOS'u izleme yönü) bu
  planın kapsamında değil — sadece `bacak-remote-server`'ın (Windows
  ekranını BacakOS'a akıtan) encode tarafı ele alınıyor. İstemci tarafı
  video zaten decode ETMİYOR, sadece `bacak-remote-server`'ın encode
  ürettiği kareleri BacakOS'un `remote_desktop.rs`'i decode ediyor —
  yani madde 5 tek decode noktası.

## Şu an nerede duruyoruz

**Madde 1 (Cargo entegrasyonu) tamamlandı — 2026-09-14.** Henüz gerçek
encode kodu yok (madde 2'den itibaren hâlâ bekliyor), ama cross-compile
linker/bindgen zinciri doğrulandı:

- `bacak-remote-server/Cargo.toml`'a Windows-only `ffmpeg-next = "9.0"`
  eklendi.
- `.cargo/config.toml`'a `[env]` bölümü eklendi: `FFMPEG_DIR` (vendor
  dizinine, workspace-relative) ve `BINDGEN_EXTRA_CLANG_ARGS`.
- **Karşılaşılan/çözülen sorun**: `ffmpeg-sys-next`'in bindgen adımı,
  mingw'in `malloc.h`'ının içeri aldığı `mm_malloc.h`'ı bulamadı
  (`fatal error: 'mm_malloc.h' file not found`). GCC'nin kendi
  `include/` dizinini eklemek işe yaramadı — bu sefer GCC'nin intrinsic
  header'ları (`xmmintrin.h`, `ia32intrin.h`, ...) clang'ın kendi
  (ABI-uyumlu) builtin'leriyle çakıştı (`invalid conversion between
  vector type '__m128' and integer type 'int'` gibi onlarca hata).
  Çözüm: sadece clang'ın **kendi** resource-dir'indeki header'ları
  ekle (`/usr/lib/llvm-19/lib/clang/19/include`), GCC'ninkini değil.
  Bu path **makineye özel** (llvm-19 sürümüne bağlı) — başka bir
  makinede kırılırsa `find / -iname mm_malloc.h | grep clang` ile
  doğrusu bulunmalı.
- `cargo build -p bacak-remote-server --target x86_64-pc-windows-gnu`
  temiz ortamda (env değişkenleri elle verilmeden, `.cargo/config.toml`
  üzerinden) başarıyla derlenip linklendi — sadece `scrap`'ten gelen
  (önceden var olan, bu işle ilgisiz) `mem::uninitialized()` uyarıları
  var.

**Madde 2 (encode modülü) yazıldı, derleniyor — ama HENÜZ TEST EDİLMEDİ,
HENÜZ BAĞLANMADI.** `bacak-remote-server/src/encode_h264.rs`:
`H264Encoder::new`/`encode`/`flush` — `CapturedFrame` (ham BGRA) alır,
`sws_scale` ile NV12'ye çevirir, `h264_qsv` kodlayıcıya besler,
`EncodedPacket { data, is_keyframe }` döner (madde 4'ün ihtiyaç duyduğu
anahtar-kare ayrımı burada zaten var). v1 tasarımına uygun: sistem
belleğinden kopya (DXGI sıfır-kopya v2'de).

Önemli sınırlamalar — bir sonraki oturum bunları bilerek devam etmeli:
- **Hiçbir yerden çağrılmıyor.** `main.rs`'e sadece `#[cfg(windows)] mod
  encode_h264;` eklendi (derlenmesi için) — `run_session`/`network.rs`
  hâlâ eski `encode::encode_frame` (zstd) yolunu kullanıyor. Üretim
  pipeline'ı DEĞİŞMEDİ.
- **Sadece derleme doğrulandı** (`cargo build --target
  x86_64-pc-windows-gnu`, tip/API hataları yok — bkz. madde 1'deki
  ffmpeg-next API'siyle satır satır karşılaştırıldı), **gerçek donanımda
  hiç çalıştırılmadı.** `h264_qsv` encoder'ının gerçekten açılıp
  açılmadığı (oneVPL runtime var mı, `open_as` başarılı mı) bilinmiyor.
- Sabit `keyframe_interval`/`bitrate` CLI'dan henüz seçilebilir değil
  (madde 2'nin son alt maddesi) — `H264Encoder::new` parametre olarak
  alıyor ama çağıran taraf yok.

Mevcut zstd tabanlı pipeline (üretimde) DEĞİŞMEDİ ve hâlâ çalışıyor.

## `--test-h264-encode` sınaması yapıldı — 2026-09-14: BLOKE EDEN bulgu

`main.rs`'e geçici bir `--test-h264-encode` yolu eklendi (capture'ı
başlatıp `H264Encoder`'ı ~5 saniye gerçek kareyle besleyen, PIN/ağ
gerektirmeyen bir konsol aracı) ve **"Windows test makinesi" olarak
kullanılan `192.168.1.55` (`os6@..., DESKTOP-7J01345`) üzerinde
çalıştırıldı** (SSH ile — not: SSH üzerinden DXGI ekran yakalama bu
sefer de sorunsuz çalıştı, `README.tr.md`'deki SSH kısıtlaması hâlâ
sadece `SendInput`'a özgü).

**Sonuç — encoder açılamadı:**
```
[h264_qsv] Error creating a MFX session: -9.
[h264_qsv] The current mfx implementation is not supported, try next mfx implementation.
[h264_qsv] Error creating a MFX session: -9.
```
`-9` = `MFX_ERR_UNSUPPORTED`. Kök sebep runtime eksikliği DEĞİL, çok
daha temel bir şey: `wmic path win32_VideoController get name` bu
makinede **`VirtualBox Graphics Adapter (WDDM)`** döndürdü — yani bu
"Windows test makinesi" gerçek donanım değil, bir **VirtualBox sanal
makinesi**. Sanal makinede gerçek bir Intel GPU (ve dolayısıyla Quick
Sync donanımı) yok; GPU passthrough kurulmadığı sürece `h264_qsv`
hiçbir zaman açılamaz — bu, planın en başından beri varsaydığı "gerçek
Windows makinesi + Intel dahili grafik" senaryosuyla çelişiyor.

**Sıradaki adım artık madde 3 değil — önce şunlardan biri netleşmeli:**
1. Gerçek Intel iGPU'lu **fiziksel** bir Windows makinesi bulup orada
   aynı `--test-h264-encode` sınamasını tekrarlamak (asıl doğrulama bu
   olmalı — plan baştan beri bunu varsayıyordu).
2. VirtualBox'ta Intel GPU passthrough/3D hızlandırma denemek (kırılgan,
   VirtualBox'ın WDDM sanal adaptörü muhtemelen bunu hiç desteklemiyor —
   büyük olasılıkla zaman kaybı).
3. QSV'yi bu VM'de test edilemez kabul edip yazılım H.264 (`libx264`)
   fallback'ini de düşünmek — ama bu, planın "donanım kodlama" amacının
   dışına çıkar, ayrı bir karar gerektirir.

`encode_h264.rs`'in kendisi (kod, API kullanımı) bu bulguyla
doğrulanmadı ne de çürütülmedi — sadece çalıştığı ortamda gerçek bir
Intel GPU olmadığı için hiçbir sonuca varılamadı.

## Karar: tek satıcıya bağlı kalma — satıcıdan bağımsız aday listesi (2026-09-14)

Kullanıcı isteği: dağıtım hedefinin GPU'su (Intel/AMD/NVIDIA, belki
Apple/Qualcomm) önceden bilinmiyor, o yüzden sadece QSV'ye bağlı kalmak
yanlış tasarım. `encode_h264.rs` yeniden yazıldı:
`H264Encoder::new` artık tek bir codec adı yerine sırayla dener —
**`h264_nvenc` → `h264_amf` → `h264_qsv` → `libx264` (yazılım, son
çare)** — ilk açılanı kullanır. Vendored FFmpeg build'inde hepsi zaten
derlenmiş halde var (`strings avcodec-63.dll` ile doğrulandı:
`--enable-ffnvcodec`/CUDA, `--enable-amf`, `--enable-libvpl`,
`--enable-libx264`) — ekstra indirme/derleme gerekmedi. `H264Encoder`
artık hangi adayın açıldığını (`backend`) ve donanım mı yazılım mı
olduğunu (`is_hardware`) da dışarı veriyor.

**VirtualBox VM'inde (`192.168.1.55`) uçtan uca doğrulandı — fiziksel
konsoldan `--test-h264-encode` çalıştırıldı:**
```
[h264_nvenc] Cannot load nvcuda.dll                    -> atlandı (NVIDIA yok)
[AMF] DLL amfrt64.dll failed to open                   -> atlandı (AMD yok)
[h264_qsv] Error creating a MFX session: -9.            -> atlandı (Intel yok — bilinen VM kısıtı)
[libx264] ... kodlayıcı açıldı: libx264 (yazılım)
girdi kare sayısı: 14, çıktı paket sayısı: 14, anahtar kare: 1
toplam çıktı: 362840 bayt (69.4 KB/s), ~3.1 Mbps
```
Bu, sıralı deneme/hata yönetimi/`EncodedPacket{is_keyframe}` zincirinin
**gerçekten çalıştığını** kanıtlıyor — sadece bu VM'de hiçbir donanım
kodlayıcı açılamadığı için hâlâ doğrulanamayan tek şey, gerçek bir
NVENC/AMF/QSV donanımının açılıp açılmadığı (kod yolu aynı, sadece bu
makinede hiç tetiklenmedi).

## Şu an nerede duruyoruz (güncel)

- Madde 1 (Cargo entegrasyonu): TAMAM.
- Madde 2 (encode modülü): TAMAM ve **fonksiyonel olarak doğrulandı**
  (yazılım yolunda uçtan uca), satıcıdan bağımsız aday listesiyle.
  Gerçek bir donanım kodlayıcının (nvenc/amf/qsv) açılıp açılmadığı hâlâ
  hiçbir makinede görülmedi — sadece gerçek GPU'su olan bir Windows
  makinesinde ortaya çıkacak.
- Hâlâ bağlanmadı: `main.rs`'in gerçek oturumu/`network.rs` hâlâ eski
  zstd yolunu kullanıyor; `--test-h264-encode` geçici/ayrı bir yol.
- Madde 3 (protokol değişikliği): TAMAM — `bacak-remote-proto::Codec`'e
  `H264 { is_keyframe: bool }` eklendi. `is_keyframe`'i ayrı bir
  `FrameInfo` alanı yerine `Codec` variant'ının içine koyduk (sadece
  H264 için anlamlı, `RawZstd` için anlamsız bir alan eklememek için).
  Bunun exhaustive match kıran iki yeri de güncellendi (henüz gerçek
  decode yazılmadı, sadece "H264 geldi ama decode edemiyorum, kareyi at"
  ile derlenir/çalışır durumda tutuldu):
  - `bacak/crates/bacak-compositor/src/remote_desktop.rs` (asıl
    BacakOS taraf decode noktası — madde 5'in yeri)
  - `uzakel-windows/bacak-remote-client/src/decode.rs` (ayrı, bağımsız
    winit istemcisi — `bacak-compositor` eklentisi öncesi yazılmış bir
    dev/test harness'i, o da video decode ediyor, unutulmamalı)
  Doğrulama: `cargo build --workspace --target x86_64-pc-windows-gnu`
  (uzakel-windows) ve `cargo check -p bacak-compositor` (bacak) hatasız;
  `cargo test -p bacak-remote-proto` (13 test) hâlâ geçiyor. Sunucu
  hâlâ hiçbir zaman `Codec::H264` göndermiyor — bu adım sadece telin
  yeni bir codec'i taşıyabildiğini ve her iki decode noktasının
  kırılmadan bunu reddedebildiğini kanıtladı.

## Gerçek oturum `encode_h264`'e bağlandı — 2026-09-14

`main.rs`'e `--hardware-encode` bayrağı eklendi (Windows-only,
varsayılan KAPALI — BacakOS tarafı H.264 decode etmediği için varsayılan
davranış/üretim hâlâ `RawZstd`). Açıkken `run_session`'ın encode
task'ı `encode::encode_frame` (zstd) yerine `encode_h264::H264Encoder`
kullanıyor; `encode_h264::chunk_packet` bir `EncodedPacket`'i
`encode::EncodedFrame` (aynı `FrameInfo`+`FrameChunk` şekli) haline
getiriyor — `network.rs`/`send_frame` hangi codec'in ürettiğini
bilmeden/önemsemeden çalışıyor. Bir kodlayıcı çağrısı 0/1/N paket
dönebildiği için (B-frame yok ama iç tamponlama olabilir) her paket
kendi `frame_id`'ini alıyor, girdi kare sayısıyla 1:1 değil.

Karşılaşılan/çözülen sorun: `H264Encoder`, ham işaretçi tutan
`ScalerContext` (`SwsContext`) içerdiği için ffmpeg-next onu otomatik
`Send` yapmıyor — `tokio::spawn`'a taşınamadı ("future cannot be sent
between threads safely"). Tek thread'den sırayla kullanıldığı
gerekçesiyle `unsafe impl Send for H264Encoder {}` eklendi (yorumla
gerekçelendirildi).

**VirtualBox VM'inde uçtan uca doğrulandı** (`--pin 1234
--hardware-encode`, fiziksel konsoldan): yakalama başladı, nvenc/amf/qsv
sırayla denenip `libx264`'e düştü ("hardware encode: libx264 (software
fallback...)" log satırı), video/girdi soketleri açıldı, **gerçek bir
BacakOS istemcisi (`192.168.1.15`) bağlanmaya çalıştı** (sadece test PIN'i
`1234` gerçek PIN'le eşleşmediği için reddedildi). Encode döngüsü
çökmeden, kesintisiz aktı. Bu, kablolamanın (task/watch-channel/network
katmanı) gerçek bir oturumda çalıştığını kanıtlıyor — hâlâ görülmeyen
tek şey gerçek donanımda bir NVENC/AMF/QSV'nin açılması.

## Şu an nerede duruyoruz (güncel)

- Madde 1, 2, 3: TAMAM.
- Gerçek oturuma bağlama: TAMAM, `--hardware-encode` ile opt-in,
  varsayılan üretim davranışı (zstd) DEĞİŞMEDİ.
- Madde 4 (paket kaybı/anahtar-kare toleransı) ve madde 5 (BacakOS
  tarafında gerçek H.264 decode) hâlâ yapılmadı — `--hardware-encode`
  ile gerçek bir istemci eşleşse bile ekranı şu an boş/donmuş kalır
  (`remote_desktop.rs` H264 karelerini atıyor, bkz. madde 3).
- Hâlâ görülmedi: gerçek bir NVIDIA/AMD/Intel GPU'sunda nvenc/amf/qsv'den
  birinin açılması — sadece VirtualBox VM'inde (GPU'suz) test edildi.

## Madde 5 (BacakOS'ta gerçek H.264 decode) — TAMAM, 2026-09-14

`bacak/crates/bacak-compositor/src/decode_h264.rs` (yeni dosya, `lib.rs`'e
`#[cfg(feature = "runtime")] mod decode_h264;` — private, sadece
`remote_desktop` kullanıyor): `H264Decoder::new()` / `decode(&[u8]) ->
Vec<DecodedBgra>`. `ffmpeg-next`'in `h264` yazılım decoder'ı (VAAPI/donanım
decode değil — decode zaten encode'dan çok daha ucuz, "önce v1'i çalışır
hale getir" mantığı burada da geçerli) + `sws_scale` ile hangi format/boyut
gelirse gelsin BGRA'ya çeviriyor (kaynak format/boyut değişirse scaler
yeniden kuruluyor).

`remote_desktop.rs::FrameReassembler` güncellendi: artık oturum boyunca
kalıcı bir `h264_decoder: Option<H264Decoder>` alanı taşıyor (encode'un
aksine decoder **durumsuz değil** — delta kare, önceki karelerin decoder
içindeki durumuna bağlı, bu yüzden `RawZstd`'nin her karede yeniden
`zstd::decode_all` çağırdığı gibi her seferinde yeniden kurulamaz).
İlk `Codec::H264` karesinde tembel kuruluyor (çoğu oturum hâlâ `RawZstd`
kullandığından — sunucu varsayılanı değişmedi). Bir decode çağrısı 0/1+N
kare dönebildiği için sadece en yenisi gösteriliyor ("freshest wins",
kodun genelindeki desenle tutarlı).

Bağımlılık/kurulum notu: `bacak-remote-server`'ın aksine (vendor'lanmış
Windows FFmpeg), bu taraf **native Linux pkg-config** ile sistemin
`libavcodec`/`libavutil`/`libswscale`/`libavformat`'ını linkliyor —
geliştirme makinesinde `apt install libavcodec-dev libavutil-dev
libswscale-dev libavformat-dev` gerekti (çalışma zamanı kütüphaneleri
zaten kuruluydu, `-dev` header paketleri eksikti). `ffmpeg-next`'in
`format` özelliği (`avformat`) teknik olarak gereksiz görünse de
KAPATILAMIYOR — o özellik kapalıyken `ffmpeg-next` 9.0.0'ın kendi
`software/mod.rs` ve `codec/packet/packet.rs`'i derlenmiyor (üst kaynakta
bir feature-gate hatası, bizim kodumuzda değil) — `features = ["codec",
"software-scaling", "format"]` (`default-features = false`) ile
avfilter/avdevice olmadan, ama avformat'la birlikte derleniyor.

Doğrulama: `cargo check -p bacak-compositor --features runtime` ve
`--features udev` (üretim/paketleme özelliği) hatasız. `cargo test -p
bacak-compositor --features runtime --lib`: 195 geçti, 5 kaldı — hepsi
bu değişiklikle ilgisiz, önceden bilinen kırıklıklar (`ws_swipe_tests` —
bkz. bu projenin "wallpaper testi takılıyor" hafıza notu — ve
`icons::tests`, ikisi de `decode_h264`/`remote_desktop`'a dokunmuyor).
**Gerçek bir H.264 karesiyle uçtan uca (gerçek donanımda encode →
BacakOS'ta decode → ekranda görüntü) henüz sınanmadı** — hâlâ hiçbir
makinede gerçek bir donanım kodlayıcı açılmadığı için.

## Madde 4 (paket kaybı/anahtar-kare toleransı) — TAMAM, 2026-09-14

`remote_desktop.rs::FrameReassembler`'a `h264_awaiting_keyframe: bool`
eklendi (oturum başında `true` — özel `Default` impl'i ile, `#[derive]`
kaldırıldı). Kural:
- `start_frame`: yeni bir kare başlarken bir önceki access unit
  tamamlanmadan atıldıysa (eksik chunk) VE o access unit H.264 ise,
  `h264_awaiting_keyframe = true` — decoder'ın referans durumu artık
  şüpheli.
- `add_chunk`'ın H264 kolu: `is_keyframe == false` VE
  `h264_awaiting_keyframe == true` ise, o delta kareyi decoder'a HİÇ
  vermeden atla (`tracing::debug!` ile, sessizce). Bir keyframe
  geldiğinde (`is_keyframe == true`) her zaman decode dener —
  başarılıysa `h264_awaiting_keyframe = false`.
- Decode çağrısının kendisi hata dönerse (`decoder.decode(&payload)`
  `Err`), aynı şekilde `h264_awaiting_keyframe = true` — decoder'ın iç
  durumu artık güvenilir değil, sıradaki keyframe'e kadar delta
  kareler yine atlanacak.

Bilinçli olarak yapılmayan (plan metninde de "gerçek bir çözüm çok daha
büyük bir iş, v1'de gerekmeyebilir" denen) kısım: NACK/retransmit ya da
FEC yok — bu sadece "bozuk veri besleme" riskini "bir sonraki
keyframe'e kadar dur" ile değiştiriyor, kaybı telafi etmiyor. Yeterli:
`bacak-remote-server`'daki `keyframe_interval = fps*2` (saniyede bir
keyframe fırsatı) sayesinde en kötü durumda ~2 saniyelik donma.

Doğrulama: `cargo check -p bacak-compositor --features runtime` ve
`--features udev` hatasız (let-chain sözdizimi bu crate'in edition'ında
desteklenmediği için nested `if let`'e çevrildi — `uzakel-windows` tarafının
aksine bu crate 2024 edition değil).

## Şu an nerede duruyoruz (2026-09-14, güncel)

- Madde 1, 2, 3, 4, 5: TAMAM (kod yazıldı, derleniyor; encode tarafı
  gerçek oturumda VM'de doğrulandı). Madde 5 ve 4 kendi başlarına henüz
  uçtan uca (gerçek bir kayıp/keyframe senaryosuyla) sınanmadı.
- Hâlâ görülmedi: gerçek bir NVIDIA/AMD/Intel GPU'sunda nvenc/amf/qsv'den
  birinin açılması — sadece VirtualBox VM'inde (GPU'suz, `libx264`
  yazılım yoluna düşerek) test edildi. Encode → decode → ekranda görüntü
  zincirinin tamamı henüz hiçbir makinede uçtan uca görülmedi.
- Sıradaki en değerli adım artık kod değil: gerçek donanımda
  `--hardware-encode` ile BacakOS'a bağlanıp ekranda gerçekten görüntü
  çıktığını görmek (madde 7) — bu hem encode hem decode'u hem de bu
  oturumda yazılan keyframe-toleransını birlikte doğrular.

## Madde 7 (gerçek donanımda uçtan uca) — BAŞARILI, 2026-09-15

Fiziksel bir Windows makinesinde (Intel UHD Graphics 630, gerçek donanım —
VirtualBox değil) `bacak-remote-server.exe --hardware-encode` masaüstünden
çift tıklanarak (pencereli PIN ekranıyla) çalıştırıldı, BacakOS'un "Uzak
Masaüstü" paneline PIN elle girildi ve **eşleşme + video akışı + ekranda
gerçek görüntü** başarıyla doğrulandı:

```
hardware encode: h264_qsv (hardware)
client 'bacak-os' paired successfully
```

Bu, madde 1-7'nin hepsini (encode, protokol, gerçek oturuma bağlama, BacakOS
decode, keyframe-toleransı, ve gerçek donanımda uçtan uca) birlikte
doğruluyor. Yol boyunca çıkan ve çözülen gerçek engeller (not: ileride aynı
kurulumu tekrarlayacak biri için):
- SSH üzerinden çalıştırıldığında ekran yakalama (DXGI) "no primary display"
  hatasıyla çöküyor — süreç Session 0'da (SSH servis oturumu) açılıyor,
  gerçek masaüstü Session 1'de. Çözüm: `schtasks /Create ... /IT /RU <user>`
  ile süreci interaktif Session 1'e enjekte etmek, ya da (daha basit ve
  sonunda kullanılan) fiziksel/uzak masaüstünden `.exe`'ye çift tıklamak.
- Vendored FFmpeg DLL'leri (`avcodec-63.dll` vb.) `.exe` ile **aynı dizine**
  kopyalanmalı — Windows DLL arama sırası önce exe'nin kendi dizinine bakar;
  eksik olduğunda süreç hiçbir çıktı vermeden anında çöküyor (exit 1).
- Windows Firewall, ilk kez ağdan kopyalanan bu imzasız `.exe`'nin gelen UDP
  paketlerini (video/input portları) reddediyor olabilir —
  `New-NetFirewallRule` ile açmak gerekti.
- BacakOS tarafı (`remote_desktop.rs::run`) bir `PairResponse{accepted:false}`
  aldığında döngüden tamamen çıkıp hata veriyor (otomatik sonsuz tekrar
  denemiyor) — panel PIN'i sadece panel yeniden açıldığında tazeliyor, bu
  yüzden iki taraf da (BacakOS panelindeki PIN ve Windows'a girilen PIN) aynı
  anda güncel olmalı.
- `~/.config/bacak-remote/pair.json`'daki `ip` alanı test makinesi
  değiştiğinde elle güncellenmeli (VM → fiziksel makine geçişinde unutulup
  saatlerce "sunucu reddetti" hatasına yol açtı).

## GUI cilası — 2026-09-15

Kullanıcı geri bildirimiyle `gui.rs` üç şekilde iyileştirildi:
- Konsol penceresi artık double-click ile açılışta görünmüyor
  (`winapi::um::wincon::FreeConsole()`, `gui::run`'ın başında) — binary
  hâlâ console-subsystem (`--no-gui`/`--pin` otomasyon yolu stdout'u
  bozulmadı), sadece GUI yolu kendi konsolunu anında bırakıyor.
- **"Eşleşmeyi Bitir"** ve **"Kapat"** düğmeleri eklendi. Bitir, oturumu
  kapatmadan `run_session`'a yeni bir `shutdown_rx: watch<bool>` ile haber
  verip ağ thread'ini sonlandırıyor; `SessionStatus::Ended` (yeni variant)
  UI'ı sıfırlayıp yeni bir PIN girilmesine izin veriyor —
  `PairingWindow::start_session` artık her PIN gönderiminde yeniden
  çağrılabilen bir metod (eskiden `run()` içinde tek seferlik kod).
- Eşleşme başarılı olduğunda bildirim alanında (tray) balon bildirimi
  gösteriliyor (`nwg::TrayNotification`, stok `OemIcon::WinLogo` — ekstra
  `.ico` asseti yok).

## Kapanış donması (10sn) — KÖK NEDEN BULUNDU VE DÜZELTİLDİ, 2026-09-15

Fiziksel donanımda uçtan uca çalışırken bulunan gerçek bir performans
bug'ı: bir pencere (Firefox) açılışı ~2-3sn kabul edilebilir gecikmeyle
görünürken, **kapanışı ekranda ~10 saniye donuk kalıyordu**.

İlk deneme — anahtar-kare aralığını `fps*2`'den `fps*1`'e düşürmek —
donmayı GİDERMEDİ. Asıl kök neden farklıydı: `encoded_tx`/`encoded_rx`
`watch` kanalı ("en yeniyi gönder, eskiyi sessizce at" — `run_session`'da
zaten belgeliydi) hem `RawZstd` hem `--hardware-encode` (H.264) yolu
tarafından paylaşılıyordu. `RawZstd` için bu doğru: her kare bağımsız
kod çözülüyor. **H.264 delta kareleri için YANLIŞ**: bir delta kare
sadece kendinden önceki karenin decoder'da bıraktığı referans duruma
göre çözülebiliyor. Network task network'e büyük bir keyframe
gönderirken (ör. pencere kapanışı gibi büyük bir sahne değişimi
sırasında) encode task yeni delta kareler üretmeye devam ediyor —
`watch` kanalı bunların hepsini "en yeniyle" değiştiriyor, aradakiler
**hiç gönderilmiyor**. BacakOS tarafındaki decoder bunu gerçek bir paket
kaybından ayırt edemiyor, `h264_awaiting_keyframe = true` moduna geçip
sıradaki keyframe'e kadar donuyor — tam da gözlemlenen davranış.

**Düzeltme**: `network::FrameSource` enum'u eklendi —
`Latest(watch::Receiver<...>)` (RawZstd, değişmedi) ve
`Ordered(mpsc::Receiver<EncodedFrame>)` (H.264, hiçbir kareyi atlamaz,
network yavaşsa encode task'ı `.send().await` ile geri bastırır —
sessizce atlamak yerine gerçek backpressure). `main.rs`'te yeni
`EncodeSink` enum'u aynı ayrımı gönderen tarafta yapıyor. Fiziksel
donanımda doğrulandı: **Firefox kapanış donması tamamen geçti.**

Ders: "en yeniyi göster, eskiyi at" deseni (bu kod tabanında capture ve
zstd decode için doğru ve kasıtlı) sahne-bağımsız veri için güvenli,
ama H.264 gibi zamansal referans zinciri olan bir codec'in kodlanmış
çıktısına doğrudan uygulanamaz.

**Kurulu paket (NSIS) üzerinde tekrar test edildi**: `FrameSource`
düzeltmesi sonrası donma 10sn'den 3-4sn'ye düştü — artık bozulma değil,
gerçek bir aktarım süresi (1920x1080 anahtar-kare ~1.5-2MB, 4 Mbps'te
bu kadar sürer). Bitrate 4→15 Mbps'e çıkarıldı (LAN için), sonuç:
**kabul edilebilir bir gecikme, donma değil.** Hem `main.rs`'teki
kod hem paketleme (`build.sh`/`installer.nsi`) hem de kurulu `.exe`
üzerinde doğrulandı.

## Bırakıldığı yer — 2026-09-14 oturum sonu

Yeni bir olası "gerçek donanım" makinesi denendi: `192.168.1.181`
(`os7`), ama **SSH hâlâ bağlanmıyor** ("Connection refused" port 22'de)
— kullanıcı OpenSSH Server'ı kurduğunu söyledi ama servis/güvenlik
duvarı kuralı doğrulanamadı (`Get-Service sshd` /
`Get-NetFirewallRule -Name sshd` çıktısı istendi, henüz gelmedi). Bu
makinenin gerçek bir GPU'su olup olmadığı da (`wmic path
win32_VideoController`) henüz kontrol edilemedi.

**Yarın buradan devam:**
1. `192.168.1.181`'e SSH bağlantısını doğrula (servis/firewall kuralı
   kontrolü kullanıcıdan bekleniyor).
2. Bağlanınca ilk iş: `wmic path win32_VideoController get
   name,driverversion,status` — gerçek bir GPU (VirtualBox değil) mi
   diye bak.
3. Öyleyse `h264-test` klasörünü (bu sefer gerçek oturum exe'siyle,
   `--pin <gerçek-pin> --hardware-encode`) oraya kopyala, BacakOS
   tarafından gerçek bir istemciyle eşleştirip **uçtan uca görüntü**
   almayı dene — bu, madde 1-5'in tamamını tek seferde doğrular.
4. Diğer VM (`192.168.1.55`/`os6`) hâlâ GPU'suz; onda daha fazla
   donanım testi denemeye değmez.
