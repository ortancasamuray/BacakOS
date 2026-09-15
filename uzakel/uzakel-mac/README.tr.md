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

## Bilinen eksik — bu dizinin kendi parçaları hâlâ doğrulanmadı

Paylaşılan sunucu crate'i doğrulandı (yukarıya bak). Doğrulanmayan:

- `packaging/build_mac.sh` *Linux'tan* çapraz derliyor (`osxcross`) —
  yukarıdaki doğrulamada gerçek Mac'te çalıştırılan native `cargo build`
  ile farklı bir yol. Bu ortamda osxcross araç zinciri yok, script'in
  kendisi hâlâ hiç çalışmadı. Gerçek bir Mac varsa (yukarıdaki doğrulamada
  olduğu gibi), doğrudan onun üzerinde native derlemek (çapraz derleme
  yok, osxcross yok) daha basit ve çalıştığı zaten biliniyor — script'e
  sadece özellikle Linux/CI'dan derlemek hedefse başvur.
- `gui-launcher/`, `cargo check --target aarch64-apple-darwin` /
  `--target x86_64-apple-darwin` ile temiz geçiyor (`objc2`/
  `objc2-app-kit` saf Rust bağlayıcıları — `check` için C derleyici ya da
  Apple framework'leri gerekmiyor, sadece hedefin `std`'si). Bu, kodun
  *şeklinin* tip kontrolünden geçtiğine dair gerçek bir sinyal, ama
  `main()` düz bir `todo!()` — hiçbir şey linklenmedi (gerçek
  framework'ler gerekir) ya da çalıştırılmadı (gerçek bir Mac gerekir),
  yani hâlâ başlangıç taslağı, çalışan kod değil.
- Ekran Kaydı (TCC) izninin "başlatma bağlamları arasında taşınmama"
  davranışı (bağlantılı yazıya bak) `build_mac.sh`'ın kendi TODO'sunu
  gerçek ve çözülmemiş bırakıyor: bir kullanıcının indirip çift tıkladığı
  bir `.app`, şimdiye kadar test edilen SSH ya da Terminal.app
  bağlamlarından apaçık farklı, üçüncü bir başlatma bağlamı — `.app`
  paketleme gerçek olduğunda özellikle yeniden doğrulanmaya değer.
