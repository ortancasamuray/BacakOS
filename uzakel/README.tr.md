# Uzakel — BacakOS Uzaktan Kontrol ve Dosya Transferi

🌐 **Türkçe** · [English](README.md)

> **Durum: tasarım aşaması.** Henüz hiç kod yazılmadı — bu belge ve
> [ARCHITECTURE.tr.md](ARCHITECTURE.tr.md), hedeflenen sistemi anlatır.
> Aşağıdakilerin hiçbiri "bitti" olarak okunmamalı.

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

## Derleme (kod yazıldığında)

```sh
# Daemon
cd daemon && cargo build --release
systemctl --user enable --now uzakel-daemon

# Android
cd android && ./gradlew assembleDebug
```

## Lisans

GPL-3.0-or-later (BacakOS'un geri kalanıyla aynı).
