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

> **v1 durumu: iki ayrı makinede, gerçek LAN/Wi-Fi üzerinde uçtan uca
> doğrulandı.** `bacak-remote-server` (Windows 10, ayrı fiziksel donanım
> üzerinde gerçek bir VM) ve `bacak-remote-client` (bu Linux makinesi)
> gerçek bir ağ bağlantısı üzerinden (`192.168.1.x`, loopback değil)
> birbirine karşı çalıştırıldı: gerçek `Hello`/`HelloAck` eşleşmesi, Windows
> makinesinin gerçek masaüstünün (1400×1050) yakalanması, sürekli
> sunucu-taraf CPU aktivitesiyle ve sıfır hatayla sürdürülen akış, ve —
> temiz şekilde doğrulanması birkaç deneme alan kısım — ağ üzerinden
> gönderilen gerçek işaretçi/tık paketlerinin Windows makinesinde imleci
> görsel olarak hareket ettirip tıklaması, o ekranı izleyen bir kişi
> tarafından doğrulandı. Tam hikaye (bu test yönteminin ortaya çıkardığı
> gerçek bir bug ve ilk girdi testlerinin neden yanıltıcı sonuç verdiği
> dahil) için aşağıdaki "Gerçek iki-makine testinin bulduğu şeyler"
> bölümüne bakın. Orijinal spesifikasyonda adı geçen birkaç parça (donanım
> H.264/AV1 kodlama, QUIC/WebRTC, compositor'a sıfır-kopya `dmabuf`, gerçek
> çoklu dokunma enjeksiyonu) hâlâ bilinçli
> olarak **uygulanmadı** — bunlardan herhangi birini bitmiş saymadan önce
> aşağıdaki "Dürüst kapsam" bölümüne bakın.

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

### Aynı makinede loopback (Xvfb)

Server ve client ikisi de bir `Xvfb :99` sanal X11 ekranına (1280×800)
yönlendirilmiş, `bacak-remote-client` `127.0.0.1`'e bağlanıyor:

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
  gerçek bir `libxdo` çağrısı).

### İki ayrı fiziksel makine, gerçek LAN/Wi-Fi (Windows 10 ↔ bu Linux makinesi)

`bacak-remote-server.exe`, NSIS kurulum paketiyle gerçek bir Windows 10 Pro
makinesine (`192.168.1.55`) kurulmuş; `bacak-remote-client` bu Linux
makinesinde (`192.168.1.15`, render yüzeyi için `Xvfb`) çalıştırıldı —
gerçek, ayrı donanımlı, gerçek ağ üzerinden bir test, loopback değil:

- **Eşleştirme + yakalama** — `Hello`/`HelloAck` gerçek ağ üzerinden
  başarıyla tamamlandı; sunucu Windows makinesinin *gerçek* masaüstü
  çözünürlüğünü (`1400x1050`) bildirdi (ve istemci bunu aldı) — `scrap`'in
  DXGI arka ucunun sahte bir değer değil, gerçekten o ekranı yakaladığının
  kanıtı.
- **Sürdürülen akış** — sunucunun süreç CPU süresi 5 saniyelik örnekleme
  penceresi boyunca sürekli ilerledi (kodlama+gönderme işi yalnızca
  el sıkışmada değil, sürekli oluyor), her iki UDP soket çifti de Linux
  tarafında boyunca `ESTAB` kaldı.
- **Girdi iletimi** — Linux makinesinin gerçek IP'sinden gönderilen ham tel
  formatında bir `PointerMotion`/`PointerButton` patlaması Windows
  sunucusunun girdi portuna ulaştı, doğru şekilde decode edildi ve
  `enigo`'ya **sıfır** enjeksiyon hatasıyla iletildi — tekrarlanan birkaç
  gönderimde de aynı sonuç.
- **Canlı, görsel olarak doğrulanan enjeksiyon** — Linux makinesinden
  gönderilen gerçek tel formatında bir fare süpürmesi + tık, Windows
  makinesinin gerçek ekranında canlı izlendi: imleç görsel olarak hareket
  edip tıkladı. İlk denemelerin bunu doğrularken neden yanıltıcı sonuç
  verdiği ve bu sürecin ortaya çıkardığı gerçek bir bug için aşağıdaki
  "Gerçek iki-makine testinin bulduğu şeyler" bölümüne bakın.

**Henüz test edilmeyenler:** macOS (bu geçişte bir Mac yoktu); istemcinin
kendi `winit` olayı → UDP gönderim yolunun canlı yerel bir fare/dokunuşla
test edilmesi (hem loopback hem iki-makine geçişinde hâlâ yalnızca doğrudan
enjekte edilen tel paketleriyle test edildi); gerçek paket kaybı/jitter
altındaki davranış; çok dakikalık sürekli çalışma; boş bir ekrandan çok
daha büyük sıkışacak ve parçalama yolunu çok daha zorlayacak gerçek
(Xvfb olmayan, boş olmayan) masaüstü içeriği.

