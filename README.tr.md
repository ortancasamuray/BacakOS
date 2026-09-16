# BacakOS

**Türkçe** · [English](README.md)

BacakOS, Debian 13 (trixie) tabanlı, özgün Wayland masaüstü ortamıdır. Masaüstü
onlarca ayrı uygulama yerine, temel sistem işlevlerini (Wi-Fi, Bluetooth,
ses, ayarlar) doğrudan tek bir native compositor içinde barındırır.

## Bileşenler

| Bileşen | Açıklama |
|---|---|
| [`bacak/`](bacak) | Masaüstü çekirdeği — [Smithay](https://smithay.github.io/) tabanlı Wayland compositor, eklentiler ve CLI |
| [`turan/`](turan) | Bacak Display Manager (BDM) — greeter ve oturum başlatıcı |
| [`altay/`](altay) | Dosya yöneticisi |
| [`Belgeler/`](Belgeler) | `bacak-belge` — birleşik PDF/EPUB/resim görüntüleyici |
| [`tahta/`](tahta) | GPU hızlandırmalı dijital tahta motoru |
| [`kur/`](kur) | Sistem yükleyici |
| [`uzakel/`](uzakel) | Uzak masaüstü: Android/Windows/macOS istemcileri ve BacakOS tarafındaki daemon |
| [`Buildeba/`](Buildeba) | Kurulabilir ISO'yu üretmek için kullanılan live-build yapılandırması |
| [`kilavuz/`](kilavuz) | Kullanım kılavuzu (HTML) |

Her bileşen dizininin kendi README'i, derleme ve mimari detaylarını içerir.

## Derleme

Tüm bileşenler için `.deb` paketlerini derle:

```bash
sudo bash setup-deps.sh   # bir kez: derleme + çalışma zamanı bağımlılıkları
bash build-debs.sh        # dist/*.deb üretir
```

Kurulabilir ISO'yu derle (root gerektirir, `dist/*.deb`'i
`Buildeba/config/packages.chroot` üzerinden kullanır):

```bash
sudo ./Buildeba/APbuild iso
```

## Lisans

GPL-3.0-or-later — bkz. [LICENSE](LICENSE).
