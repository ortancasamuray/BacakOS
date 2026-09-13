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

- **`uzakel/uzakel-pc/vendor/ffmpeg-n9.0-latest-win64-gpl-shared-9.0/`**
  altında BtbN'in [FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds)
  projesinden **win64-gpl-shared, n9.0** sürümü indirilip açıldı:
  ```sh
  curl -sL -o /tmp/ffmpeg.zip \
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip"
  unzip /tmp/ffmpeg.zip -d uzakel/uzakel-pc/vendor/
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

Kod tabanında **hiçbir üretim kodu değişmedi** — sadece FFmpeg vendor
edildi ve bu plan yazıldı. Mevcut zstd tabanlı pipeline (bu oturumda
düzeltilen dokunma kalibrasyonu ve watch-channel "en yeni kazanır" fix'i
ile) hâlâ çalışır durumda ve üretimde kullanılabilir; bu plan onun
üzerine bir sonraki büyük adım.
