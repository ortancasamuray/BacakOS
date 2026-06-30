# Bacakos — Proje Ağacı

> Bu dosya tüm projenin neyin nerede olduğunu gösterir.
> Yeni bir özellik istendiğinde önce buraya bakın; ilgili kod varsa onu düzenleyin.

---

## Dizin Yapısı

```
/home/os2/bacakos/
├── bacak/          Wayland compositor + dock + control center
├── turan/          Ekran yöneticisi (BDM) + greeter + PAM
├── Belgeler/       PDF / EPUB / resim / belge görüntüleyici
├── altay/          Dosya yöneticisi
├── build-debs.sh   Tüm paketleri derle
└── setup-deps.sh   Bağımlılıkları kur
```

---

## bacak — Compositor

**Kök:** `/home/os2/bacakos/bacak/`
**Binary:** `/usr/bin/bacak-compositor`

### Önemli Kaynak Dosyaları

| Dosya | İçerik |
|-------|--------|
| `src/state.rs` | Ana durum yapısı `BacakState` — tüm paneller, pencereler, event işleme burada |
| `src/udev_runtime.rs` | Native DRM/KMS ana döngüsü (production) |
| `src/runtime.rs` | Winit backend (geliştirme/test) |
| `src/render.rs` | GPU render + tüm panellerin ekrana çizimi |
| `src/bluetooth.rs` | BlueZ entegrasyonu — `bluetoothctl` coprocess yönetimi |
| `src/wm.rs` | Pencere yöneticisi (tiling, workspace) |
| `src/input.rs` | Klavye / fare / dokunmatik girdi |
| `src/config.rs` | `~/.config/bacak/compositor.json` okuma |
| `src/session.rs` | Oturum geri yükleme/kaydetme |
| `src/animation.rs` | Spring animasyonları |
| `src/gestures.rs` | Dokunmatik jestler (swipe, pinch, long-press) |
| `src/icons.rs` | Uygulama ikonları |
| `src/launcher.rs` | Uygulama başlatma |

### Plugin Modülleri (`src/plugins/`)

Her plugin bir UI paneli veya sistem özelliği:

| Dosya | Panel / Özellik |
|-------|----------------|
| `control_center.rs` | **Kontrol Merkezi** (hızlı ayarlar kutusu) — Wi-Fi, BT, ses, parlaklık, karanlık mod, ekran görüntüsü, güç |
| `desktop_settings.rs` | **Masaüstü Ayarları** — duvar kağıdı, hostname, otomatik giriş, parola değiştirme |
| `dock.rs` | Dock çubuğu — sabitlenmiş uygulamalar, geçici uygulamalar, başlatıcı |
| `apps_menu.rs` | Uygulama listesi (tüm kurulu uygulamalar) |
| `network.rs` | Wi-Fi ağ bağlantı mantığı (`nmcli` tabanlı) |
| `audio.rs` | Ses cihazı seçimi (`pactl` tabanlı) |
| `overview.rs` | Pencere/workspace genel görünümü |
| `screenshot.rs` | Ekran görüntüsü alma |
| `keyboard.rs` | Ekran klavyesi |
| `selection.rs` | Metin seçimi ve kopyalama |
| `gestures.rs` | Jestlere bağlı eylemler |

### Bluetooth (compositor içinde)

**Tüm BT kontrolü compositor'dadır — ayrı uygulama YAZMA.**

| Yapı / Fonksiyon | Yer | Açıklama |
|-----------------|-----|----------|
| `BtCtl` | `bluetooth.rs` | `bluetoothctl` coprocess — eşleşme, güven, bağlantı |
| `BtPanel` | `plugins/control_center.rs` | BT paneli görsel yapısı |
| `BtDevice`, `BtRow`, `BtAction` | `plugins/control_center.rs` | BT UI tipleri |
| `bt_poll()` | `state.rs:6449` | Her tick'te BT olaylarını işle |
| `build_bt_panel()` | `state.rs` | BT panelini render için hazırla |
| `open_bt_panel()` | `state.rs` | BT panelini aç |
| `start_obex_receiver()` | `bluetooth.rs:265` | Python OBEX ajanı — dosya alma → `~/Downloads` |

### Wi-Fi (compositor içinde)

| Yapı | Yer |
|------|-----|
| `WifiPanel`, `WifiRow` | `plugins/control_center.rs` |
| `WifiAction`, `WifiMsg` | `plugins/control_center.rs` |
| `wifi_poll()`, `build_wifi_panel()` | `state.rs` |
| `wifi_connect()` | `plugins/network.rs` — `nmcli` çağrısı |

### Masaüstü Ayarları (compositor içinde)

| Yapı | Yer |
|------|-----|
| `DesktopSettingsPanel`, `DsRow`, `DsAction` | `plugins/desktop_settings.rs` |
| `DsMode` | `plugins/desktop_settings.rs` — Ana, root-auth, hostname, parola |
| `ds_tick()`, `build_ds_panel()` | `state.rs` |

### Ses (compositor içinde)

| Yapı | Yer |
|------|-----|
| `AudioPanel`, `MicPanel` | `plugins/control_center.rs` |
| `audio_connect()`, `list_sinks()` | `plugins/audio.rs` — `pactl` çağrısı |

