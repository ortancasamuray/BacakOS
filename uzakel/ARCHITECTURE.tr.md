# Uzakel — Mimari Özeti

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Bu, [ARCHITECTURE.md](ARCHITECTURE.md) dosyasının kısa Türkçe özetidir.

> **Durum: gerçek donanımda uçtan uca doğrulandı, eşleştirme zorunlu,
> trafik şifreli.** Gerçek bir telefon ve gerçek bir daemon birbirine
> karşı çalıştırıldı — keşfetti, PIN ile eşleşti, gerçek imleci hareket
> ettirdi, gerçek bir dosya transfer etti; hepsi çekirdek düzeyinde
> yakalamayla doğrulandı, sadece uygulama loglarıyla değil (§6).
> Eşleştirme artık gerçek bir geçici X25519 ECDH değişimi ve PIN'e bağlı
> bir onay etiketi çalıştırıyor, ve bundan sonraki her girdi/dosya
> çerçevesi türetilen oturum anahtarları altında ChaCha20-Poly1305 ile
> şifreleniyor (§2.3) — yalnızca IP ile kapılanmıyor. Rust ve Kotlin
> türetimleri, birbirine bağlanmadan önce sabit bir bilinen-cevap test
> vektörüyle bayt bayt karşılaştırıldı (`daemon/examples/kat.rs`). Bu
> şemanın aktif bir saldırgana karşı tam olarak ne garanti edip
> etmediği için §2.3'e bakın.

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
`PAIR_REQUEST { pin, client_pubkey }` gönderir — `client_pubkey`, bu
eşleştirme denemesi için üretilmiş taze, tek kullanımlık bir X25519 açık
anahtarıdır. Daemon kendi geçici X25519 anahtar çiftini üretir, ECDH'yi
yapar ve `PAIR_RESPONSE { accepted, daemon_pubkey, confirm_tag }` ile
yanıtlar.

#### 2.3.1 Oturum anahtarı türetimi

Her iki taraf da ECDH paylaşılan sırrından aynı anahtar materyalini
HKDF-SHA256 ile türetir (`daemon/src/crypto.rs`, `android/.../crypto/
UzakelCrypto.kt` tarafından bayt bayt yansıtılır):

```
shared      = X25519(kendi geçici gizli anahtarım, karşı tarafın geçici açık anahtarı)
prk         = HKDF-Extract(salt = "uzakel-pairing-v1", ikm = shared)
transcript  = client_pubkey || daemon_pubkey
c2s_key     = HKDF-Expand(prk, info = "uzakel c2s" || transcript)
s2c_key     = HKDF-Expand(prk, info = "uzakel s2c" || transcript)
confirm_key = HKDF-Expand(prk, info = "uzakel confirm" || transcript || pin)
confirm_tag = HMAC-SHA256(confirm_key, transcript)
```

