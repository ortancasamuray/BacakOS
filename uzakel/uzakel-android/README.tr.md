# Uzakel — BacakOS Uzaktan Kontrol ve Dosya Transferi

🌐 **Türkçe** · [English](README.md)

> **Durum: gerçek donanımda uçtan uca doğrulandı, eşleştirme zorunlu.**
> `daemon/` ve `android/` gerçekten birbirine karşı çalıştırıldı — fiziksel
> bir Android telefon (MIUI, Android 11), gerçek bir BacakOS oturumunun
> `bacak-compositor`'ında gerçek Wi-Fi üzerinden keşfedip, PIN ile eşleşip,
> gerçek imleci hareket ettirdi; sonuç çekirdek `REL_X`/`REL_Y` olayları
> doğrudan `/dev/input/eventN`'den yakalanarak doğrulandı. Dosya transferi
> de aynı şekilde doğrulandı (gerçek SHA-256 doğrulamalı bir dosya
> `~/İndirilenler`'e indi). Bu süreçte hiçbir kod incelemesinin
> yakalayamayacağı üç gerçek bug bulunup düzeltildi, ayrıca trackpad
> hissiyatı ayarlandı — bkz. ARCHITECTURE.md §6. Eşleştirme artık hem
> girdi hem dosya transferi soketlerini koruyor (IP tabanlı, kriptografik
> değil — bkz. [ARCHITECTURE.md](ARCHITECTURE.md) §2.3'teki güvenlik
> notu); bu da aynı gerçek kurulumda doğrulandı: eşleşmemiş bir
> göndericinin trafiği hem eşleşme öncesi hem sonrası gerçek telefonla
> teyit edilerek düşürülüyor/reddediliyor.

Uzakel, BacakOS için iki parçalı bir uzaktan kontrol ve dosya transferi
ekosistemidir: bir telefonu BacakOS makinesi için trackpad/klavye/dosya
transferi istemcisine dönüştüren bir Android uygulaması, ve BacakOS
tarafında girdiyi simüle edip dosyaları alan bir Rust daemon.

## Bileşenler

| Bileşen | Dil | Rol |
|---|---|---|
| **Daemon** (host, BacakOS) | Rust | LAN'da keşfedilebilir; `/dev/uinput` üzerinden fare/klavyeyi simüle eder; dosyaları TCP üzerinden alır; `systemd --user` servisi olarak çalışır. |
| **İstemci** (Android) | Kotlin, Jetpack Compose | Trackpad + IME-köprülü klavye arayüzü; girdiyi UDP üzerinden gönderir; dosyaları TCP üzerinden gönderir (alım yönü henüz hiçbir tarafta uygulanmadı); host'ları keşfeder, eşleştirir ve hatırlar. |

Kablo protokolü, paket yerleşimi ve her iki bileşenin iç modül tasarımı
için [ARCHITECTURE.md](ARCHITECTURE.md) (İngilizce) /
[ARCHITECTURE.tr.md](ARCHITECTURE.tr.md) (Türkçe özet) dosyalarına bakın.

## Neden VNC/RDP değil?

Uzakel bir ekran paylaşımı protokolü değildir — hiçbir zaman bir video
akışını kodlayıp iletmez. Daha çok, "aynı ağdaki bu belirli BacakOS
makinesini telefonumdan kontrol et" için özel olarak tasarlanmış bir
Bluetooth trackpad/klavye artı sürükle-bırak dosya transferine yakındır —
bu da gecikmeyi ve bant genişliğini herhangi bir uzak masaüstü
protokolünün çok altında tutar.

## Depo yerleşimi

```
uzakel/
├── daemon/            # Rust — BacakOS tarafındaki servis
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs           # daemon giriş noktası, üç soketi de bağlar, /dev/uinput'u açar
│       ├── discovery.rs      # UDP yayın yanıtlayıcısı + PIN eşleştirme
│       ├── trust.rs           # TrustStore — eşleştirilmiş IP'lerin paylaşılan kaydı
│       ├── protocol.rs       # paylaşılan paket tanımları (başlık, opcode'lar) + encode/decode + testler
│       ├── input_manager.rs  # /dev/uinput sanal fare + klavye
│       └── file_server.rs    # parçalı TCP dosya alımı + SHA-256 doğrulama
└── android/           # Kotlin + Jetpack Compose — telefon tarafındaki istemci
    └── app/src/main/kotlin/org/anadolupanteri/uzakel/
        ├── protocol/          # Protocol.kt — daemon/src/protocol.rs'in bayt-bayt Kotlin karşılığı
        ├── discovery/         # SavedHostsStore — kalıcı eşleştirilmiş host listesi
        ├── network/           # NetworkClient — keşif taraması, PIN eşleştirme, InputChannel (UDP), dosya gönderme (TCP)
        ├── input/             # TrackpadView (çok dokunuşlu jest yüzeyi), KeyCodes, IME→KEY_PRESS köprüsü
        ├── transfer/          # FileTransferManager — SAF dosya seçimi, SHA-256, StateFlow ilerleme
        └── ui/                # UzakelApp (ekran geçişi) + DeviceListScreen, ControlScreen, TransferScreen
```

## Derleme

```sh
# Daemon
cd daemon && cargo build --release && cargo test
UZAKEL_DOWNLOAD_DIR=~/İndirilenler ./target/release/uzakel-daemon   # veya (paketlendiğinde): systemctl --user enable --now uzakel-daemon

# Android
cd android && ./gradlew assembleDebug   # app/build/outputs/apk/debug/app-debug.apk üretir
```

Daemon'ın `/dev/uinput`'ı açabilmesi için çalıştıran kullanıcının `uinput`
grubunda olması gerekir; açamazsa bu gereksinimi net şekilde belirten bir
hata loglar. Eşleştirme PIN'i ve dosya-alındı bildirimleri için
`notify-send`'e çıkar — bildirimler görünmüyorsa `libnotify-bin` (veya
eşdeğerini) kurun.

Android derlemesi, `ANDROID_SDK_ROOT`/`local.properties`'in platform 34 +
build-tools 34.0.0 kurulu bir Android SDK'ya işaret etmesini gerektirir
(Android Studio bunu otomatik kurar); bunun dışında olağandışı bir
gereksinimi yok.

## Lisans

GPL-3.0-or-later (BacakOS'un geri kalanıyla aynı).
