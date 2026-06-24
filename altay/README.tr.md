# Altay

🌐 **Türkçe** · [English](README.md)

Modern, **dokunmatik dostu, kum havuzlu (sandbox)** bir Linux dosya yöneticisi —
Wayland öncelikli, Rust ile ve [Slint](https://slint.dev) GPU hızlandırmalı
arayüzle yazıldı. Tamamen normal kullanıcı olarak çalışır ve **asla root
gerektirmez**.

## Öne çıkanlar

- **Tasarım gereği kum havuzlu** — yalnızca ev dizininiz, bağlı
  çıkarılabilir/harici aygıtlar ve ağ paylaşımları erişilebilir; sistem dizinleri
  kapalıdır. Sembolik bağlantı ve `..` ile kaçışa karşı güvenli.
  (Bkz. [ARCHITECTURE.md](ARCHITECTURE.md).)
- **Arşivler** — zip/jar/apk, tar(.gz/.bz2/.xz/.zst), gz/bz2/xz/zst, 7z
  (oku+yaz), RAR (RAR5 + çok-bölümlü dahil), DEB, RPM ve bölünmüş
  `.001/.002…` ciltler; parolalı çıkarma; zip-slip koruması.
- **Aygıtlar** — canlı udisks2 hotplug, çıkarılabilir ortamı bağla/ayır.
- **Ağ** — gvfs üzerinden SMB/SFTP/FTP/WebDAV/NFS; kimlik bilgisi diyaloğu ve
  parolalar sistem anahtarlığında saklanır.
- **Önizleme** — görseller (önbellekli küçük resimler), PDF ilk sayfa, video
  karesi, metin.
- **Aktarımlar** — duraklat/devam/iptal destekli arka plan kopyala/taşı kuyruğu.
- **Arama** — diske önbelleklenen anlık dosya-adı indeksi + isteğe bağlı içerik
  araması.
- **Dokunmatik ve klavye** — dokun, çift dokun, uzun-basış menüsü, sürükle-taşı,
  ızgara için pinch-yerine yakınlaştırma, ve tam klavye kısayolları.
- **Koyu/açık tema**, tıklanır breadcrumb, bağlam menüsü, geri yüklemeli çöp.
- **Türkçe/İngilizce arayüz** — Ayarlar'dan canlı geçiş, ilk açılışta yerel
  ayardan otomatik.

## Derle ve çalıştır

```sh
cargo run            # hata ayıklama
cargo build --release
cargo test           # 37 birim testi
```

## Kurulum

```sh
sudo make install        # sistem geneli (/usr/local)
make install-user        # geçerli kullanıcı (~/.local), root gerekmez
```

Veya bir Debian paketi üret (`target/debian/altay_*.deb`):

```sh
scripts/build-deb.sh            # .deb üretir (gerekirse cargo-deb kurar)
scripts/build-deb.sh --lint     # ayrıca lintian çalıştırır
scripts/build-deb.sh --install  # üretip apt ile kurar
make deb                        # eşdeğeri: build-deb.sh --lint
```

Paket, linklenen kütüphaneleri otomatik, dlopen edilen Wayland/GL
kütüphanelerini ise **Depends** olarak bildirir; isteğe bağlı yardımcı araçlar
(gvfs, udisks2, polkit, portal, poppler, ffmpeg, anahtarlık, wl-clipboard)
**Recommends/Suggests** altındadır. Bir man sayfası (`altay(1)`) ve changelog
içerir ve **lintian-temizdir**.

## İsteğe bağlı çalışma-zamanı yardımcıları

Yardımcı yoksa ilgili özellik zarifçe devre dışı kalır:

| Özellik | Gerektirir |
|---|---|
| PDF önizleme | `poppler-utils` (pdftoppm/pdftocairo) veya `ghostscript` |
| Video küçük resimleri | `ffmpegthumbnailer` veya `ffmpeg` |
| Ağ bağlamaları | `gvfs` (`gio`) |
| İzin yükseltme | `pkexec` (PolicyKit) |
| Uygulamalar arası dosya kopyalama | `wl-clipboard` (veya `xclip`) |
| Aygıt bağla/ayır, hotplug | `udisks2` |
| Kayıtlı ağ parolaları | bir Secret Service anahtarlığı |
| İçe aktarma (dosya seçici) | `xdg-desktop-portal` |

## Proje

- Web sitesi: <https://anadolupanteri.org.tr>
- İletişim: <bilgi@anadolupanteri.org.tr>

## Lisans

GPL-3.0-or-later — bkz. [COPYING](COPYING).