`c2s_key` istemci→daemon trafiğini, `s2c_key` daemon→istemci trafiğini
şifreler — yönlü anahtarlar, böylece bir yöndeki bozulmuş bir nonce
sayacı diğer yöne karşı tekrar oynatılamaz. `confirm_tag`, değişimi
gerçekten PIN'e bağlayan şeydir: daemon bunu `PAIR_RESPONSE`'a ekler, ve
Android istemcisi kendi türetiminden bağımsız olarak yeniden hesaplayıp
yanıta güvenmeden önce karşılaştırır. Bir uyuşmazlık (yanlış PIN, ya da
ECDH değişimini yakalayan ama PIN'i bilmeyen bir ortadaki adam), istemcinin
saldırgan kontrolündeki anahtarlar altında herhangi bir şeyi şifrelemek
yerine eşleştirmeyi tamamen reddetmesine yol açar.

Başarılı bir eşleştirmede daemon, `(c2s_key, s2c_key)`'i istemcinin IP'sine
göre anahtarlanmış bir [`TrustStore`](../daemon/src/trust.rs) oturumunda
saklar. O andan itibaren, **girdi ve dosya transferi soketlerindeki her
çerçeve `ENCRYPTED_FRAME { nonce: [u8; 12], ciphertext }` içine
sarmalanır** — gerçek iç çerçevenin bir ChaCha20-Poly1305 AEAD şifreli
metni, nonce olarak düz bir little-endian sayaç (12 bayta sıfırla
genişletilmiş) kullanılır. Hem `input_manager.rs` hem `file_server.rs` bu
sarmalayıcıyı zorunlu kılar — bir oturum var olduğunda **düz metin yedeği
yoktur**; şifresi çözülemeyen veya tekrar oynatılan (sayaç ≤ görülen en
yüksek) bir çerçeve, eşleşmemiş bir çerçevenin her zaman olduğu gibi
düşürülür. Gerçek donanımda doğrulandı: eşleşmeden önce gerçek bir
telefonun trackpad kaydırmaları daemon tarafında sıfır çekirdek girdi
olayı üretti; eşleştikten sonra aynı kaydırmalar gerçek `REL_X`/`REL_Y`
olayları üretti (§6) — şimdi düz metin yerine şifreli çerçeveler olarak
taşınıyor.

**Bu şemanın tam olarak ne garanti edip etmediği:** bu, geçici X25519 +
PIN'e bağlı bir onay etiketi, tam bir PAKE (Parola ile Doğrulanmış Anahtar
Değişimi) değil. LAN'daki pasif bir dinleyiciye karşı gizlilik gerçek —
trafik gerçekten şifreleniyor, yalnızca IP ile kapılanmıyor. Aktif
saldırgan direnci tamamen PIN'e bağlı: ECDH değişimini yakalayan *ve*
PIN'i zaten bilen (ör. omuz üstünden gören) bir saldırgan, hâlâ geçerli
görünen bir el sıkışmayı tamamlayabilir, çünkü PIN'in kendisi kaba kuvvete
karşı bir direnç taşımıyor (gerçek bir PAKE, örneğin SPAKE2, PIN'i anahtar
değişiminin kendisine katar, böylece PIN üzerinde çevrimdışı bir
tahmin-ve-dene saldırısını imkansız kılar; bu şema bunun yerine PIN'i
yalnızca zaten gerçekleşmiş bir anahtar değişimini doğrulamak için
kullanır). Oturum güveni de hâlâ **yalnızca bellekte** — daemon yeniden
başlayınca hayatta kalmaz, bu yüzden Android'in "Bağlan"ı (kayıtlı host'a
yeniden bağlanma) eski anahtarları yeniden kullanmak yerine her zaman tam
PIN + ECDH el sıkışmasını yeniden çalıştırıyor, çünkü daemon'ın eski
oturumu hâlâ hatırlayıp hatırlamadığını bilmenin bir yolu yok (ve daemon
yeniden başlatmaları arasında anahtarları yeniden kullanmak zaten
nonce-sayaç tekrarı riski taşırdı). Daha yetenekli bir aktif saldırgana
karşı dayanması gerekiyorsa, gerçek bir TLS/PSK ya da SPAKE2 yaklaşımı
(§5) hâlâ bir sonraki adım.

#### 2.3.2 QR ile eşleştirme

Manuel giriş (telefonda elle yazılan host IP + 6 haneli PIN) hâlâ temel
yol — QR onun üzerine eklenmiş bir alternatif, farklı bir protokol değil.
Daemon, her taze PIN ürettiğinde — başlangıçta ve her başarılı
eşleştirmeden sonra, masaüstü bildirimiyle aynı tetikleyicide —
`~/.cache/uzakel/pairing.json`'a (`daemon/src/pairing_state.rs`) mevcut
eşleştirme durumunu yazar. Dosyadaki LAN adresi, keşif soketinin kendi
bağlı adresinden (`0.0.0.0`, telefona hiçbir işe yaramaz) değil,
yan etkisiz bir `UdpSocket::connect` rota sorgusundan (hiçbir zaman paket
göndermez) gelir.

`bacak-compositor`'ın Control Center'ında "Uzakel'e Bağlan" adlı bir çip
var (`bacak/crates/bacak-compositor/src/plugins/uzakel.rs`, diğer her
opsiyonel Control Center bölümü gibi
`/usr/share/bacak/plugins/uzakel.plugin` ile kapılı) — bu dosyayı her
açılışta taze okur ve bir QR koduna dönüştürür:

