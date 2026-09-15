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
- `gui-launcher/` — Windows crate'indeki `gui.rs`'in macOS karşılığı için
  **bir iskelet, çalışan bir uygulama değil**. Paylaşılan sunucu crate'ine
  `#[cfg(target_os = "macos")]` bir GUI modülü eklemek yerine *ayrı* küçük
  bir binary olarak tasarlandı (Windows GUI'si `main.rs`/`run_session`
  içine oldukça karmaşık şekilde örülmüş — bkz. `gui.rs`'in kendi modül
  yorumu — ve burada henüz kimsenin derleyip test edemediği bir platform
  için o örgüyü tekrarlamak, çalışan Windows yolunu incelikli şekilde
  bozma riskine değmez). Bir PIN girildiğinde gerçek
  `bacak-remote-server --pin <n> --no-gui`'yi çalıştırması/spawn etmesi
  planlanıyor — Windows GUI'sinin `run_session` ile ilişkisiyle aynı,
  sadece süreç-içi değil süreç-dışı.

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

## Bilinen eksik — bu dizinin kendi parçaları hâlâ doğrulanmadı

- `gui-launcher/`, `cargo check --target aarch64-apple-darwin` /
  `--target x86_64-apple-darwin` ile temiz geçiyor (`objc2`/
  `objc2-app-kit` saf Rust bağlayıcıları — `check` için C derleyici ya da
  Apple framework'leri gerekmiyor, sadece hedefin `std`'si). Bu, kodun
  *şeklinin* tip kontrolünden geçtiğine dair gerçek bir sinyal, ama
  `main()` düz bir `todo!()` — hiçbir şey linklenmedi (gerçek
  framework'ler gerekir) ya da çalıştırılmadı (gerçek bir Mac gerekir).
  Bunu inşa edip bağlamak artık çift-tıkla-çalışır bir `.app` için
  varsayımsal bir "olsa iyi olur" değil, somut bir blokaj (yukarıya bak).
- Ekran Kaydı (TCC) izninin "başlatma bağlamları arasında taşınmama"
  davranışı (bağlantılı yazıya bak) `build_mac.sh`'ın kendi TODO'sunu
  gerçek ve çözülmemiş bırakıyor: `open`/Finder ile başlatma, şimdiye
  kadar test edilen SSH ya da Terminal.app bağlamlarından apaçık farklı,
  üçüncü bir başlatma bağlamı — `gui-launcher` `.app`'ın ekranı yakalamayı
  gerçekten denemesine izin verdiğinde kontrol edilmeye değer.