### Build

```bash
# Geliştirme (hızlı, winit penceresi):
cargo run -p bacak-compositor --features runtime

# Production (DRM/KMS, gerçek oturum):
cargo build --release -p bacak-compositor --features udev

# Paket oluştur + kur:
cargo deb -p bacak-compositor --no-build
sudo dpkg -i target/debian/bacak-compositor_0.1.0-1_amd64.deb
sudo pkill bacak-compositor
```

---

## turan — Ekran Yöneticisi (BDM)

**Kök:** `/home/os2/bacakos/turan/`
**Binary:** `/usr/bin/bacak-display-manager`

| Krate | Binary | Açıklama |
|-------|--------|----------|
| `bacak-display-manager` | `/usr/bin/bacak-display-manager` | BDM daemon — oturum yönetimi, DRM master |
| `bacak-greeter` | greeter | Giriş ekranı UI (Slint) |
| `bacak-session-launcher` | session-launcher | Oturumu başlatan yardımcı |
| `bacak-common` | lib | BDM ↔ greeter IPC protokolü, PAM, güç yönetimi |
| `bacak-pam` | lib | PAM entegrasyonu |

### IPC (BDM ↔ Greeter)

`bacak-common/src/ipc.rs` — Unix soket protokolü, newline-delimited JSON.

---

## Belgeler — Belge Görüntüleyici

**Kök:** `/home/os2/bacakos/Belgeler/`
**Workspace:** `~/bacakos/Belgeler/`

| Krate | Binary | Açıklama |
|-------|--------|----------|
| `bacak-belge` | `/usr/bin/bacak-belge` | Birleşik görüntüleyici: PDF + EPUB + resim + metin |
| `bacak-pdf` | `/usr/bin/bacak-pdf` | Sadece PDF (`pdfium`) |
| `bacak-epub` | `/usr/bin/bacak-epub` | Sadece EPUB |
| `bacak-resim` | `/usr/bin/bacak-resim` | Sadece resim |

**UI:** Her krate `ui/main.slint` dosyasında Slint UI tanımlar.
**Araçlar:** kalem, marker, silgi, renk seçimi, sayfa kaydırma (tek parmak pan), yakınlaştırma (pinch-to-zoom).

---

## altay — Dosya Yöneticisi

**Kök:** `/home/os2/bacakos/altay/`
**Binary:** `/usr/bin/altay`
**UI:** `ui/main.slint`

| Modül | Yer | Açıklama |
|-------|-----|----------|
| Dosya sistemi | `src/filesystem/` | Listeleme, kopyalama, taşıma |
| Arşiv | `src/archive/` | tar, zip, 7z, rar, deb, rpm |
| Arama | `src/search/` | Dosya adı ve içerik arama |
| Önizleme | `src/preview/` | Resim, metin, PDF önizlemesi |
| Çöp kutusu | `src/trash/` | XDG çöp kutusu |
| Aygıtlar | `src/devices/` | USB, disk bağlama/çıkarma |
| Ağ | `src/network/` | Ağ sürücüleri |
| İzinler | `src/permissions/` | Dosya izinleri |
| Portal | `src/portal/` | XDG desktop portal |

---

## Kurulu Uygulamalar / Dock

Dock'ta varsayılan sabitlenmiş uygulamalar:
- `firefox-esr` — Tarayıcı
- `libreoffice-startcenter` — Ofis
- `bacak-belge` — Belge görüntüleyici

Compositor'un dock düğmeleri (sentinel):
- Uygulama başlatıcı (`\u{1}bacak:apps`)
- Son uygulamalar (`\u{1}bacak:recents`)
- Ayarlar / Kontrol Merkezi (`\u{1}bacak:settings`) → `open_control_center()`

---

## Sistem Entegrasyonları

| Araç | Nerede kullanılır |
|------|-------------------|
| `bluetoothctl` | `bacak/src/bluetooth.rs` — coprocess |
| Python OBEX ajan | `bacak/src/bluetooth.rs:start_obex_receiver()` → `~/.cache/bacak/obex-agent.py` |
| `nmcli` | `bacak/src/plugins/network.rs` — Wi-Fi bağlantı |
| `pactl` | `bacak/src/plugins/audio.rs` — ses cihazları |
| `pdfium` | `Belgeler/crates/bacak-belge/` — PDF render |
| PAM | `turan/crates/bacak-pam/` — kullanıcı kimlik doğrulama |

---

## Geliştirme Kuralları

1. **Bir özellik compositor'da varsa → `bacak/crates/bacak-compositor/src/` içinde düzenle**
2. **Yeni uygulama YAZMA** — kontrol merkezi, BT, Wi-Fi, ses, ayarlar compositor içindedir
3. **UI değişiklikleri** → `state.rs` + `render.rs` + ilgili `plugins/*.rs`
4. **Bluetooth** → `bluetooth.rs` + `state.rs::bt_poll()` + `plugins/control_center.rs::BtPanel`
5. **Wi-Fi** → `plugins/network.rs` + `state.rs::wifi_poll()` + `plugins/control_center.rs::WifiPanel`
6. **Ayarlar** → `plugins/desktop_settings.rs` + `state.rs::ds_tick()`