## Gerçek iki-makine testinin bulduğu şeyler

Gerçek donanım hemen gerçek bir bug ortaya çıkardı, ve çözmesi birkaç
deneme alan bir ölçüm baş ağrısı bıraktı — ikisi de `uzakel`'in kendi §6
tarzında burada kayıt altına alınmaya değer, üstünün örtülmesi yerine.

**Bulunan ve etrafından dolaşılan bug: SSH üzerinden başlatılan bir
süreçten çağrıldığında `SendInput`, `tasklist`'in onu interaktif konsol
oturumunda çalışıyor olarak raporlamasına rağmen `ERROR_ACCESS_DENIED`
(Win32 hata 5) döndürüyor.** Girdi enjeksiyonunu test etmenin ilk girişimi
`bacak-remote-server.exe`'yi test makinesini yönetmek için kullanılan SSH
bağlantısı üzerinden doğrudan çalıştırdı. Video yakalama (DXGI) oradan
sorunsuz çalıştı — ama her `enigo` çağrısı sessizce `Ok(())` döndürürken
imleç hiç hareket etmedi, ve `enigo`'yu tamamen atlayan ham bir `SendInput`
çağrısı nedenini doğruladı: Windows'un OpenSSH sunucusu bir oturumun
süreçlerini kendi, varsayılan-olmayan bir pencere istasyonuna yerleştiriyor,
ve `SendInput` özellikle *interaktif* pencere istasyonuna
(`WinSta0\Default`) erişim gerektiriyor — Desktop Duplication'ın (yakalama
için kullanılan) paylaşmadığı bir kısıtlama. Yalnızca oturum ID'si
(`tasklist`/`query session`'ın raporladığı şey) bir sürecin gerçekte hangi
pencere istasyonuna bağlı olduğunu söylemiyor. **Pratik sonuç:
`bacak-remote-server`'ın kendisini test için SSH üzerinden çalıştırmayı
denemeyin — gerçek bir interaktif oturumdan (konsol, RDP, ya da fiziksel
olarak makinenin başında) çalıştırın.** Bunun gerçek dağıtımlarla bir ilgisi
yok (kimse kendi PC'sine kendi uzaktan-kontrol sunucusunu çalıştırmak için
SSH ile bağlanmaz), ama bunun test edildiği şekilde otomatikleştirmeye
çalışan biri için keskin bir kenar.

**İzole olarak çalıştığı doğrulandı: hem ham `SendInput` hem de `enigo`'nun
onun etrafındaki sarmalayıcısı, gerçek bir interaktif oturumdan
çalıştırıldığında bu makinede gerçek imleci doğru şekilde hareket
ettiriyor.** İki bağımsız tanılama (biri doğrudan
`windows::Win32::UI::Input::KeyboardAndMouse::SendInput` çağıran, biri
`enigo::Enigo::move_mouse` çağıran) her ikisi de imleci gerçek bir
başlangıç konumundan beklenen tam sonuca taşıdı — ekranı aşacak boyutta
göreli bir hareketten sonra ekranın sağ-alt köşesinde kenetlenerek
(`1400x1050` ekranda `(1399, 1049)`), tam olarak gerçek bir
`MOUSEEVENTF_MOVE`/`SendInput` çağrısının bir ekran kenarında ürettiği
davranış. İkisi de aynı temiz, belirsizliğe yer bırakmayan sonuçla iki kez
çalıştırıldı.

**Çözüldü: canlı sunucu hattı üzerinden, görsel olarak doğrulanan uçtan
uca enjeksiyon.** "Operatör Linux tarafından bir paket gönderiyor" ile
"Windows makinesindeki kişi bir imleç konumu okuyor"u sohbet-aracılı,
iki-insanlı, iki-makineli bir kurulum üzerinden ilişkilendirmenin ilk
denemeleri gürültülü, tutarsız delta'lar üretti — bunun sebebi Windows
makinesinin bir VM olması ve host'un fare-entegrasyon katmanının, olağan
sohbet gidiş-dönüş zamanlama gevşekliğinin üstüne, iki manuel konum
okuması arasında guest imlecini kendiliğinden hafifçe kaydırmasıydı.
Sayısal bir önce/sonra okumasından doğrudan "ekranı izle, görsel olarak
onayla" kontrolüne geçmek (büyük, hızlı bir süpürme + tık, anında görülmesi
kolay) temiz, belirsizliğe yer bırakmayan bir sonuç verdi: **imleç görsel
olarak hareket edip tıkladı**, makinenin başındaki kişi tarafından
doğrulandı. Yukarıdaki izole `SendInput`/`enigo` tanılamalarıyla ve
sunucunun hatasız alım loglarıyla birleştiğinde, tüm zincir — gerçek ağ →
decode → `enigo` → gerçek Windows donanımında görsel imleç hareketi —
artık yalnızca çıkarım değil, doğrulanmış durumda.

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

- `PairRequest` / `PairResponse` — açık metinde gönderilen tek mesajlar;
  eşleştikten sonra geri kalan her şey `Encrypted` içine sarılarak gider
  (aşağıdaki "Güvenlik ve eşleşme" bölümüne bakın).
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

