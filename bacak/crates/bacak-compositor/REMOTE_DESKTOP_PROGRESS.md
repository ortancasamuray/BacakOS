# Uzak Masaüstü — Oturum Durumu (2026-09-13)

Bu dosya, "Uzak Masaüstü" (`bacak-remote` + compositor plugin) özelliğinin
o anki geliştirme oturumundaki durumunu kaydeder — oturum kesilirse
kaldığı yerden devam edebilmek için. Kalıcı mimari belgeleri değil,
bunlar sırasıyla `ARCHITECTURE.md` §3 ve `uzakel/uzakel-pc/README.md`'de.

## Genel özet

Önceki oturumdan (2026-09-11) devralınan **dokunma kalibrasyonu bug'ı
ÇÖZÜLDÜ** — kök neden dokunma koordinatı hesabında değil, **video karesinin
render edilme biçimindeydi**. Ayrıca bu oturumda: Windows istemcisi
(`bacak-remote-client`) artık tam ekran açılıyor, eşleşme PIN'i BacakOS
tarafında üretilip ekranda gösteriliyor, ve `bacak-remote-server`'a
(Windows) grafik bir eşleşme penceresi eklendi.

## ÇÖZÜLDÜ: dokunma kalibrasyonu bug'ı

**Kök neden**: `render_remote_desktop_panel`'de video karesi
`MemoryRenderBufferRenderElement::from_buffer(..., src: None, size:
Some(dst), ...)` ile çiziliyordu — `dst` (hedef boyut, panel'in tüm
ekranı) veriliyor ama `src` (kaynak dikdörtgen) verilmiyordu. Smithay'ın
`from_buffer`'ı, `src` verilmediğinde onu buffer'ın **native boyutundan
değil, `dst`'nin kendisinden** türetiyor (bkz. `memory.rs`'deki
`from_buffer`'ın `size`/`src` öncelik sırası). Sonuç: uzak ekranın native
çözünürlüğü (bu oturumda ölçülen: **1400×1050**) BacakOS çıktısından
(1920×1080) küçük olduğunda, GL örnekleyici doku sınırlarının **dışına**
taşıyordu — gerçek görüntü sol-üstte native boyutta sabit kalıyor,
`video_rect`'in geri kalanı (sağ ~%27, alt ~%3) kenar pikselinin
tekrarıyla (clamp-to-edge) dolduruluyordu. Bu, sağ kenara yakın
dokunuşların ekranda görünenle **alakasız bir noktaya** gitmesinin tam
nedeniydi — dokunma koordinatı matematiği (`norm_in_rect`,
`udev_runtime.rs`'deki touch→logical dönüşümü) baştan beri doğruydu;
teşhis logları da bunu (raw_permille ≈ logical/output_size oranı) net
gösterdi.

**Düzeltme** (`remote_desktop.rs`, `render_remote_desktop_panel`): `src`
artık native `(w, h)` ile açıkça veriliyor, `dst` ekran boyutunda
kalıyor — GL artık native görüntüyü gerçekten gererek tüm `video_rect`'e
sığdırıyor. Gerçek donanımda doğrulandı: video artık tüm ekranı düzgün
kaplıyor, sağ kenara yakın dokunuşlar Windows'ta doğru noktaya gidiyor.

Bu vesileyle **Sunshine**'ın (`/home/os2/bacakos/Sunshine`, kullanıcı
tarafından referans için eklendi) `src/input.cpp`'deki `touch_port_t`
yaklaşımına bakıldı — Sunshine mutlak fare konumunu bir sanal HID
sürücüsüne referans genişlik/yükseklikle birlikte gönderiyor, bu da bize
ilham vermedi ama kaynağı okurken kendi Smithay kullanımımızdaki asıl
hatayı fark etmemizi sağladı. Sunshine'dan doğrudan alınan bir kod yok.

## PIN akışı: artık BacakOS üretiyor, Windows'ta giriliyor

Önceki tasarımda PIN `bacak-remote-server`'da (Windows) üretilip konsola
basılıyor, BacakOS tarafı `~/.config/bacak-remote/pair.json`'dan
(`{"ip", "pin"}`) okuyordu — pratikte BacakOS'ta PIN'i dosyaya elle
yazmak gerekiyordu. Artık:

- **BacakOS** paneli her açıldığında `bacak_remote_proto::crypto::generate_pin()`
  ile yeni bir PIN üretiyor ve ekranın üst ortasında büyük punto ile
  gösteriyor (bağlanana kadar kalıyor). `pair.json` artık sadece
  `{"ip": "...", "video_port"?, "input_port"?}` taşıyor — `pin` alanı
  kaldırıldı.
- **`bacak-remote-server`** artık kendi PIN'ini üretmiyor; operatör
  BacakOS ekranındaki PIN'i buraya giriyor, sunucu gelen `PairRequest`'i
  bu değere karşı doğruluyor.

**Önemli operasyonel not**: `bacak-remote-server`'ın PIN kutusu **tek
seferlik** — bir PIN gönderildikten sonra kutu kilitleniyor. Compositor
(veya display manager) yeniden başlarsa panel yeni bir PIN üretir; eski
PIN'i zaten göndermiş olan Windows penceresi artık eşleşmeyen bir PIN'e
kilitli kalır → "sunucu PIN'i reddetti" hatası. Çözüm: Windows'taki
pencereyi kapatıp yeniden başlatmak (yeni PIN promptu için).

## Grafik eşleşme penceresi (Windows, `bacak-remote-server`)

