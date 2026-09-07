# Uzakel — Mimari Özeti

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Bu, [ARCHITECTURE.md](ARCHITECTURE.md) dosyasının kısa Türkçe özetidir.

> **Durum: `daemon/` (Rust) uygulandı ve bu belgeyle örtüşüyor;
> `android/` henüz yok.** Bu belge daemon davranışını anlatırken gerçek
> `daemon/src/` kodunu anlatıyordur. Android istemcisini anlatan kısımlar
> hâlâ bir plandır.

İki bileşen, üç ağ kanalı, tek bir paylaşılan kablo protokolü.

## 1. Sistem genel bakışı

Üç bağımsız kanal, her biri kendi trafiğine uygun:

| Kanal | Taşıma | Neden |
|---|---|---|
| Keşif + eşleştirme | 45922 portunda UDP yayın (`UZAKEL_DISCOVERY_PORT`) | Sıfır-yapılandırma LAN eşleştirmesi; kaybolan bir yayın yalnızca sıradaki periyodik yayının başarılı olması demektir. **Gerçek mDNS değil** — aşağıdaki nota bakın. |
| Girdi (fare/klavye) | UDP, 9876 portu (`UZAKEL_INPUT_PORT`) | Gecikme, güvenilirlikten daha önemli — düşen bir `MOUSE_MOVE` deltası fark edilmez; yeniden gönderilen bayat bir tanesi gecikmeli hissettirir. |
| Dosya transferi | TCP, 9877 portu (`UZAKEL_FILE_PORT`) | Doğruluk, gecikmeden daha önemli — dosyalar bayt-bayt eksiksiz gelmeli. |

**Keşif portu üzerine:** buradaki asıl plan standart 5353 portunda mDNS'ti.
Uygulama bunun yerine özel bir portta (45922) düz bir UDP yayın
yanıtlayıcısı — `avahi-daemon` çoğu masaüstü Linux sisteminde zaten
5353'ü elinde tutuyor, ve elle yazılmış bir yanıtlayıcının bu portu
gerçek mDNS/DNS-SD trafiğiyle paylaşması onu karıştırma (veya onun
tarafından karıştırılma) riski taşır. Gerçek mDNS desteği, istenirse,
hâlâ açık — bkz. §5.

## 2. Kablo protokolü

Tüm paketler sabit bir ikili başlığı paylaşır, little-endian:

```
┌─────────┬─────────┬──────────────┬─────────────────┐
│ magic   │ version │ opcode       │ payload_len      │
│ u16     │ u8      │ u8           │ u32              │
├─────────┴─────────┴──────────────┴─────────────────┤
│ payload (payload_len bayt, opcode'a özgü yerleşim)   │
└──────────────────────────────────────────────────────┘
```

`magic`, port üzerindeki başıboş bir paketin (veya sürüm uyuşmazlığının)
`/dev/uinput`'a dokunmadan önce reddedilmesini sağlayan sabit bir sabittir.
`version`, daemon ve istemcinin protokol kaymasını fark edip payload'ları
sessizce yanlış yorumlamak yerine eşleşmeyi reddetmesine izin verir.

### 2.1 Girdi kanalı opcode'ları (UDP)

| Opcode | Payload | Anlamı |
|---|---|---|
| `MOUSE_MOVE` | `dx: i16, dy: i16` | İstemcinin hassasiyet/ivme eğrisiyle zaten ölçeklenmiş göreli imleç deltası. |
| `MOUSE_CLICK` | `button: u8, state: u8` | `button`: sol/sağ/orta; `state`: bas/bırak. |
| `MOUSE_SCROLL` | `dx: i16, dy: i16` | Göreli kaydırma deltası (trackpad görünümünde iki parmak sürükleme). |
| `KEY_PRESS` | `keycode: u16, modifiers: u8, state: u8` | `modifiers` bir bit maskesi (Shift/Ctrl/Alt/Super); `state` bas/bırak — basılı tutulan tuşlar ve tekrarlar kablonun değil istemcinin sorumluluğudur. |

Her girdi paketi ayrıca payload'ında monoton bir `seq: u32` taşır, böylece
`input_manager.rs` sırası bozuk bir UDP paketini, imleci anlık olarak geri
hareket ettirmek yerine düşürebilir.

### 2.2 Dosya transferi kanalı (TCP)

Bir transfer tek bir TCP bağlantısı, tek bir dosya, üç aşamadır: **el
sıkışma** (`FILE_META { name, size, sha256 }`, `FILE_ACCEPT`/`FILE_REJECT`
ile onaylanır), **akış** (dosya gövdesi, her biri `CHUNK { index, len }`
ile öncelenmiş ardışık 64 KiB parçalar halinde; alıcı istediği an
`TRANSFER_CANCEL` gönderebilir), **doğrulama** (alıcı, yeniden birleştirilen
dosya üzerinde SHA-256 hesaplar ve el sıkışmanın hash'iyle karşılaştırır;
uyuşmazlıkta `FILE_CORRUPT` bildirir ve sessizce kesilmiş bir dosyayı
tutmak yerine kısmi dosyayı siler). Yön simetrik olmak *üzere tasarlandı*
— hangi taraf gönderiyorsa aynı durum makinesi — ama **bugün
`file_server.rs`'te yalnızca alım tarafı (Android → BacakOS) uygulandı**;
daemon-başlatımlı bir gönderim (BacakOS → Android) henüz bağlanmadı (§5).
Şu anki kodda `TRANSFER_CANCEL`, alıcı tarafından bir sonraki `CHUNK`
yerine okunur — yani bugün transferi iptal eden taraf, daha fazla veri
yerine bunu gönderen taraftır, alıcının geri gönderdiği bir şey değil.

