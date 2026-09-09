# bacak-remote — Bacak OS için PC ekran/girdi köprüsü

🌐 **Türkçe** · [English](README.md)

Düşük gecikmeli, iki yönlü bir ekran akışı ve girdi yönlendirme köprüsü:
bir PC (Windows/Linux/macOS) masaüstünü bir Bacak OS istemcisine akıtır,
Bacak OS tarafı da dokunma/işaretçi/klavye girdisini PC'yi sürmek üzere geri
gönderir.

Bu, [`uzakel`](../ARCHITECTURE.md)'in PC tarafındaki kardeşidir (uzakel aynı
işi ters yönde yapar — bir telefon BacakOS makinesini kontrol eder). İkisi de
aynı tasarım içgüdülerini paylaşır (göreli işaretçi delta'ları, gecikmeye
duyarlı her şey için UDP, arabelleğe almak yerine "en taze kazanır") ama
bağımsız tel protokolleri ve kod tabanlarıdır — bir PC masaüstü akışı,
trackpad delta'larından çok farklı bir yüktür.

> **v1 durumu: aynı makinede loopback ile doğrulandı, henüz gerçek Wi-Fi
> üzerinde değil.** Server + client aynı makinede birbirine karşı çalıştırıldı
> (Xvfb sanal X11 ekranı, `127.0.0.1`) ve video hattı canlı olarak uçtan uca
> doğrulandı: `Hello`/`HelloAck` eşleşmesi, gerçek 1280×800 yakalama,
> yapılandırılan kare hızına uyan periyodik `FrameInfo`/`FrameChunk` trafiği,
> ve 80+ saniye boyunca sıfır hatayla çalışan bir `wgpu` (llvmpipe/Vulkan)
> render döngüsü. Girdi enjeksiyonu, input portuna doğrudan gerçek bir tel
> paketi gönderilerek sunucu tarafında doğrulandı — `enigo`, `libxdo`
> üzerinden hatasız decode edip enjekte etti — ama istemcinin kendi
> winit→UDP gönderim yolu bu geçişte canlı bir fare/dokunma ile test
> edilmedi (aşağıdaki "Neler test edildi" bölümüne bakın). **Henüz** iki
> ayrı makine arasında gerçek bir Wi-Fi bağlantısı üzerinden çalıştırılmadı
> ve orijinal spesifikasyonda adı geçen birkaç parça (donanım H.264/AV1
> kodlama, QUIC/WebRTC, compositor'a sıfır-kopya `dmabuf`, gerçek çoklu
> dokunma enjeksiyonu) bilinçli olarak **uygulanmadı** — bunlardan herhangi
> birini bitmiş saymadan önce aşağıdaki "Dürüst kapsam" bölümüne bakın.

---

## Dürüst kapsam: gerçek olan ile gelecek iş

*Üretim* kalitesinde donanım hızlandırmalı (NVENC/VAAPI/DXGI) + QUIC/WebRTC +
compositor'a sıfır-kopya bir hat kurmak, platform başına sistem
bağımlılıkları (CUDA/VAAPI sürücüleri, çalışan bir xdg-desktop-portal +
PipeWire oturumu, GStreamer/ffmpeg sistem kütüphaneleri) gerektiren, tek
seferde bu donanım/OS matrisi önümde olmadan doğrulanamayacak çok haftalık
bir iştir. Bunu yaptığını *iddia edip* aslında test edilemeyen kod
göndermek, kapsam hakkında açık olmaktan daha kötü olurdu. Bu yüzden v1,
tam spesifikasyona büyüyebilecek temiz eklem noktalarıyla, gerçekten
çalışan bir taban hat gönderiyor:

