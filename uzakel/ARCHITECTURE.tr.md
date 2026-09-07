# Uzakel — Mimari Özeti

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Bu, [ARCHITECTURE.md](ARCHITECTURE.md) dosyasının kısa Türkçe özetidir.

> **Durum: tasarım aşaması — burada hiçbir şey henüz uygulanmadı.** Bu belge,
> ilk uygulamanın izlemesi gereken hedef tasarımdır; aşağıdaki her
> "olacak"/"dır" ifadesini niyet olarak okuyun, var olan kodun tarifi
> olarak değil.

İki bileşen, üç ağ kanalı, tek bir paylaşılan kablo protokolü.

## 1. Sistem genel bakışı

Üç bağımsız kanal, her biri kendi trafiğine uygun:

| Kanal | Taşıma | Neden |
|---|---|---|
| Keşif | mDNS, olmazsa 5353 portunda UDP yayın | Sıfır-yapılandırma LAN eşleştirmesi; kaybolan bir yayın yalnızca sıradaki periyodik yayının başarılı olması demektir. |
| Girdi (fare/klavye) | UDP, özel port (varsayılan 9876) | Gecikme, güvenilirlikten daha önemli — düşen bir `MOUSE_MOVE` deltası fark edilmez; yeniden gönderilen bayat bir tanesi gecikmeli hissettirir. |
| Dosya transferi | TCP, özel port (varsayılan 9877) | Doğruluk, gecikmeden daha önemli — dosyalar bayt-bayt eksiksiz gelmeli. |

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
tutmak yerine kısmi dosyayı siler). Yön simetriktir — hangi taraf
gönderiyor olursa olsun aynı durum makinesi çalışır (Android → BacakOS
veya BacakOS → Android); yalnızca TCP bağlantısını kimin başlattığı
değişir.

### 2.3 Eşleştirme

Bir istemci ile daemon arasındaki ilk temas, herhangi bir taraf diğerinden
girdi veya dosya paketi kabul etmeden önce bir PIN el sıkışmasından geçer:
daemon kısa bir PIN gösterir (masaüstü bildirimi), istemci bunu keşif
yanıt kanalı üzerinden geri gönderir, ve daemon o istemcinin
sertifikasını/anahtarını gelecekteki TLS-sarmalı oturumlar için güvenilir
olarak işaretler. Bu, "LAN'daki herhangi bir telefon" ile "kullanıcının
gerçekten onayladığı bir telefon" arasındaki tek fark budur.

## 3. Daemon (Rust) — modül tasarımı

```
daemon/src/
├── main.rs            # argüman ayrıştırma, systemd notify, üç servisi birbirine bağlar
├── discovery.rs        # mDNS/UDP yayın yanıtlayıcısı; daemon sürümü + eşleştirme durumuyla yanıtlar
├── protocol.rs          # input_manager ve file_server'ın paylaştığı paket başlığı/opcode tanımları
├── input_manager.rs     # UDP soketi → girdi opcode'larını ayrıştırır → /dev/uinput üzerinden yeniden oynatır
└── file_server.rs       # TCP dinleyici → parçalı al/gönder, SHA-256, ~/İndirilenler'e yazar
```

`input_manager.rs`, `/dev/uinput` üzerinden oluşturulmuş sanal bir fare ve
klavye aygıtını sahiplenir (`input-linux` veya `evdev` crate'i ile). Ham
deltalara enjekte etmeden önce bir ivme eğrisi uygular ve sentezlenmiş
imleç hareketini compositor'ın bilinen ekran sınırlarına sıkıştırır. Soket
ile enjeksiyon arasında kuyruklama katmanı yoktur — eklenecek gecikme,
UDP kullanmanın amacını baştan yener. `file_server.rs`, düz bir `tokio` TCP
dinleyicisidir; kabul edilen her bağlantı, §2.2'deki üç aşamalı durum
makinesini çalıştıran kendi görevini alır ve başarılı doğrulamada bir
masaüstü bildirimi tetikler (`bacak-compositor`'ın `plugins/*`'ının zaten
kullandığı freedesktop bildirim veriyolu üzerinden). `discovery.rs`,
yayın/mDNS sorgularını daemon'ın sürümü ve mevcut eşleştirme durumuyla
yanıtlar ve §2.3'teki PIN el sıkışmasının üzerinde gezindiği kanaldır.
Daemon, kullanıcı oturumuyla başlaması ve asla root gerektirmemesi için bir
`systemd --user` servisi olarak çalışır — `/dev/uinput`'ı, BacakOS'un
oturum kurulumunun kullanıcıya verdiği `uinput` grup üyeliği üzerinden
açar, `bacak/packaging/bacak-session`'ın diğer kullanıcı-kapsamlı masaüstü
servisleri için kullandığı aynı desen.

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

- `/dev/uinput` erişimi için tam crate seçimi (`input-linux` mı yoksa elle
  yazılmış ioctl bağlamaları mı) — `udev`/DRM altında `bacak-compositor`
  çalıştıran gerçek bir BacakOS oturumuna karşı bir keşif gerektiriyor.
- Eşleştirme sonrası oturumlar için TLS materyali: eşleştirme anında
  sabitlenen kendinden imzalı sertifika (en basit, CA gerekmez) mı yoksa
  PIN değişimine bağlı daha hafif bir PSK şeması mı.
- `MOUSE_SCROLL`'un, gerçek trackpad kullanımı bir tercih ortaya çıkardığında
  `MOUSE_MOVE`'dan ayrı kendi ivme eğrisine ihtiyacı olup olmadığı.
- Çoklu istemci davranışı: iki telefon aynı daemon'ı aynı anda kontrol
  edebilir mi, yoksa yeni bir istemciyi eşleştirmek öncekini düşürür mü?
