# BacakOsSDK

[Türkçe](README.tr.md) · **English**

The shared development kit for BacakOS applications. It gathers the desktop
integration every app used to write on its own, and lets new (third-party)
applications fit into BacakOS.

| Crate | Provides |
|---|---|
| [`bacak-sdk`](crates/bacak-sdk) | Desktop-wide dark mode, colour palette, `.desktop` entries, icon lookup, MIME/default apps, compositor IPC client |
| [`bacak-sdk-slint`](crates/bacak-sdk-slint) | `BacakTema` and widgets for Slint (window, button, card, switch, heading, text); follows dark mode by itself |
| [`bacak-ipc`](crates/bacak-ipc) | Compositor ↔ application protocol and shared settings formats (the compositor depends on it too) |

Plus: [`sablon/`](sablon) + [`araclar/yeni-uygulama.sh`](araclar/yeni-uygulama.sh)
(new-app skeleton) and [`ornekler/merhaba`](ornekler/merhaba) (an example using
every feature).

Licence: **MIT OR Apache-2.0** (the rest of BacakOS is GPL-3.0-or-later; the SDK
is permissive so closed-source applications can use it too).

## Quick start

```sh
BacakOsSDK/araclar/yeni-uygulama.sh not-defteri "Not Defteri"
cd not-defteri
cargo run            # run it
cargo deb            # target/debian/not-defteri_0.1.0-1_amd64.deb
```

The generated project has a Slint UI (`ui/app.slint`), a main window bound to
`BacakTema`, a `.desktop` file and icon (`paket/`) and `cargo deb` settings.
Once installed the app shows in the app menu and dock with its own icon:
`slint::set_xdg_app_id` matches the `.desktop` name, so the dock maps the
window to its entry.

## bacak-sdk

```toml
[dependencies]
bacak-sdk = { path = "…/BacakOsSDK/crates/bacak-sdk" }
```

### Appearance (dark mode)

Dark mode is a **desktop-wide** setting (`~/.config/bacak/gorunum.json`). The
compositor writes it from the Control Center / Desktop Settings switch and
announces changes over IPC. Applications should **not** keep a separate dark
theme preference: an in-app switch calls `ayarla` and everything changes
together.

```rust
use bacak_sdk::{gorunum, Gorunum, Palet};

let g = Gorunum::load();                 // works without a compositor too
let palet = Palet::icin(g);              // arka, yuzey, metin, vurgu, tehlike…
gorunum::izle(|g| println!("dark: {}", g.koyu));   // on change (background thread)
gorunum::ayarla(false)?;                 // switch the whole desktop to light
```

`izle` uses the compositor event while connected and polls the file
otherwise; it survives compositor restarts.

### Applications (`.desktop`)

```rust
use bacak_sdk::masaustu;

for app in masaustu::uygulamalar() { println!("{} ({})", app.ad, app.id); }   // menu entries, by name
let belge = masaustu::uygulama("bacak-belge").unwrap();   // id, id.desktop or window class
belge.baslat(&[Path::new("/home/ali/rapor.pdf")])?;       // in a terminal if Terminal=true
```

Localised names such as `Name[tr]` are picked by `LANG`. `Exec` is parsed per
the spec: quoted arguments stay whole, `%f %F %u %U %c %k` are expanded.

### Icons

```rust
let path = bacak_sdk::simge::bul("folder", 48);                // PNG or SVG path
let path = bacak_sdk::simge::uygulama_simgesi(&app, 64);
let path = bacak_sdk::simge::mime_simgesi("application/pdf", 32);
```

Search order matches the dock: theme chosen in Desktop Settings/GTK →
BacakOS → Breeze/Adwaita/Papirus… → other installed themes → hicolor → pixmaps.

### MIME and "Open With"

```rust
use bacak_sdk::mime;
let kind = mime::tur(file);                       // xdg-mime, else by extension
let default = mime::varsayilan("application/pdf");
let choices = mime::uygulamalar_icin("application/pdf");   // default first
mime::varsayilan_yap("application/pdf", "bacak-belge.desktop")?;
mime::ac(file)?;  mime::birlikte_ac(file, "altay.desktop")?;
```

### Compositor IPC

```rust
use bacak_sdk::{bildir, Istemci};

bildir("Exported", "rapor.pdf")?;                 // a silent no-op outside BacakOS
let mut c = Istemci::baglan()?;                   // ErrorKind::NotFound without a session
c.kontrol_merkezi(Some(true))?;
for w in c.pencereler()? { println!("{} {}", w.uygulama, w.baslik); }
for o in c.ekranlar()? { println!("{}×{} @{}x", o.genislik, o.yukseklik, o.olcek); }
for ev in Istemci::baglan()?.abone_ol()? { println!("{ev:?}"); }   // blocks
```

Each request waits at most 2 s (`ipc::ZAMAN_ASIMI`); don't call it in a tight
UI-thread path.

