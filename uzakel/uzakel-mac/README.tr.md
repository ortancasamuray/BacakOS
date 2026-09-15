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
olmadan crate zaten platform-bağımsız varsayılana düşüyor: konsol `--pin`
yolu, `RawZstd` encode. Bu varsayılanın macOS'ta *olduğu gibi* derlenip
çalışması gerekir — **bu doğrulanmadı**, aşağıdaki "Bilinen eksik"e bakın.

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

## Bilinen eksik — burada hiçbir şey derlenmedi ya da çalıştırılmadı

Bu ortamda ne bir macOS makinesi ne de çalışan bir macOS çapraz araç
zinciri (osxcross) var. `rustup target add aarch64-apple-darwin` başarılı
oluyor (Rust bu hedef için derlenmiş `std`'yi zaten dağıtıyor), ama
`uzakel-windows`'tan `cargo check --target aarch64-apple-darwin -p
bacak-remote-server` bu crate'in kendi koduna ulaşmadan **önce** başarısız
oluyor — `zstd-sys`'in C kısmı gerçek bir Apple `cc` (`-arch`/
`-mmacosx-version-min` anlayan) istiyor, Linux makinenin `cc`'si anlamıyor.
Yani:

- Paylaşılan `bacak-remote-server` crate'inin macOS için gerçekten temiz
  derlenip derlenmediği **doğrulanmadı** — sadece GUI/paketleme kısmı
  değil.
- `gui-launcher/`, `cargo check --target aarch64-apple-darwin` /
  `--target x86_64-apple-darwin` ile temiz geçiyor (`objc2`/
  `objc2-app-kit` saf Rust bağlayıcıları — `check` için C derleyici ya da
  Apple framework'leri gerekmiyor, sadece hedefin `std`'si). Bu, kodun
  *şeklinin* tip kontrolünden geçtiğine dair gerçek bir sinyal, ama
  `main()` düz bir `todo!()` — hiçbir şey linklenmedi (gerçek
  framework'ler gerekir) ya da çalıştırılmadı (gerçek bir Mac gerekir),
  yani hâlâ başlangıç taslağı, çalışan kod değil.

Bunu bir sonraki oturumda gerçek bir Mac'te (ya da düzgün kurulmuş bir
osxcross ile) devam ettirecek kişi: önce `uzakel-windows`'tan `cargo build
--target <aarch64|x86_64>-apple-darwin -p bacak-remote-server`'ı konsol
`--pin` yoluyla yeşile çıkarsın, *sonra* bu dizinin paketleme/GUI
parçalarına dönsün.