## Güvenlik ve eşleşme

Eşleşme, yeni bir şema icat etmek yerine `uzakel`'in tam olarak aynı
şemasını yeniden kullanıyor (`uzakel/daemon/src/crypto.rs`, orada gerçek
donanımda doğrulanmış): sunucunun başlangıçta konsoluna yazdığı 6 haneli
bir PIN, geçici bir **X25519 ECDH** anahtar değişimiyle birleştirilmiş. PIN
tek başına hiçbir zaman telin üzerinden geçmiyor ve asla bir şifreleme
anahtarı olarak kullanılmıyor — yalnızca anahtar değişimini doğrulamak için
kullanılıyor (istemcinin sunucunun genel anahtarına güvenmeden önce
kontrol ettiği bir HMAC-SHA256 `confirm_tag` üzerinden), böylece değişimi
izleyen pasif bir dinleyici kullanılabilir hiçbir şey öğrenmiyor, ve PIN'i
bilmeyen bir ortadaki-adam kendi anahtarlarını sessizce ikame edemiyor.
Tam türetme ve dürüst uyarılar için `bacak-remote-proto/src/crypto.rs`'in
modül belgesine bakın (bu tam bir PAKE değil — PIN'i zaten bilen bir
saldırgan hâlâ geçerli görünen bir el sıkışma tamamlayabilir, `uzakel`'in
kendi şeması için belgelediği aynı sınırlama).

Bu projeye özgü bir detay (`uzakel`'de yok, çünkü onun tek bir şifreli
kanalı var): video ve girdi **ayrı UDP soketlerinde** gidiyor, bu yüzden
her biri `SessionMaterial::channel_keys("video" | "input")` üzerinden
**kendi bağımsız türetilmiş anahtar çiftini** alıyor — bir anahtarı iki
bağımsız sayılan nonce dizisinde yeniden kullanmak gerçek bir (anahtar,
nonce) tekrar kullanımı hatası olurdu. `bacak_remote_proto::crypto`'nun
modül belgesi bunu ayrıntılı anlatıyor; bu, şemanın `uzakel`'in
orijinalini birebir kopyalamak yerine genişletmesi gereken tek yer.

Test edildi (loopback, Xvfb): doğru PIN ile eşleşme başarılı oluyor ve
normal akıyor; yanlış PIN ile eşleşme her iki tarafta da temiz şekilde
reddediliyor (sunucu loglayıp anahtar türetmeyi/saklamayı reddediyor;
istemci sonsuza kadar askıda kalmak ya da yeniden denemek yerine net bir
"sunucu eşleşmeyi reddetti" hatası alıyor).

**Henüz yapılmayan:** değişimi yakalayan bir saldırgana karşı PIN-tahmin
direnci için gerçek bir PAKE (SPAKE2/OPAQUE); başarılı bir eşleşmeden
sonra PIN rotasyonu (`uzakel`'in daemon'u bunu yapıyor, bu projenin
sunucusu henüz yapmıyor — aynı PIN süreç ömrü boyunca geçerli); PIN
girmek için bir arayüz (v1 bir CLI konumsal argümanı — bkz. "Derleme &
çalıştırma").

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
#   Pairing PIN: 123456   <- başlangıçta bir kez gösterilir; istemciye bunu girin

# Bacak OS makinesinde (ya da şimdilik aynı LAN'daki herhangi bir test makinesinde):
./target/release/bacak-remote-client <sunucu-lan-ip> <eşleşme-pin>
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

**Güncelleme: bu artık gerçek bir Windows 10 makinesinde çalıştırıldı** —
kurulum paketi, güvenlik duvarı kuralı, DXGI yakalama (gerçek masaüstü
çözünürlüğü tespit edilip akıtıldı) ve `enigo`'nun `SendInput` enjeksiyonu
(gerçek imleci hareket ettirip tıklatarak, makinenin başındaki kişi
tarafından canlı izlenerek) hepsi çalışıyor. Tam hikaye
için (Windows'un kendisiyle ilgisi olmayan, yalnızca bu şekilde test
etmeye kalkışınca ısıran bir SSH pencere-istasyonu kısıtlaması dahil)
yukarıdaki "Gerçek iki-makine testinin bulduğu şeyler" bölümüne bakın.
`scrap`'in DXGI arka ucu (bize ait olmayan, miras alınan 3. parti kod)
hâlâ birkaç yerde `mem::uninitialized()` kullanıyor — kullanımdan
kaldırılmış ve teknik olarak UB, ama struct'lar hemen ardından
DXGI/Direct3D çağrısı tarafından dolduruluyor; bu, crate yazıldığında
bunu kabul edilebilir kılan örüntü. Gözlemlenen bir hataya yol açmadı,
ama bilinmesi gereken miras teknik borç.

## Lisans

GPL-3.0-or-later (BacakOS'un geri kalanıyla eşleşir).
