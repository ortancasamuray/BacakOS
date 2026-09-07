# Uzakel — BacakOS Uzaktan Kontrol ve Dosya Transferi

🌐 **Türkçe** · [English](README.md)

> **Durum: daemon uygulandı, istemci henüz başlamadı.** `daemon/` derleniyor,
> birim testlerini geçiyor ve keşif/eşleştirme, girdi replay'i, dosya
> alımını uçtan uca uyguluyor. `android/` henüz yok. Eşleştirme, girdi/dosya
> soketlerinde henüz zorunlu kılınmıyor — bkz.
> [ARCHITECTURE.tr.md](ARCHITECTURE.tr.md)'nin "açık sorular" bölümündeki
> güvenlik notu.

Uzakel, BacakOS için iki parçalı bir uzaktan kontrol ve dosya transferi
ekosistemidir: bir telefonu BacakOS makinesi için trackpad/klavye/dosya
transferi istemcisine dönüştüren bir Android uygulaması, ve BacakOS
tarafında girdiyi simüle edip dosyaları alan bir Rust daemon.

## Bileşenler

| Bileşen | Dil | Rol |
|---|---|---|
| **Daemon** (host, BacakOS) | Rust | LAN'da keşfedilebilir; `/dev/uinput` üzerinden fare/klavyeyi simüle eder; dosyaları TCP üzerinden alır; `systemd --user` servisi olarak çalışır. |
| **İstemci** (Android) | Kotlin, Jetpack Compose | Trackpad + sanal klavye arayüzü; girdiyi UDP üzerinden gönderir; dosyaları TCP üzerinden gönderir/alır; host'ları keşfeder ve hatırlar. |

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

## Planlanan depo yerleşimi

```
uzakel/
├── daemon/            # Rust — BacakOS tarafındaki servis
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs           # CLI/daemon giriş noktası, systemd entegrasyonu
│       ├── discovery.rs      # mDNS / UDP yayın yanıtlayıcısı
│       ├── protocol.rs       # input_manager ve file_server'ın paylaştığı paket tanımları (başlık, opcode'lar)
│       ├── input_manager.rs  # /dev/uinput sanal fare + klavye
│       └── file_server.rs    # parçalı TCP dosya al/gönder + SHA-256 doğrulama
└── android/           # Kotlin — telefon tarafındaki istemci
    └── app/src/main/kotlin/org/anadolupanteri/uzakel/
        ├── discovery/         # host keşfi + kaydedilmiş host listesi
        ├── network/           # NetworkClient (coroutine), UDP girdi soketi, TCP dosya soketi
        ├── input/             # TrackpadView delta yakalama, hassasiyet/ivme
        ├── transfer/          # FileTransferManager, SAF entegrasyonu, ilerleme durumu
        └── ui/                # Compose ekranları: trackpad, klavye, cihaz listesi, transfer paneli
```

## Derleme

```sh
# Daemon (uygulandı)
cd daemon && cargo build --release && cargo test
UZAKEL_DOWNLOAD_DIR=~/İndirilenler ./target/release/uzakel-daemon   # veya (paketlendiğinde): systemctl --user enable --now uzakel-daemon

# Android (henüz başlamadı)
cd android && ./gradlew assembleDebug
```

Daemon'ın `/dev/uinput`'ı açabilmesi için çalıştıran kullanıcının `uinput`
grubunda olması gerekir; açamazsa bu gereksinimi net şekilde belirten bir
hata loglar. Eşleştirme PIN'i ve dosya-alındı bildirimleri için
`notify-send`'e çıkar — bildirimler görünmüyorsa `libnotify-bin` (veya
eşdeğerini) kurun.

## Lisans

GPL-3.0-or-later (BacakOS'un geri kalanıyla aynı).