```
uzakel://pair?host=<ip>&port=<discovery_port>&pin=<pin>&name=<url-encode edilmiş daemon adı>
```

Android uygulamasının kamera ekranı (`ui/QrScanScreen.kt`, bir `CameraX`
`ImageAnalysis` çerçevesini ZXing ile çözer — Google'ın ML Kit'i değil,
özellikle bu LAN-yalnızca uygulamaya bir Google Play Services çalışma
zamanı bağımlılığı eklememek için) bunu `network/PairingUri.kt` ile
ayrıştırır ve elle yazılan bir PIN'in çalıştıracağı *tam olarak aynı*
`NetworkClient.pair()` çağrısını çalıştırır — QR yalnızca PIN ve IP'yi
elle yazmayı atlatır, kendisi hiçbir kriptografik materyal taşımaz. ECDH
değişimi, onay-etiketi kontrolü ve §2.3.1'deki her şey her iki yolda da
aynen gerçekleşir.

Dosya, daemon tarafında bir sunucusu olmayan düz bir `.cache` tarzı taslak
dosya olduğundan (neden olduğu için `pairing_state.rs`'in modül
doc-comment'ine bakın: daemon ile compositor'ın hangi sırayla
başlayacağına dair bir garanti yok, bu yüzden hiçbir şeyin bunu daemon'a
eşzamanlı olarak sormasına gerek olmamalı), bu doğası gereği *yerel* bir
mekanizma — onu okumak, daemon'ın çalıştığı aynı BacakOS oturumuna giriş
yapmış olmayı gerektirir, kabul edilebilir bir güven sınırı (o dosyayı
okuyabilen biri zaten eşleştirilecek masaüstünü kontrol ediyordur).

## 3. Daemon (Rust) — modül tasarımı

```
daemon/src/
├── main.rs            # ortam değişkeni tabanlı yapılandırma, üç soketi bağlar, /dev/uinput'u açar, üç görevi başlatır
├── discovery.rs        # UDP yayın yanıtlayıcısı; DISCOVER_REQUEST'i yanıtlar + ECDH eşleştirme el sıkışmasını (§2.3) çalıştırır
├── crypto.rs            # X25519 ECDH + HKDF-SHA256 anahtar türetimi + ChaCha20-Poly1305 seal/open (§2.3.1)
├── pairing_state.rs      # ~/.cache/uzakel/pairing.json'ı yazar (mevcut PIN + LAN adresi), QR eşleştirmesi için (§2.3.2)
├── trust.rs             # TrustStore — IP başına oturum (türetilmiş anahtarlar + AEAD durumu), input_manager + file_server'ın kontrol ettiği
├── protocol.rs          # paket başlığı/opcode tanımları + encode/decode, her modülün paylaştığı
├── input_manager.rs     # UDP soketi → TrustStore oturumunu arar → şifreyi çözer → girdi opcode'larını ayrıştırır → /dev/uinput üzerinden yeniden oynatır
└── file_server.rs       # TCP dinleyici → TrustStore oturumunu arar → çerçeveleri şifreler/çözer → parçalı alım, SHA-256 doğrulama, ~/İndirilenler'e yazar
```

`crypto.rs`, §2.3.1'deki ECDH/HKDF/AEAD ilkelleridir: el sıkışma için
`EphemeralKeypair::generate()`/`derive()`, ve monoton bir sayaç-nonce'la
ChaCha20-Poly1305'i sarmalayıp tekrar oynatmayı reddeden `Cipher`/`Opener`.
Bir taraf diğerinin baytlarına güvenmeden önce, paralel bir Kotlin/
BouncyCastle uygulamasına karşı sabit bir bilinen-cevap test vektörüyle
(`daemon/examples/kat.rs`) bağımsız olarak bayt bayt karşılaştırıldı.