| Spesifikasyon maddesi | v1 gerçeği | Yükseltme yolu |
|---|---|---|
| Yakalama | [`scrap`](https://docs.rs/scrap) — Linux'ta X11 (XWayland dahil), Windows'ta DXGI, macOS'ta CoreGraphics | Yerel Wayland host'ları için aynı `run_capture_thread` sözleşmesinin arkasında bir `PipeWireCapturer` (`bacak-remote-server/src/capture.rs` modül belgesi) |
| Video kodeği | Ham BGRA + `zstd` (kayıpsız, GPU/sürücü bağımlılığı yok) | Bir `Codec` varyantı + eşleşen kodlama/çözme arka ucu ekleyin (`gstreamer-rs`/`ffmpeg-sys-next`/NVENC/VAAPI bağlamaları) — kodeğe özgü baytlara dokunan tek yerler `bacak_remote_proto::Codec` ve `encode.rs`/`decode.rs`'dir |
| Taşıma | Düz `tokio` UDP, elle parçalama, FEC/onay yok, "en taze kare kazanır" | Aynı `Message`/parça API'sinin arkasında tıkanıklık kontrolü + isteğe bağlı güvenilirlik için `quinn` (QUIC), ya da RTP+FEC |
| İstemci render | CPU tarafı doku yüklemesiyle bağımsız `winit` + `wgpu` penceresi | Sıfır-kopya blit için kendi EGL bağlamını paylaşan bir `bacak-compositor` eklenti yüzeyi (`render.rs` modül belgesi) — bu aynı zamanda [projenin kendi kuralıyla](../../bacak/README.md) (bağımsız masaüstü uygulaması açmama) olan gerilimi de çözer |
| Dokunma yönlendirme | `winit::event::Touch`, tek parmak 1:1 (istemci) → tek işaretçi `enigo` enjeksiyonu, tek aktif parmak (sunucu) | Bu, compositor içine taşındığında gerçek `wl_touch`/libinput yakalama; gerçek eş zamanlı parmak jestleri (pinch/iki parmak kaydırma) için host üzerinde özel bir sanal çoklu dokunma `/dev/uinput` cihazı — bkz. `input_inject.rs` modül belgesi |
| Girdi enjeksiyonu | `enigo` (göreli fare hareketi, düğmeler, kaydırma, tıklama olarak mutlak-konum dokunma) | Aynı `enigo` crate'i her üç hedef işletim sistemini de kapsıyor; çoklu dokunma enjeksiyonu (yukarıda) ele alınmadıkça değişiklik gerekmez |

Gecikme rakamları (dokunuştan-fotona, <30 ms ağ) spesifikasyonda bir tasarım
*hedefiydi*, burada ölçülen bir şey değil — iki makine arasında Wi-Fi testi
yapılmadı. Taşımayı "düşük gecikme için tasarlandı" (UDP, küçük parçalar,
eski-kareyi-düşürerek-yeniden-birleştirme, destekleniyorsa `Immediate` sunum
modu) olarak değerlendirin, ölçülmüş bir garanti olarak değil.

---

## Neler test edildi

Aynı makinede loopback, server ve client ikisi de bir `Xvfb :99` sanal X11
ekranına (1280×800) yönlendirilmiş, `bacak-remote-client` `127.0.0.1`'e
bağlanıyor:

- **Eşleştirme** — istemcinin `Hello`'su sunucuya ulaştı, `HelloAck` gerçek
  yakalanan çözünürlükle (`1280x800`) geri geldi; her iki UDP soket çifti de
  (video `9910`, girdi `9911`) `ss -u -an` çıktısında `ESTAB` gösterdi.
- **Video hattı** — sunucu 80+ saniye boyunca kodlama/gönderme hatası
  olmadan çalıştı, yazma-syscall hızı yapılandırılan kare hızıyla eşleşti
  (kare başına 2 gönderim — `FrameInfo` + bir `FrameChunk`, çünkü statik bir
  Xvfb karesi zstd ile 1200 B'lik parça bütçesinin çok altına sıkışıyor).
  İstemci bir Vulkan adaptörü buldu (`llvmpipe`, yazılım), penceresini açtı
  ve render döngüsünü aynı 80+ saniye boyunca hiç `wgpu::SurfaceError` ya da
  panik olmadan çalıştırdı.
- **Girdi enjeksiyonu (yalnızca sunucu tarafı)** — sunucunun girdi portuna
  doğrudan gönderilen ham tel formatında bir `PointerMotion` + sol tık
  paketi, hatasız decode edilip `enigo`'ya iletildi (sanal ekrana karşı
  gerçek bir `libxdo` çağrısı). İstemcinin kendi `winit` olayı → UDP
  gönderim yolu bu geçişte **test edilmedi** (o ortamda istemcinin
  penceresine gerçek fare/dokunma olayı sürecek sentetik-girdi aracı yoktu).

**Henüz test edilmeyenler:** gerçek Wi-Fi üzerinden fiziksel olarak ayrı iki
makine; canlı bir işaretçi/dokunuşla tam istemci-taraflı girdi yakalama
yolu; paket kaybı ya da jitter altındaki davranış; çok dakikalık sürekli
çalışma; boş bir sanal ekrandan çok daha büyük sıkışacak ve parçalama
yolunu çok daha zorlayacak gerçek (Xvfb olmayan) masaüstü içeriği.

---

## Workspace düzeni

```
uzakel-pc/
├── .cargo/config.toml      # Windows cross-compile linker + crt-static ayarları
├── vendor/scrap-0.5.0/     # yerelde yamalanmış `scrap` (aşağıdaki "Windows: hazır .exe" bölümüne bakın)
├── bacak-remote-proto/     # paylaşılan tel protokolü (postcard ile serileştirilmiş)
│   └── src/lib.rs          # Message, InputEvent, FrameInfo/FrameChunk, encode()/decode()
├── bacak-remote-server/    # PC tarafı daemon: yakalama, kodlama, akış, enjeksiyon
│   ├── packaging/windows/  # build.sh + installer.nsi -> bacak-remote-server-setup.exe
│   └── src/
│       ├── capture.rs      # scrap tabanlı ekran yakalayıcı, kendi OS thread'inde
│       ├── encode.rs       # zstd sıkıştırma + parça bölme
│       ├── network.rs      # UDP video bağlantısı (Hello/HelloAck/kareler) + girdi dinleyici
│       ├── input_inject.rs # enigo tabanlı işaretçi/dokunma enjeksiyonu
│       └── main.rs         # capture -> encode -> network'ü, + input -> inject'i bağlar
└── bacak-remote-client/    # Bacak OS tarafı alıcı + girdi yönlendirici
    └── src/
        ├── decode.rs        # kare yeniden birleştirme (parçalar -> zstd çözme)
        ├── network.rs       # UDP video alıcı + girdi gönderici
        ├── render.rs        # wgpu doku yüklemesi + tam ekran quad sunumu
        ├── input_capture.rs # winit olayları -> InputEvent
        └── main.rs          # her şeyi bağlayan winit olay döngüsü
```

## Tel protokolü özeti

Tek bir `Message` enum'u (`bacak-remote-proto`), sabit bir
`[MAGIC:4][VERSION:1]` başlığının arkasında postcard ile kodlanmış — böylece
başıboş ya da sürüm uyuşmazlığı olan bir paket, deserialize edilmeden önce
reddedilir:

- `Hello` / `HelloAck` — istemci video portunda kendini duyurur, sunucu
  gerçek ekran boyutuyla yanıt verir ve istemcinin adresini hatırlar.
- `FrameInfo` — kare başına bir tane, parçalarından önce gelir; genişlik/
  yükseklik/kodek/parça sayısını taşır.
- `FrameChunk` — sıkıştırılmış kare yükünün en fazla `MAX_CHUNK_BYTES`
  (1200 B) kadarı; istemcinin `FrameReassembler`'ı, daha yeni bir
  `FrameInfo` geldiği anda tamamlanmamış bir kareyi doğrudan düşürür (asla
  eski kareleri arabelleğe almaz).
- `Input` — `PointerMotion` (göreli dx/dy), `PointerButton`,
  `PointerScroll`, `TouchDown`/`TouchMotion`/`TouchUp` (sunucunun ekranına
  göre 0.0–1.0 normalize edilmiş).
- `Heartbeat` / `Bye` — canlılık ve temiz oturum kapatma.

Video trafiği ve girdi trafiği **ayrı UDP soketleri/portları** kullanır
(`DEFAULT_VIDEO_PORT` 9910, `DEFAULT_INPUT_PORT` 9911) — böylece bir kare
parçası patlaması hiçbir zaman bir girdi paketinin arkasında kuyruğa
giremez ya da onu geciktiremez; bu, `uzakel`'in Android köprüsünün zaten
kullandığı amaca-özel kanal ayrımıyla örtüşür.

## Derleme & çalıştırma

Her crate kendi platformunun olağan Rust/grafik araç zincirini gerektirir
(bir C linker, ve Linux'ta `scrap` ile `enigo`'nun bağlandığı X11 geliştirme
başlıkları — Debian/Ubuntu'da `libxcb-randr0-dev libxdo-dev`; bunlar
kurulu olmadan `cargo check` başarılı olur, ama gerçek bir binary'yi
linklemek için kurulu olmaları gerekir). v1 için PipeWire, GStreamer,
ffmpeg ya da bir GPU üretici SDK'sı gerekmiyor.

```sh
sudo apt-get install libxcb-randr0-dev libxdo-dev   # Debian/Ubuntu Linux host
```

```sh
cd uzakel-pc
cargo build --release --workspace

# Akıtılacak PC üzerinde (Windows/Linux/macOS):
./target/release/bacak-remote-server --fps 60

# Bacak OS makinesinde (ya da şimdilik aynı LAN'daki herhangi bir test makinesinde):
./target/release/bacak-remote-client <sunucu-lan-ip>
```

```sh
cargo test -p bacak-remote-proto   # protokol round-trip + çerçeveleme testleri
cargo clippy --workspace --all-targets
```

Yerel loopback testi (her iki binary de aynı makinede, `127.0.0.1`), hattı
iki makine arasında Wi-Fi üzerinden denemeden önce doğrulamanın en hızlı
yoludur.

## Windows: hazır `.exe` + kurulum paketi

`bacak-remote-server`, Linux'tan gerçek, bağımsız bir Windows `.exe`'sine
temiz şekilde cross-compile ediliyor — *derlemek* için Windows makinesi ya
da Visual Studio gerekmiyor (elbette *çalıştırmak* hâlâ Windows'a özgü bir
adım). `bacak-remote-client` Windows için derlenmiyor: o, Bacak OS
tarafındaki alıcı, akışın *kaynağı* olan platformda bir anlamı yok.

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt-get install mingw-w64 nsis   # gcc-mingw-w64 linker + NSIS kurulum paketleyici

bacak-remote-server/packaging/windows/build.sh
# -> bacak-remote-server/packaging/windows/bacak-remote-server-setup.exe
```

`build.sh`'nin yaptıkları ve her birinin neden var olduğu:

- **`.cargo/config.toml`**, `x86_64-pc-windows-gnu` hedefini özellikle
  `x86_64-w64-mingw32-gcc-posix`'e yönlendiriyor (`update-alternatives`
  üzerinden `x86_64-w64-mingw32-gcc`'nin varsayılan olarak seçtiği şeye
  değil — `win32` thread modeli varyantında Rust'ın std'sinin ihtiyaç
  duyduğu bazı parçalar eksik) ve `-C target-feature=+crt-static`
  ayarlıyor; böylece gönderilen `.exe` yalnızca standart Windows
  DLL'lerine bağımlı (`kernel32`, `user32`, `ws2_32`, `d3d11`, `dxgi`,
  `msvcrt`) — `objdump -p` ile doğrulandı, paketlenecek ya da kullanıcıdan
  kurmasını isteyeceğimiz bir `libwinpthread-1.dll`/`libgcc_s_seh-1.dll`/
  `libstdc++-6.dll` yok.
- **`vendor/scrap-0.5.0/`**, `scrap` crate'inin yerelde yamalanmış bir
  kopyası. Upstream'in `build.rs`'i yakalama arka ucunu `cfg!(windows)` ile
  seçiyor — bu, cross-compile edilen `--target`'ı değil, bir build
  script'inin çalıştığı *host*'u yansıtıyor; bu yüzden Linux'tan derlemek
  her zaman X11 arka ucunu seçip Windows'un DXGI'sine karşı linklemeyi
  başaramıyordu. Yamalanmış kopya bunun yerine Cargo'nun `TARGET` ortam
  değişkenini okuyor (tek satırlık bir düzeltme; dosyanın kendi yorumuna
  bakın). Workspace `Cargo.toml`'undaki `[patch.crates-io]` ile sabitlendi,
  böylece yerel Linux derlemesi etkilenmiyor — yama uygulandıktan sonra
  `cargo check --workspace` ile doğrulandı.
- **`installer.nsi`** (`makensis` ile derlendi, Debian'ın `nsis` paketinden
  — burada da cross-platform çalışıyor, Windows gerekmiyor) `Program
  Files`'a kuruyor, Başlat Menüsü kısayolları ekliyor, bir kaldırıcı
  kaydediyor ve 9910–9911 portları için gelen Windows Güvenlik Duvarı UDP
  kuralını açıyor — bu kural olmadan Windows Defender Güvenlik Duvarı,
  istemcinin `Hello`'sunu hiçbir hata vermeden sessizce düşürür; bu da
  aksi halde çok kafa karıştırıcı bir ilk-çalıştırma hatası olurdu.

**Henüz yapılmayan:** bu `.exe`/kurulum paketi üretildi ve incelendi
(`file`, `objdump`) ama **gerçek bir Windows makinesinde hiç
çalıştırılmadı** — DXGI yakalamanın ya da `enigo`'nun `SendInput`
enjeksiyonunun orada gerçekten çalıştığını doğrulamak için Windows
donanımı/VM'i yoktu. `scrap`'in DXGI arka ucu (bize ait olmayan, miras
alınan 3. parti kod) birkaç yerde `mem::uninitialized()` kullanıyor —
kullanımdan kaldırılmış ve teknik olarak UB, ama struct'lar hemen
ardından DXGI/Direct3D çağrısı tarafından dolduruluyor; bu, crate
yazıldığında bunu kabul edilebilir kılan örüntü. Windows derlemesini
"gerçekten bir ekrana karşı çalıştırılıp doğrulandı" değil, "doğru
derleniyor ve linkleniyor" olarak değerlendirin.

## Lisans

GPL-3.0-or-later (BacakOS'un geri kalanıyla eşleşir).
