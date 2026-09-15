# uzakel-mac

"Uzak Masaüstü" özelliğinin macOS'a özgü parçaları. **Gerçek uzak-masaüstü
sunucusu burada tekrarlanmıyor** — o
[`../uzakel-windows/bacak-remote-server`](../uzakel-windows/bacak-remote-server)'da
yaşıyor ve kendi yorum satırlarına göre zaten OS'a bağımsız yazılmış:

- `capture.rs` — `scrap` (Windows'ta DXGI, **macOS'ta CoreGraphics/Quartz**,
  Linux'ta X11)
- `input_inject.rs` — `enigo` (Windows'ta Win32 `SendInput`, **macOS'ta
  CGEvent**, Linux'ta uinput)
- `encode.rs`, `network.rs` — düz Rust/tokio, hiç OS'a özgü kod yok

Bu crate'te sadece iki şey Windows'a özgü (`#[cfg(windows)]`): grafik
eşleştirme penceresi (`gui.rs`) ve donanım H.264 kodlama (`encode_h264.rs`,
`ffmpeg-next` + Intel/NVIDIA/AMD vendor SDK'ları gerektiriyor, Mac'te zaten
anlamsız — bkz. `../uzakel-windows/HARDWARE_ENCODE_PLAN.md`). Bunlar
olmadan crate platform-bağımsız varsayılana düşüyor: konsol `--pin` yolu,
`RawZstd` encode.

**Güncelleme, 2026-09-15 — gerçek Apple Silicon donanımında (M4 MacBook
Air) doğrulandı.** Bu varsayılan orada derlenip çalışıyor, ve tam bir
oturum — eşleşme, video (BacakOS, Mac'in gerçek ekranını çözüyor) ve
girdi (BacakOS taraflı imleç hareketi Mac'in gerçek imlecine ulaşıyor) —
uçtan uca doğrulandı. Bunu doğrularken paylaşılan crate'te/onun
vendor'lanmış `scrap` fork'unda iki gerçek bug bulundu ve düzeltildi
(`#[cfg(windows)]` ile korunmuyorlardı, yani herhangi bir macOS derlemesi
bunlara çarpardı): `enigo`'nun macOS backend'i `Send` değil (girdi
task'ını `tokio::spawn` etmeyi kırıyordu), ve `scrap`'in quartz yakalaması
yanlış satır aralığı (stride) türetiyordu (kaymış/çizgili görüntü
üretiyordu). Tam yazı için `../uzakel-windows/README.tr.md`'nin "Gerçek
macOS uçtan uca geçişi" bölümüne bakın — iki düzeltme de burada değil o
paylaşılan crate'te.

Bu dizin, o paylaşılan, platform-bağımsız crate'e ait olmayan macOS'a özgü
parçalar için var:

- `packaging/build_mac.sh` — paylaşılan sunucuyu `aarch64-apple-darwin`/
  `x86_64-apple-darwin` için çapraz derler ve minimal bir `.app` paketine
  koyar (iskelet/test edilmedi — script'in kendi başlığına bakın).
- `gui-launcher/` — Windows crate'indeki `gui.rs`'in macOS karşılığı olan
  native bir eşleştirme penceresi, *ayrı* küçük bir binary olarak
  (paylaşılan sunucu crate'ine `#[cfg(target_os = "macos")]` bir GUI
  modülü eklemek yerine — Windows GUI'si `main.rs`/`run_session` içine
  oldukça karmaşık şekilde örülmüş, bkz. `gui.rs`'in kendi modül yorumu,
  ve o örgüyü tekrarlamak çalışan Windows yolunu incelikli şekilde bozma
  riskine değmezdi). Bir PIN girildiğinde gerçek `bacak-remote-server
  --pin <n> --no-gui`'yi süreç olarak çalıştırıyor — Windows GUI'sinin
  `run_session` ile ilişkisiyle aynı, sadece süreç-içi değil süreç-dışı.
  **Gerçek Mac'te inşa edildi, derlendi ve çalıştırıldı, 2026-09-15** —
  aşağıya bak.

## `packaging/build_mac.sh` — gerçek Mac'te doğrulandı, 2026-09-15

Gerçek Mac'in *üzerinde* native çalıştırıldı (Linux'tan çapraz derleme
değil — bu ortamda osxcross yok, o yol hâlâ doğrulanmadı; gerçek bir
Mac'te doğrudan native derlemek zaten daha basit ve buna gerek yok).
Her iki `cargo build --release --target {aarch64,x86_64}-apple-darwin`,
`lipo -create` ile evrensel binary, ve `.app` paketi (doğru doldurulmuş
`Info.plist` dahil) hepsi çalıştı — `lipo -info` sonuçta hem `x86_64`
hem `arm64` dilimlerini doğruluyor.

**Ama `.app` çift tıklandığında gerçekten çalışmıyor** (`open 'Bacak
Remote Server.app'` ile simüle edildi: hiçbir süreç oluşmuyor). Kök
sebep (henüz düzeltilmedi): çift tıklama `bacak-remote-server`'ı **hiç
argümansız** başlatıyor, bu yüzden `main()` konsol `--pin` yoluna
düşüyor, `prompt_pin()` `stdin` okumaya çalışıyor — ama Finder/`open`
ile başlatılan bir uygulamanın okuyacağı bir `stdin`'i yok, bu yüzden
anında başarısız oluyor ve görünür hiçbir şey olmadan çıkıyor (bağlı bir
konsol da yok — Windows'un gerçek bir GUI'ye kavuşmadan önce
`gui.rs`'in modül yorumunda anlattığı sorunun aynısı). Bu tam olarak
`gui-launcher/`'ın doldurması gereken boşluk — o gerçek, linklenmiş,
çalışan bir program olana kadar, bu script'in ürettiği `.app` çift
tıklayarak kullanılamıyor, sadece bir terminalden `--pin <n> --no-gui`
ile başlatılırsa çalışıyor (paylaşılan crate'in yukarıdaki kendi
doğrulaması buna göre).

## `gui-launcher/` — gerçek Mac'te inşa edildi, derlendi, çalıştırıldı, 2026-09-15

`objc2`/`objc2-app-kit` 0.6/0.3, delegate nesnesi için (app delegate +
pencere delegate + üç düğme aksiyonu) `define_class!` — `objc2`'nin kendi
`hello_world_app.rs` örneğindeki desenle. Temiz derlendi (`cargo build
--release`, derleyicinin işaretlediği bir avuç gereksiz `unsafe` bloğunu
kaldırdıktan sonra sıfır uyarı) ve gerçek Mac'te `open` ile çalıştırıldı:

- Pencere gerçekten görünüyor (başlık, PIN alanı, "Eşleştir"/
  "Eşleşmeyi Bitir"/"Kapat" düğmeleri, durum etiketi) — Mac'in gerçek
  ekranındaki kişi tarafından görsel olarak doğrulandı (bu ortam SSH
  üzerinden ekran görüntüsü alamıyor, bu belgedeki her yerdeki aynı Ekran
  Kaydı/TCC hikayesi).
- PIN göndermek gerçek `bacak-remote-server --pin <n> --no-gui`'yi bir alt
  süreç olarak başlatıyor (`find_server_binary()` önce kendi yanına bakıyor,
  sonra test edilen geliştirme kopyası yoluna düşüyor) ve bu süreç
  gerçekten BacakOS ile eşleşip video/girdi akıtıyor — tam oturum bu
  launcher üzerinden uçtan uca doğrulandı, sadece çıplak bir terminal
  çağrısıyla değil.
- Yol boyunca gerçek bir tuzak (ama `gui-launcher`'ın hatası değil): daha
  önceki manuel testten kalan eski bir `bacak-remote-server` süreci hâlâ
  UDP portlarını tutuyordu, bu yüzden BacakOS'un `PairRequest`'i taze
  başlatılan yerine *o* eski sürece (eski PIN'iyle) çarpıyordu — reddedilme
  gibi görünüyordu, aslında bir port işgaliydi. Bunu tekrar test ederken
  akılda tut: her denemeden önce `pkill -f bacak-remote-server` (ya da
  tam yeniden başlatma), doğru görünen bir PIN reddedilirse.

Henüz yapılmayan: gerçek eşleşme durumu için alt sürecin çıktısını takip
etmek (bu dosyanın kendi modül yorumuna bak — henüz süreç sınırı boyunca
bir kanal yok, bu yüzden pencere sadece "PIN gönderildi…" gösteriyor,
"eşleşti"/"reddedildi" hiç değil).

## Finder'dan çift tıklama — uçtan uca doğrulandı, 2026-09-15

`build_mac.sh` artık **her iki** binary'yi tek bir `.app`'a paketliyor
(`CFBundleExecutable` launcher; gerçek sunucu tam yanında,
`Contents/MacOS/` içinde — `find_server_binary()`'nin "kendi yanıma bak"
kontrolü onu orada buluyor). `open` ile başlatıldı (gerçek bir Finder
çift tıklamasını simüle ederek): pencere göründü, PIN göndermek
*paketlenmiş* sunucuyu başlattı (süreç yolunun `.app` içine işaret
etmesiyle doğrulandı, geliştirme kopyasına değil) ve BacakOS ile eşleşip
gerçek video/girdi akıttı.

**Bulunan ve düzeltilen gerçek, apaçık olmayan bir blokaj: paket
imzalanmış olmalı — sadece ad-hoc bile olsa (`codesign --sign -`, gerçek
bir Developer ID gerekmiyor) — Ekran Kaydı (TCC) izninin hiç çalışması
için.** İmzasız bırakıldığında, Sistem Ayarları'nda "Bacak Remote
Server"a Ekran Kaydı izni vermek hiçbir işe yaramadı — asıl yakalayan kod
*alt süreçte* (`bacak-remote-server`) çalışıyor, paketin ana yürütülebilir
dosyası olan (önceden imzasız) `gui-launcher`'da değil — TCC'nin ebeveynden
çocuğa sorumlu-süreç aktarımı, ebeveynin gerçek bir kod kimliğine sahip
olmasını istiyor gibi görünüyor, tamamen imzasız bir paketin bu kimliği
yok. `build_mac.sh` artık son adım olarak bitmiş pakette `codesign --sign
- --deep --force` çalıştırıyor; bundan sonra (ve yeniden imzalama kimliği
değiştirdiği için Ekran Kaydı iznini bir kez daha vererek) hemen çalıştı.

Şimdiye kadar sadece kozmetik, henüz kök nedeni bulunmamış bir gözlem:
taze bir eşleşmeden sonraki ilk video karesi görünmesi kabaca 10 saniye
sürdü (oturumun kalanı sorunsuzdu) — muhtemelen `CGDisplayStream`'in
kendi ilk-kare başlangıç gecikmesi (o API'nin bilinen gerçek bir
özelliği, bu kod tabanının kontrol ettiği bir şey değil) bir bug değil,
ama sadece bir kez test edildi; tutarlı mı yoksa tek seferlik mi olduğu
beklenen davranış olarak kabul edilmeden önce izlenmeye değer.

## Bilinen eksik

- Alt sürecin `stdout`'unu gerçek eşleşme durumu için takip etmek
  (yukarıya bak).
- *Gerçek* kod imzalama (asıl bir Developer ID) ve notarization —
  yukarıdaki ad-hoc imza TCC'yi yerel olarak düzeltiyor, ama
  internetten indirilen ad-hoc imzalı bir `.app` hâlâ Gatekeeper
  tarafından engelleniyor (bkz. `build_mac.sh`'ın kendi TODO'ları).
- Yukarıda not edilen ~10sn'lik ilk kare gecikmesi — tek bir veri noktası,
  henüz tekrarlanabilir olduğu doğrulanmadı ya da kök nedeni bulunmadı.