`trust.rs`, tek bir `TrustStore`'dur (küçük bir API'nin arkasındaki bir
`Arc<Mutex<HashMap<IpAddr, Session>>>`), `main.rs`'te bir kez oluşturulur
ve her üç göreve klonlanır. `discovery.rs` tek yazandır (onay etiketi
doğrulanmış bir eşleştirmeden sonra `trust()`, türetilmiş `Cipher`/
`Opener` çiftini saklayarak); `input_manager.rs` ve `file_server.rs`
yalnızca okuyan çağrıcılardır (`session()`) ve saklanan anahtarları her
çerçevenin şifresini çözmek/şifrelemek için kullanır. Hâlâ IP ile
anahtarlanmış ve yalnızca bellekte — tam olarak ne garanti edip
etmediği için kendi doc-comment'ine ve §2.3.1'e bakın.

`input_manager.rs`, `input-linux` crate'iyle `/dev/uinput` üzerinden
oluşturulmuş sanal bir fare ve klavye aygıtını sahiplenir (geçerli her
evdev tuş kodu, üç fare düğmesi, X/Y/tekerlek göreli eksenleri baştan
kaydedilir). Ham deltalara enjekte etmeden önce hafif bir ivme eğrisi
uygular — istemcinin zaten uyguladığı eğrinin üzerine, ham delta
gönderen bir istemci için güvenlik ağı olarak — ama imleç konumunu
kendisi sıkıştırmaz: her olay `REL_X`/`REL_Y` (göreli)'dir, bu yüzden
ekran-kenarı sıkıştırması tamamen compositor'ın işidir, gerçek bir fare
için olduğu gibi. Bir `tokio` görevinde UDP soketini sıkı bir döngüde
okur, `TrustStore`'un bir oturumu olmayan bir adresten gelen her paketi ve
o oturumun anahtarı altında şifresi çözülüp doğrulanamayan her
`ENCRYPTED_FRAME`'i (§2.3.1) düşürür, ve kaynak-adresi başına son görülen
`seq`'i (şifresi çözülmüş iç paketten okunan) takip eder, böylece sırası
bozuk veya yinelenen bir paket imleci geri hareket ettirmek yerine
düşürülür.

