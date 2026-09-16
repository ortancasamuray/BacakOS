# BacakOS

🌐 [Türkçe](README.tr.md) · **English**

BacakOS is a custom Wayland-based Linux desktop environment built on Debian
13 (trixie). Instead of assembling a desktop from many separate applications,
core system functions (Wi-Fi, Bluetooth, audio, settings) live directly
inside a single native compositor.

Official site: [www.anadolupanteri.org.tr](https://www.anadolupanteri.org.tr)

## Components

| Component | Description |
|---|---|
| [`bacak/`](bacak) | The desktop core — a [Smithay](https://smithay.github.io/)-based Wayland compositor, plugins, and CLI |
| [`turan/`](turan) | Bacak Display Manager (BDM) — greeter and session launcher |
| [`altay/`](altay) | File manager |
| [`Belgeler/`](Belgeler) | `bacak-belge` — unified PDF/EPUB/image viewer |
| [`tahta/`](tahta) | GPU-accelerated digital whiteboard engine |
| [`kur/`](kur) | System installer |
| [`uzakel/`](uzakel) | Remote desktop: Android/Windows/macOS companion apps and the BacakOS-side daemon |
| [`kilavuz/`](kilavuz) | User guide (HTML) |

Each component directory has its own README with build and architecture
details.

## Building

Build all `.deb` packages for every component:

```bash
sudo bash setup-deps.sh   # once: build + runtime dependencies
bash build-debs.sh        # produces dist/*.deb
```

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).