### 2.3 Eşleştirme

`discovery.rs`, başlangıçta (ve her başarılı eşleştirmeden sonra tekrar —
böylece yakalanan bir PIN tekrar oynatılamaz) taze bir 6 haneli PIN üretir
ve masaüstü bildirimiyle gösterir. Bir istemci, keşif UDP soketi üzerinden
`PAIR_REQUEST { pin }` gönderir; daemon karşılaştırır ve
`PAIR_RESPONSE { accepted }` ile yanıtlar.

**Bu henüz bir güvenlik sınırı değil.** Doğru bir PIN, bugün yalnızca
daemon'ın bir eşleşme kaydettiğini kanıtlar — bu onayı girdi (UDP) veya
dosya transferi (TCP) soketlerine bağlayan hiçbir şey yok; bu ikisi şu an
eşleştirilmiş olsun olmasın LAN'daki *herhangi* bir gönderenden paket
kabul ediyor, ve hiç TLS/sertifika materyali değişimi yok. §5'teki TLS
yaklaşımlarından birini seçip bir güven deposunu bu iki kanala bağlamak,
burada hâlâ açık olan gerçek güvenlik işidir; şu anki PIN kontrolünü bir
garanti değil, bir kullanıcı-deneyimi inceliği ("telefonun göstermesi
gereken PIN bu") olarak görün.

## 3. Daemon (Rust) — modül tasarımı

```
daemon/src/
├── main.rs            # ortam değişkeni tabanlı yapılandırma, üç soketi bağlar, /dev/uinput'u açar, üç görevi başlatır
├── discovery.rs        # UDP yayın yanıtlayıcısı; DISCOVER_REQUEST + PIN eşleştirmesini (§2.3) yanıtlar
├── protocol.rs          # paket başlığı/opcode tanımları + encode/decode, her modülün paylaştığı
├── input_manager.rs     # UDP soketi → girdi opcode'larını ayrıştırır → /dev/uinput üzerinden yeniden oynatır
└── file_server.rs       # TCP dinleyici → parçalı alım, SHA-256 doğrulama, ~/İndirilenler'e yazar
```

`input_manager.rs`, `input-linux` crate'iyle `/dev/uinput` üzerinden
oluşturulmuş sanal bir fare ve klavye aygıtını sahiplenir (geçerli her
evdev tuş kodu, üç fare düğmesi, X/Y/tekerlek göreli eksenleri baştan
kaydedilir). Ham deltalara enjekte etmeden önce hafif bir ivme eğrisi
uygular — istemcinin zaten uyguladığı eğrinin üzerine, ham delta
gönderen bir istemci için güvenlik ağı olarak — ama imleç konumunu
kendisi sıkıştırmaz: her olay `REL_X`/`REL_Y` (göreli)'dir, bu yüzden
ekran-kenarı sıkıştırması tamamen compositor'ın işidir, gerçek bir fare
için olduğu gibi. Bir `tokio` görevinde UDP soketini sıkı bir döngüde
okur ve kaynak-adresi başına son görülen `seq`'i takip eder, böylece
sırası bozuk veya yinelenen bir UDP paketi imleci geri hareket ettirmek
yerine düşürülür.

`file_server.rs`, düz bir `tokio` TCP dinleyicisidir; kabul edilen her
bağlantı, §2.2'deki durum makinesinin alım tarafını çalıştıran kendi
görevini alır, çakışmasız bir hedef dosya adı seçer (`ad`, sonra
`ad (2)`, `ad (3)`, …) — `altay`'ın masaüstü tarafındaki transfer
modülünün yaptığı aynı şekilde. Başarılı doğrulamada `notify-send`'e
çıkar (kasıtlı bir kapsam kısıtlaması — bkz. §5 — `org.freedesktop.
Notifications`'ı doğrudan D-Bus üzerinden konuşmak yerine).

`discovery.rs`, `DISCOVER_REQUEST`'i daemon'ın adı ve sürümüyle yanıtlar,
ve §2.3'teki PIN eşleştirme el sıkışmasını çalıştırır — ikisi de aynı UDP
yayın soketi üzerinde.

Kullanıcının oturumuyla başlaması ve asla root gerektirmemesi için bir
`systemd --user` servisi olarak çalışması hedefleniyor (README'ye bakın)
— `/dev/uinput`'ı `uinput` grup üyeliği üzerinden açar, BacakOS'un oturum
kurulumunun `bacak/packaging/bacak-session`'ın diğer kullanıcı-kapsamlı
masaüstü servisleri için yaptığı gibi kullanıcıya vermesi gereken bir
yetki. Gerçek systemd birim dosyası henüz yazılmadı (§5).

## 4. Android istemcisi (Kotlin) — modül tasarımı

```
android/app/src/main/kotlin/org/anadolupanteri/uzakel/
├── discovery/    # host tarama + daha önce eşleştirilmiş host'ların kalıcı listesi
├── network/      # NetworkClient: coroutine/Flow tabanlı UDP girdi soketi + TCP dosya soketi
├── input/        # TrackpadView: ham dokunma deltaları → hassasiyet/ivme → UDP paketleri
├── transfer/     # FileTransferManager: SAF dosya seçimi, parçalı yükleme/indirme, ilerleme Flow'u
└── ui/           # Compose ekranları: trackpad, sanal klavye, cihaz listesi, transfer paneli
```

`input/TrackpadView`, ham dokunma olaylarını `input_manager.rs`'nin
beklediği aynı `dx`/`dy` deltalarına çeviren bir Compose `pointerInput`
yüzeyidir — tek parmak sürükleme = `MOUSE_MOVE`, tek parmak dokunuş =
`MOUSE_CLICK` (sol), iki parmak dokunuş = `MOUSE_CLICK` (sağ), iki parmak
sürükleme = `MOUSE_SCROLL`. Hassasiyet ve ivme, paket gönderilmeden önce
istemci tarafında uygulanır, böylece daemon telefonun dokunma
çözünürlüğünü hiç tahmin etmek zorunda kalmaz. `network/NetworkClient`,
her iki soketi de bir coroutine/`Flow` API'sinin arkasında sahiplenir:
girdi paketleri gönder-ve-unut'tur (onay beklenmez, §2.1'in UDP
tasarımıyla uyumlu), dosya transferi ise transfer panelinin ilerleme
çubuğunu sürüklemek için topladığı bir `Flow<TransferProgress>` sunar.
`transfer/FileTransferManager`, hem gönderilecek bir dosya seçmek hem de
bir indirme için hedef seçmek için Android'in Storage Access
Framework'ünü kullanır, böylece asla geniş depolama izinlerine ihtiyaç
duymaz — BacakOS'un masaüstü tarafındaki dosya işlemenin (`altay`'ın
`security::Sandbox`'ı) izlediği "shell'e çıkma yok, tasarım gereği
sandbox'lı" içgüdüsüyle örtüşür. `discovery/`, eşleştirilmiş host'ları
(ad, son bilinen IP, PIN el sıkışmasından gelen güven anahtarı) kalıcı
hale getirir, böylece geri dönen bir kullanıcının her oturumda yeniden
eşleşmesi gerekmez, ve LAN cihazları oturumlar arasında sıkça adres
değiştirdiğinden (DHCP kira döngüsü) her başlatmada mevcut IP'yi
mDNS/yayın üzerinden yeniden çözer.

## 5. Açık sorular / henüz karara bağlanmadı

- **Eşleştirme henüz zorunlu kılınmıyor.** Girdi ve dosya transferi
  soketleri, eşleştirme PIN durumundan bağımsız olarak LAN'daki herhangi
  bir göndericiden kabul ediyor — bkz. §2.3'teki not. "Ne inşa edildi" ile
  "güvenilir bir ev LAN'ının ötesine açmak için güvenli olan" arasındaki
  en büyük fark budur.
- Eşleştirme sonrası oturumlar için TLS materyali: eşleştirme anında
  sabitlenen kendinden imzalı sertifika (en basit, CA gerekmez) mı yoksa
  PIN değişimine bağlı daha hafif bir PSK şeması mı — ve sonra UDP/TCP
  soketlerini buna gerçekten bağlamak.
- Daemon-başlatımlı dosya gönderimleri (BacakOS → Android) —
  `file_server.rs` şu an yalnızca alım tarafını uyguluyor.
- 45922'deki düz UDP yayın yanıtlayıcısı yerine gerçek mDNS/DNS-SD (bir
  mDNS crate'i veya elle yazılmış multicast DNS kayıtları gerektirir).
- Bir `systemd --user` birim dosyası + paketleme (`.deb`) — daemon bugün
  kabuktan sorunsuz çalışıyor ama henüz hiçbir yere servis olarak
  kurulmuyor.
- `MOUSE_SCROLL`'un, gerçek trackpad kullanımı bir tercih ortaya çıkardığında
  `MOUSE_MOVE`'dan ayrı kendi ivme eğrisine ihtiyacı olup olmadığı.
- Çoklu istemci davranışı: iki telefon aynı daemon'ı aynı anda kontrol
  edebilir mi, yoksa yeni bir istemciyi eşleştirmek öncekini düşürür mü?
  Şu an eşleştirilmiş veya eşleştirilmemiş her istemci eşit kabul
  edildiğinden, bu henüz pratikte geçerli değil.
- `discovery.rs`/`file_server.rs`'teki `notify-send` kabuk çağrısını yerel
  bir `org.freedesktop.Notifications` D-Bus çağrısıyla (ör. `zbus` ile)
  değiştirmek, `libnotify-bin` çalışma zamanı bağımlılığını kaldırmak.