`file_server.rs`, düz bir `tokio` TCP dinleyicisidir; `TrustStore`'da
oturumu olmayan bir adresten gelen bağlantı, el sıkışma başlamadan anında
`FILE_REJECT` alır ve düşürülür — bu ilk ret, bu kanalda hâlâ düz metin
gönderilen tek çerçevedir, çünkü henüz onu şifrelemek için bir oturum
anahtarı yoktur. Güvenilir bir bağlantı, §2.2'deki durum makinesinin alım
tarafını her iki yönde de `ENCRYPTED_FRAME`ile sarmalanmış trafik üzerinde
çalıştıran kendi görevini alır, çakışmasız bir hedef dosya adı seçer (`ad`, sonra
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

## 4. Android istemcisi (Kotlin + Jetpack Compose) — modül tasarımı

```
android/app/src/main/kotlin/org/anadolupanteri/uzakel/
├── protocol/      # Protocol.kt — daemon/src/protocol.rs'in bayt-bayt Kotlin karşılığı
├── crypto/        # UzakelCrypto.kt — X25519/HKDF/ChaCha20-Poly1305, daemon/src/crypto.rs'in Kotlin ikizi (§2.3.1)
├── discovery/     # SavedHostsStore: SharedPreferences tabanlı eşleştirilmiş host listesi
├── network/       # NetworkClient + PairingUri.kt: keşif taraması, ECDH+PIN eşleştirme (manuel ya da QR, §2.3.2), şifreli InputChannel (UDP), şifreli sendFile (TCP)
├── input/         # TrackpadView (çok dokunuşlu), KeyCodes (evdev tuş kodu tablosu), IME→typeChar köprüsü
├── transfer/      # FileTransferManager: SAF dosya seçimi, SHA-256, StateFlow ile ilerleme
├── ui/            # UzakelApp (kök), DeviceListScreen, QrScanScreen, ControlScreen, TransferScreen
└── MainActivity.kt
```

Navigasyon kütüphanesi yok — `UzakelApp`, küçük bir `sealed class Screen`
ve bir `when` ile üç ekran arasında geçiş yapar; bu kadar sığ bir grafik
(cihaz listesi → kontrol → transfer, ve geri) için Navigation-Compose
eklemekten daha basit.

`protocol/Protocol.kt`, daemon ile gerçek etkileşim sözleşmesidir —
içindeki her `ByteBuffer` yerleşimi (`Header`, `InputPacket.encode()`,
`FileMeta.encode()`, `encodeChunk`, `DiscoverResponse.decodePayload`, …)
`daemon/src/protocol.rs` ile bayt-bayt eşleşmek zorundadır, çünkü bu iki
dosya hiç kod paylaşmaz, yalnızca bir kablo formatı paylaşır.
`NetworkClient`, her UDP okumasını tampon'un kapasitesine değil,
datagram'ın gerçek `packet.length`'ine sınırlar — tampon `receive()`
çağrıları arasında yeniden kullanıldığından, aksi halde daha uzun bir
paketten sonra gelen kısa bir paket eski baytları okurdu.

`input/TrackpadView`, `pointerInput`/`awaitEachGesture` tabanlı bir Compose
yüzeyidir (yalnızca tek parmağı takip eden `detectDragGestures` değil) —
tek parmak sürükleme = `MOUSE_MOVE`, tek parmak dokunuş = sol tık, iki
parmak dokunuş = sağ tık, iki parmak sürükleme = `MOUSE_SCROLL`. Deltalar
aktif parmaklar arasında ortalanır ve paket oluşturulmadan önce istemci
tarafında bir hassasiyet katsayısıyla ölçeklenir.

**Klavye girdisi için özel bir ekran klavyesi yok.** `ControlScreen`, sıfır
yükseklikte, isteğe bağlı odaklanan ve tek bir sıfır-genişlikli yer
tutucu karakterle beslenen bir `BasicTextField` tutar; sistem IME'sinin bu
alana yaptığı düzenlemeler (`input/Typing.kt`'nin `typeChar`'ı) 
`input/KeyCodes.kt`'nin evdev tuş kodu tablosu üzerinden `KEY_PRESS`
paketlerine çevrilir, ve *küçülen* bir değer (yer tutucu normal yazımla
kısalamaz) bir `KEY_BACKSPACE` basışı olarak okunur. Bu, kullanıcının
zaten sahip olduğu klavyeyi yeniden kullanır — otomatik düzeltme, kaydırma
ile yazma, Latin olmayan düzenler dahil — uygulamanın kendi klavyesini
çizmesi yerine. Ctrl/Alt/Super geçiş çipleri artı Esc/Tab/ok
tuşları/Enter/Backspace butonlarından oluşan bir satır, yumuşak bir
IME'nin üretemediklerini kapsar; daemon ham tuş yukarı/aşağı durumunu
doğrudan kernel'e yeniden oynattığından, IME köprüsüyle yazarken bir
değiştirici çipini basılı tutmak, iki kod yolu hiç doğrudan koordine
olmasa bile gerçek kombinasyonlar üretir (ör. Ctrl+C).

`crypto/UzakelCrypto.kt`, `daemon/src/crypto.rs`'in (§2.3.1) Kotlin
ikizidir — el sıkışma için `EphemeralKeypair.generate()`/`derive()`,
BouncyCastle'ın ChaCha20-Poly1305'ini aynı sayaç-nonce ve tekrar-oynatma
reddi şemasıyla sarmalayan `Cipher`/`Opener`. `javax.crypto` yerine
BouncyCastle kullanılıyor, çünkü Android'in kendi X25519 desteği yalnızca
API 33+'ta geliyor, bu uygulamanın `minSdk`'si ise 26. `NetworkClient`'a
bağlanmadan önce Rust tarafına karşı sabit bir bilinen-cevap test
vektörüyle bayt bayt karşılaştırıldı.

`network/NetworkClient` — `discoverHosts()` `DISCOVER_REQUEST` yayınlar ve
sabit bir pencere boyunca yanıtları toplar; `pair()` geçici bir anahtar
çifti üretir, `PAIR_REQUEST { pin, client_pubkey }` gönderir, ve
`PAIR_RESPONSE` üzerinde daemon'ın anahtarlarına güvenmeden önce
`confirm_tag`'i yerel olarak yeniden hesaplayıp kontrol eder — bir düz
boolean yerine bir `PairedSession` (adres + her iki yönlü oturum anahtarı)
döndürür; bir onay etiketi uyuşmazlığı yanlış bir PIN ile aynı şekilde ele
alınır. `InputChannel`, her paketi göndermeden önce bir `Cipher` ile
şifreleyip `ENCRYPTED_FRAME` içine sarmalayan, ve §2.1'deki monoton `seq`
sayacını (şifreli payload'ın içinde taşınan) sahiplenen küçük bir
ateşle-unut UDP sarmalayıcısıdır; `sendFile()` §2.2'deki üç fazlı
yüklemeyi her iki yönde de `Cipher`/`Opener` ile sarmalanmış/çözülmüş
çerçevelerle çalıştırır ve trailer çerçevesi üzerinde okuma zaman aşımını
başarı olarak ele alır, çünkü daemon yalnızca `FILE_CORRUPT`'ta konuşur.

`ui/QrScanScreen` (§2.3.2), arka kameraya bağlanmış bir `CameraX`
`Preview` + `ImageAnalysis`'tir, çerçeve çerçeve ZXing'in
`MultiFormatReader`'ı ile doğrudan analiz çerçevesinin Y-düzlemi
üzerinden çözülür (bitmap dönüşümü yok — QR çözme yalnızca parlaklığa
ihtiyaç duyar). Kasıtlı olarak ZXing, Google'ın ML Kit'i değil: ML Kit'in
cihaz-üstü barkod tarayıcısı çalışma zamanında hâlâ Google Play
Services'e ihtiyaç duyuyor, ve bu uygulama başka hiçbir yerde LAN'ın
ötesinde bir şeye bağımlı değil. `network/PairingUri.kt`, çözülen
`uzakel://pair?host=...&port=...&pin=...&name=...` dizesini ayrıştırır;
`DeviceListScreen` daha sonra elle yazılan bir PIN'in çalıştıracağı *tam
olarak aynı* `NetworkClient.pair()` çağrısını çalıştırır — QR yalnızca
yazmayı atlatır, kendi başına hiçbir anahtar materyali taşımaz.

`transfer/FileTransferManager`, gönderilecek bir dosya seçmek için
Android'in Storage Access Framework'ünü kullanır, böylece asla geniş
depolama izinlerine ihtiyaç duymaz — BacakOS'un masaüstü tarafındaki dosya
işlemenin (`altay`'ın `security::Sandbox`'ı) izlediği "shell'e çıkma yok,
tasarım gereği sandbox'lı" içgüdüsüyle örtüşür. SHA-256, el sıkışmadan
önce SAF akışı üzerinde tam bir geçişte hesaplanır (protokol hash'i
baştan ister), bu yüzden büyük bir dosya iki kez okunur; gönderimle
birlikte artımlı bir digest bu maliyeti kaldırırdı — bkz. §5. Yalnızca
gönderme uygulandı, daemon'ın alım-yalnızca `file_server.rs`'iyle uyumlu.

`discovery/SavedHostsStore`, eşleştirilmiş host'ları (ad, son bilinen IP)
`SharedPreferences`'ta tek bir JSON dizisi olarak kalıcı hale getirir — bir
telefonun gerçekçi olarak eşleştiği birkaç host için fazlasıyla yeterli,
gerçek bir veritabanı verinin hak ettiğinden daha fazla makine gibi
hissettirdi. Adres, kaydedilmiş bir host için yalnızca bir *başlangıç
noktasıdır*, körü körüne güvenilmez: `DeviceListScreen`, bağlanırken onu
`InetAddress.getByName` ile yeniden çözer, çünkü LAN cihazları oturumlar
arasında sıkça adres değiştirir (DHCP kira döngüsü) — "kaydedilmiş bir
host'a bağlan" akışına henüz taze bir yayın yeniden taraması
bağlanmadı (§5).

## 5. Açık sorular / henüz karara bağlanmadı

- ~~Eşleştirme sonrası oturumlar için TLS materyali~~ — **tamamlandı**,
  geçici X25519 ECDH + PIN'e bağlı onay + ChaCha20-Poly1305 ile (§2.3.1),
  TLS/sertifika değil. Hâlâ açık: PIN kontrolünün kendisini gerçek bir
  PAKE'ye (ör. SPAKE2) yükseltmek, böylece tek başına bir PIN, aktif bir
  saldırganın zaten yakaladığı bir oturumu doğrulayamasın — bkz. §2.3.1'in
  uyarısı.
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
  değiştirmek, `libnotify-bin` çalışma zamanı bağımlılığını kaldırmak — ve
  daha önemlisi, eşleştirme PIN'ini bir bildirim servisine bağımlı olmadan
  *ekranda* göstermek. Gerçek donanım testi tam olarak buna denk geldi:
  test oturumunda çalışan bir bildirim servisi yoktu, bu yüzden PIN
  yalnızca daemon'ın kendi logunda görünüyordu — gerçek bir kullanıcı
  deneyimi eksikliği, sadece güzel olur değil.
- `DeviceListScreen`'in "Bağlan" (kayıtlı host) akışı, önce yeniden
  taramak yerine kalıcı IP'yi olduğu gibi kullanır; host'un adresi
  kaydedildiğinden beri değiştiyse, yeniden eşleşme denemesi (artık her
  seferinde gerekli, bkz. §2.3) taze bir keşif yayınına düşüp yeni adresi
  bulmak yerine "PIN yanlış veya cihaz yanıt vermedi" ile zaman aşımına
  uğrar — artık sessiz değil, ama bu özel neden için harika bir hata
  mesajı da değil.
- `FileTransferManager`, dosyayı iki kez okuyor (bir kez hash için, bir kez
  gönderim için) çünkü `FILE_META`'nın SHA-256'sı akış başlamadan önce
  bilinmek zorunda (§2.2) — artımlı hash'lenmiş bir gönderim (digest'i
  parçalar giderken hesapla, yalnızca son parçadan sonra bilinen bir
  değere karşı doğrula) doğrulamayı gönderenin hesapladığı bir trailer
  çerçevesine taşıyan bir protokol değişikliği gerektirirdi.
- Wi-Fi el değiştirmesinde veya daemon oturum ortasında yeniden
  başladığında Android'in yeniden bağlanma/tekrar deneme davranışı —
  `InputChannel` şu an gönderim hatalarını sessizce yutuyor (kendi
  doc-comment'ine bakın), yeniden bağlanma mantığı yok.
- Uzaktan görünürlük için özel bir imleç (dokunuşta büyüyüp küçülen
  yuvarlak) — bu bir `bacak-compositor` imleç-render özelliği,
  Uzakel'in kendisinin sağlayabileceği bir şey değil; kapsam dışı ama o
  proje ele alındığında oraya bağlanmaya değer.

---

## 6. Gerçek donanım testinin bulduğu şeyler

Her iki taraf da gerçek donanıma dokunmadan çok önce bağımsız olarak
derlendi, birim test edildi ve lint'ten geçti — bunların hiçbiri gerçek bir
telefon gerçek bir daemon'la ilk kez konuştuğunda asıl neyin bozulduğunu
yakalamadı. Sırayla bulunup düzeltilen üç gerçek bug:

1. **`DatagramSocket.connect()`, `ControlScreen` açılır açılmaz uygulamayı
   çökertiyordu**, `IllegalArgumentException: connect: -1` ile, gerçek bir
   Redmi Note 8'de (MIUI, Android 11) — her seferinde tekrarlanabilirdi.
   `InputChannel`'ın eşin "bağlı" olmasına ihtiyacı yoktu (her `send()`
   zaten hedefi kendi `DatagramPacket`'inde taşıyor), o yüzden düzeltme
   `connect()`'i hiç çağırmamaktı.
2. **Her tek girdi paketi sessizce başarısız oluyordu, %100 kayıp, hiç
   görünür belirti yok.** `channel.mouseMove()` vb. doğrudan Compose jest
   callback'lerinden çağrılıyor, bunlar ana thread'de çalışıyor — ama asıl
   `DatagramSocket.send()` syscall'ı, Android'in StrictMode
   `NetworkOnMainThreadException`'ının tam olarak engellemek için var
   olduğu şey. Soketin kendisi sorunsuz açılıyordu (`withContext(Dispatchers.
   IO)` ile oluşturulmuştu), o yüzden kurulumda hiçbir şey yanlış
   görünmüyordu; yalnızca sonraki her tekil gönderim sessizce fırlatıyor ve
   `InputChannel`'ın kasıtlı best-effort `catch (_: Exception) {}`'i
   tarafından yutuluyordu. Düzeltme: `InputChannel`'a kendi arka plan
   `CoroutineScope`'unu (`Dispatchers.IO`) vermek ve her `send()`'i
   çağıranın thread'inde değil orada çalıştırmak.
3. **`DeviceListScreen` zaten eşleştirilmiş bir host'u iki kez
   gösteriyordu** — bir taze keşif yanıtından ("Eşleştir"), bir de
   kaydedilmiş host listesinden ("Bağlan") — çünkü keşfedilenleri zaten
   kaydedilmiş listeye göre filtreleyen hiçbir şey yoktu.

Bug olmayan dördüncü bir bulgu bir ayar sorunuydu: varsayılan istemci-tarafı
hassasiyeti (`TrackpadView`'in `sensitivity = 1.5f`'i), daemon'ın kendi
ivme eğrisiyle (`input_manager.rs`'in `accelerate`'i) birleşince gerçek bir
trackpad'de çok hızlı hissettiriyordu — gerçek yakalanan `REL_X`/`REL_Y`
değerleri, mütevazı bir parmak sürüklemesi için 111'e ulaştı. `sensitivity
= 0.4f`'e düşürüldü.

Hiçbiri, çalışan uygulamayı doğrudan enstrümante etmeden mümkün olmazdı:
`/proc/net/udp`, bunun için **işe yaramaz** çıktı (Android 10+, başka bir
uygulamanın soketlerini `adb shell`'den bile gizliyor — araştırmayı kısa
süreliğine yanlış yöne yönlendiren bir yanlış negatif), geçici ekran üstü
sayaçlar (`moveCount`, `sendErrorCount`, `lastError`) ve telefon aktif
olarak sürerken `/dev/input/eventN`'den ham çekirdek olayları yakalamak ise
her bug'ı gerçekten sabitleyen şeydi.

### Eşleştirme zorunluluğu (`trust.rs`), aynı şekilde doğrulandı

Girdi yolu doğrulandıktan sonra, aynı gerçek telefon + gerçek daemon
kurulumu, eşleştirme zorunluluğunu uyguladıktan hemen sonra doğrulamak için
kullanıldı:

1. Daemon yeniden başlatıldı (taze, boş `TrustStore`) — telefonda yeniden
   başlatmadan önceki "bağlı" oturum hâlâ açıkken. Trackpad kaydırmaları:
   daemon tarafında **0 çekirdek girdi olayı** — artık güvenilmeyen
   adresten gelen trafik, tam olarak tasarlandığı gibi sessizce
   düşürülüyor.
2. Aynı telefon, kaydedilmiş host'ta `Bağlan` — artık doğrudan
   bağlanmak yerine her zaman PIN diyaloğunu yeniden açıyor (§2.3) — PIN
   girildi, `discovery.rs` `client paired successfully` kaydetti, hemen
   ardından trackpad kaydırmaları: **224 gerçek çekirdek olayı**.
3. Dosya transferi reddi bağımsız olarak kontrol edildi (hızlı olması için
   telefondan değil): `127.0.0.1`'den gönderilen bir `FILE_META` el
   sıkışması — gerçekten güvenilmiyordu, çünkü yalnızca telefonun LAN
   adresi eşleşmişti — `FILE_REJECT { "cihaz eşleştirilmemiş" }` aldı ve
   indirilenler dizinine hiçbir dosya yazılmadı.

Düzeltme, daemon değişikliğinin yanında küçük bir Android-tarafı UX
değişikliği gerektirdi: `DeviceListScreen`'in "Bağlan" butonu kaydedilmiş
bir host için eskiden doğrudan bağlanıyordu; artık güven daemon'da bellek
içi olduğundan ve yeniden başlatmada hayatta kalmadığından, her zaman PIN
diyaloğunu yeniden çalıştırıyor — doğrudan bağlanmak, yukarıdaki 1.
adımın kasıtlı olarak yeniden ürettiği tam olarak "her şey bağlı görünüyor
ama hiçbir şey hareket etmiyor" belirtisini sessizce üretirdi.
