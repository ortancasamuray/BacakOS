# Uzakel — BacakOS Uzaktan Kontrol ve Dosya Transferi

🌐 **Türkçe** · [English](README.md)

> **Durum: hem `daemon/` hem `android/` ilk sürümüyle uygulandı.**
> `daemon/` derleniyor, birim testlerini geçiyor ve keşif/eşleştirme, girdi
> replay'i, dosya alımını uçtan uca uyguluyor. `android/` çalışan bir debug
> APK üretiyor ve Android Lint'i geçiyor (gerçek bir Gradle + Android SDK
> derlemesiyle doğrulandı, sadece yazılıp umut edilmedi) — cihaz keşfi, PIN
> eşleştirme, trackpad + IME-köprülü klavye ve tek yönlü dosya gönderme
> bağlı. İki taraf henüz gerçek donanımda birbirine karşı hiç
> çalıştırılmadı, ve eşleştirme hâlâ girdi/dosya soketlerinde zorunlu
> kılınmıyor — bkz. [ARCHITECTURE.tr.md](ARCHITECTURE.tr.md)'nin "açık
> sorular" bölümündeki güvenlik notu.

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