## bacak-sdk-slint

```toml
[dependencies]
bacak-sdk-slint = { path = "…/crates/bacak-sdk-slint" }
slint = { version = "1.8", … }
[build-dependencies]
bacak-sdk-slint = { path = "…/crates/bacak-sdk-slint", features = ["derleme"] }
```

```rust
// build.rs
fn main() { bacak_sdk_slint::derle("ui/app.slint").unwrap(); }
```

```slint
// ui/app.slint
import { BacakTema, BacakPencere, BacakDugme, BacakKart, BacakAnahtar } from "@bacak";
export { BacakTema }
export component Uygulama inherits BacakPencere {
    BacakKart {
        BacakAnahtar { metin: "Koyu mod"; acik: BacakTema.koyu; degisti(v) => { /* to Rust */ } }
        BacakDugme { metin: "Kaydet"; birincil: true; tiklandi => { … } }
    }
}
```

```rust
// main.rs
slint::include_modules!();
let ui = Uygulama::new()?;
bacak_sdk_slint::tema_bagla!(ui, BacakTema);          // follows dark mode
ui.on_koyu_degisti(bacak_sdk_slint::koyu_mod_ayarla);  // desktop-wide
```

| Component | Properties |
|---|---|
| `BacakTema` (global) | `koyu`; colours `arka yuzey yuzey2 cizgi metin soluk vurgu vurgu-metin tehlike uyari basari`; metrics `yaricap yaricap-kucuk bosluk dolgu dokunma yazi yazi-kucuk yazi-baslik` |
| `BacakPencere` | Main window; background and font size from the theme, std-widgets' colour scheme (`Palette.color-scheme`) follows dark mode too |
| `BacakDugme` | `metin`, `birincil`, `tehlike`, `etkin`, `tiklandi()` — at least a 44 px touch target |
| `BacakKart` | Bordered surface with a vertical layout inside |
| `BacakAnahtar` | `metin`, `acik` (two-way), `etkin`, `degisti(bool)` |
| `BacakBaslik`, `BacakMetin` | Heading; body text (`soluk: true` for secondary) |

The colours equal `bacak_sdk::Palet`; update both together.

## IPC protocol

Socket: `$BACAK_IPC` (exported by the compositor to its children), else
`$XDG_RUNTIME_DIR/bacak-ipc-<WAYLAND_DISPLAY>.sock`. One JSON object per line
(UTF-8, `\n`, at most 64 KiB). The client-chosen `id` of a request comes back
in its reply; events carry no `id`. Unknown fields are ignored.

| Request | Fields | Reply `veri` |
|---|---|---|
| `surum` | — | `{"protokol":1,"compositor":"0.1.0"}` |
| `bildirim` | `baslik`, `ayrinti`? | — (error if the title is empty; texts cut at 200 characters) |
| `gorunum_al` | — | `{"koyu":true}` |
| `koyu_mod_ayarla` | `koyu` | — (saved, announced to subscribers) |
| `kontrol_merkezi` | `ac`? (`true`/`false`, absent toggles) | — |
| `pencereler` | — | `[{"id","uygulama","baslik","calisma_alani","odakli"}]` |
| `ekranlar` | — | `[{"id","x","y","genislik","yukseklik","olcek"}]` |
| `abone` | — | — ; then events: `{"olay":"gorunum","koyu":…}`, `{"olay":"odak","pencere":id\|null}` |

```sh
# From a shell:
printf '{"id":1,"istek":"bildirim","baslik":"Merhaba"}\n' | socat - UNIX-CONNECT:"$BACAK_IPC"
```

Error reply: `{"id":N,"tamam":false,"hata":"…"}`.

**Security.** The socket lives in the session user's `0700` runtime directory
with mode `0600`; only that user's processes can connect. The protocol offers
**no** window contents, key input or command execution. Limits: 64 concurrent
connections, 64 KiB per line, a client that doesn't read its replies is
dropped before 1 MiB backs up.

## Status in existing applications

| Application | SDK use |
|---|---|
| bacak-compositor | IPC server (`src/ipc.rs`); reads/writes dark mode in `gorunum.json`, announces it to subscribers |
| altay | Dark mode is desktop-wide (its own preference is no longer read); `.desktop`/`Exec`/MIME/default apps through `bacak_sdk` (icon lookup is still altay's own for now) |
| bacak-belge | EPUB reading mode starts from the desktop's dark mode (the ☀ button only affects that window) |

Next candidates: the `.desktop`/icon code in the compositor's `icons.rs`,
altay's icon lookup, the colours of kur and tahta.

## Build and test

```sh
cd BacakOsSDK
cargo test                       # bacak-ipc, bacak-sdk (IPC client against a fake server)
cargo run -p bacak-merhaba       # example app (inside a BacakOS session)
```

Compositor side: `cd bacak && cargo test -p bacak-compositor --features udev ipc`.