`native-windows-gui` + `native-windows-derive` ile küçük bir Win32
penceresi eklendi (`gui.rs`, Windows-only — `#[cfg(windows)]`): PIN giriş
kutusu + "Eşleştir" düğmesi + canlı durum satırı ("PIN gönderildi…",
"eşleşme bekleniyor…", "✓ Eşleşti — X (ip) bağlandı", "yanlış PIN
denemesi"). Ağ/yakalama işi kendi tokio runtime'ıyla ayrı bir OS
thread'inde çalışıyor; pencereyle sadece iki `std::sync::mpsc` kanalı
üzerinden konuşuyor (PIN gönderimi tek seferlik; durum bildirimleri
`nwg::Notice`/`NoticeSender` ile anlık uyandırma — polling timer değil).
`--pin <PIN>` veya `--no-gui` verilirse eski konsol prompt'una düşülüyor
(betikli/otomatik başlatmalar, ya da Windows dışı derlemeler için).

Gerçek donanımda doğrulandı: pencere SSH üzerinden başlatılırsa
**"Services" (Session 0, etkileşimsiz) oturumunda** çalışıp görünmez/
etkileşilemez hale geliyor — Windows'un kendi konsolundan başlatılmalı
(zaten bilinen bir kısıt, bkz. aşağıdaki "Test ortamı notları").

## Windows istemcisi artık tam ekran (`bacak-remote-client`)

Düşük çözünürlüklü bir Windows istemcisi, yüksek çözünürlüklü bir
BacakOS sunucusuna bağlandığında (ters yön — Windows izliyor, BacakOS
paylaşıyor) video tüm ekranı kaplamıyordu. `main.rs`'te pencere artık
`with_fullscreen(Some(Fullscreen::Borderless(None)))` ile açılıyor;
`render.rs`'deki fullscreen-triangle shader zaten geleni pencere
boyutuna göre geriyordu, eksik olan sadece pencerenin kendisinin tam
ekran olmasıydı.

**Not**: Bu iki yön (BacakOS→Windows izleme vs. Windows→BacakOS izleme)
aynı `bacak-remote-proto`/`bacak-remote-server` çiftini kullanıyor ama
hangi tarafın "istemci" hangi tarafın "sunucu" olduğu senaryoya göre
değişiyor — kafa karıştırabilir, yeni çalışan biri için netleştirilmeli.

## Teşhis logları temizlendi

Önceki oturumdan kalan geçici `RDDEBUG:` etiketli `tracing::info!/warn!`
satırları (`udev_runtime.rs`'nin `TouchDown` kolu, `remote_desktop.rs`'nin
kare-decode logu, `input_inject.rs`'nin `capture=`/`move_absolute=`
logları) bug çözüldükten sonra tamamen kaldırıldı. `remote_desktop.rs`'te
GPU'ya yükleme başarısız olursa (`from_buffer` `Err` dönerse) artık kalıcı
bir `tracing::warn!` var (RDDEBUG etiketsiz, gerçek bir hata durumu için).

## Bu oturumda ayrıca öğrenilen operasyonel sorunlar

- **PIN kutusu tek seferlik**: yukarıda "PIN akışı" bölümünde anlatıldığı
  gibi, compositor yeniden başladığında Windows tarafını da yeniden
  başlatmak gerekiyor.
- Önceki oturumlardan kalan genel notlar (`.deb` paketleme çakışması,
  `bacak-display-manager.service` restart'ının stale compositor'ı
  öldürmemesi, `pkill -9 -f` self-match riski, `RUST_LOG`/`journalctl
  _PID=` kullanımı) hâlâ geçerli — ayrıntı için git geçmişindeki önceki
  sürümüne bakılabilir.

## Geri alma planı

- Git etiketi: `pre-remote-desktop-ui-20260909-211713`
- Bilinen-çalışan binary yedekleri: her deploy'da otomatik
  `/usr/bin/bacak-compositor.bak-<timestamp>` oluşturuluyor (deploy
  script'i doğrudan binary kopyalıyor, `.deb` İLE KURMAYI DENEMEYİN —
  sistemdeki eski `bacak-compositor` paketiyle çakışıp sessizce geri
  alıyor).
- Geri alma: `sudo cp /usr/bin/bacak-compositor.bak-<en-son-iyi-olan> /usr/bin/bacak-compositor`
  sonra stale compositor'ı öldürüp `sudo systemctl restart
  bacak-display-manager.service`.

## Test ortamı notları

- Windows sunucusu: `192.168.1.55`, `os6` kullanıcısı, `sshpass -e ssh
  os6@192.168.1.55` ile erişiliyor (parola konuşmada geçti, kalıcı
  saklanmadı — her yeni oturumda kullanıcıdan tekrar istenmesi gerekir).
- `bacak-remote-server.exe` **Windows'un kendi konsolundan** başlatılmalı
  (SSH'tan başlatılırsa Session 0/"Services"e düşüyor, hem `SendInput`
  hem grafik pencere orada işe yaramıyor).
- Cross-compile: `rustup target x86_64-pc-windows-gnu` kurulu;
  `cd uzakel/uzakel-pc && cargo build --release -p bacak-remote-server
  --target x86_64-pc-windows-gnu` (ya da `bacak-remote-client` için aynı
  hedef) ile Windows exe'si üretiliyor, `sshpass -e scp ...
  "os6@192.168.1.55:C:\\Users\\os6\\Desktop\\<exe>"` ile kopyalanıyor
  (önce `taskkill /IM <exe> /F` ile eskisi kapatılmalı).
- Henüz yapılmayan: Faz 3 (bu grafik pencerenin ötesinde tam bir
  Windows tepsi uygulaması), ekran içi IP giriş formu (compositor
  tarafında — PIN artık gerekmiyor ama IP hâlâ `pair.json`'dan elle
  okunuyor), ve gerçek çok-parmak (multi-touch) enjeksiyonu (bkz.
  `input_inject.rs`'nin modül dokümanındaki bilinen sınırlama).
