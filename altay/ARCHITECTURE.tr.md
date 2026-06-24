# Altay — Mimari Özeti

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Bu, [ARCHITECTURE.md](ARCHITECTURE.md) dosyasının kısa Türkçe özetidir.
Wayland öncelikli, Rust + [Slint](https://slint.dev) ile yazılmış, **kum havuzlu
(sandbox)** bir dosya yöneticisi. **Asla root olarak çalışmaz.**

## Güvenlik modeli (çekirdek fikir)

UI ile arka uç arasındaki **her yol** `security::Sandbox`'tan geçer; diske
erişmenin başka yolu yoktur.

- **İzin listesi (allow-list)**: ev dizini, çıkarılabilir/harici bağlamalar
  (`/media/$USER`, `/run/media/$USER`, `/mnt`), gvfs ağ bağlamaları
  (`$XDG_RUNTIME_DIR/gvfs`).
- **Red listesi (deny-list)**: `/root /etc /usr /var /boot /proc /sys /dev …`.
- **Sembolik bağlantılar kontrolden önce çözülür** (`canonicalize`) — `~`
  içindeki `/etc`'ye işaret eden bir symlink kaçış sağlayamaz.
- **`..` tırmanışı** canonicalize ile çökertilir; var olmayan yollar için
  (`resolve_for_create`) sentetik kuyrukta `.`/`..` reddedilir.
- Doğrulanmış yol `SafePath`'tir; **yalnızca sandbox üretebilir** — bu yüzden
  "doğrulanmış" olma durumu kod genelinde taklit edilemez.

Garantileri çalıştır: `cargo test` (37 test; güvenlik-kritik olanlar dahil).

## Modül haritası (`src/`)

| Modül | Sorumluluk |
|---|---|
| `security` | Sandbox, izin/red politikası, kök keşfi, `SafePath`. |
| `filesystem` | Listeleme, doğal sıralama, kopyala/taşı/yeniden adlandır/oluştur/çoğalt. |
| `trash` | freedesktop Çöp; çöpe at / listele / **orijinaline geri yükle** / boşalt. |
| `search` | Bellekte canlı dosya-adı **indeksi** (arka plan + `notify`), **diske kalıcı**, **içerik araması**, yürüyüş yedeği. |
| `permissions` | POSIX mod bitlerini oku/değiştir; **PolicyKit yükseltme** (`pkexec`) — yine sandbox sınırlı, uygulama asla root olmaz. |
| `desktop` | `xdg-open` ile portal üzerinden açma; uygulamalar-arası pano (gnome-copied-files). |
| `portal` | xdg-desktop-portal **FileChooser** (`ashpd`) — "İçe Aktar". |
| `devices` | Canlı udisks2 hotplug + birim **bağla/ayır** (zbus); sistem bölümleri gizli. |
| `network` | `gio` ile gvfs bağla/ayır; kimlik diyaloğu; parolalar **sistem anahtarlığında**. |
| `archive` | zip/tar/7z/**RAR**/**DEB**/**RPM** + **bölünmüş çok-bölümlü**; parolalı çıkarma; zip-slip koruması. |
| `preview` | Önizleme paneli: görsel (küçük resim önbelleği) + **PDF** + **video karesi** + metin. |
| `transfer` | Arka plan kopyala/taşı kuyruğu: duraklat/devam/iptal, canlı ilerleme. |
| `ui` (`ui/main.slint`) | Kenar çubuğu, **tıklanır breadcrumb**, ızgara/liste/sıkışık, **yakınlaştırılabilir ızgara**, önizleme/transfer panelleri, **bağlam menüsü** (sağ-tık/uzun-basış), **klavye kısayolları**, **ayarlar** (gizli dosya, **koyu/açık tema**, varsayılan görünüm, **dil**). |

## Dokunmatik / jest / klavye

Tek dokunuş = seç + önizle, çift dokunuş = aç, **uzun-basış** (500 ms `Timer`) =
bağlam menüsü, **bas-sürükle** = taşı (hayalet etiketle), **Ctrl+tekerlek / ＋－**
= ızgara yakınlaştırma. Klavye: `Delete` çöp, `Backspace` üst, `F2` yeniden
adlandır, `F5` yenile, `Esc` iptal/temizle, `Ctrl+C/X/V` kopyala/kes/yapıştır,
`Ctrl+A` tümünü seç, `Ctrl+L` yol düzenle, `Ctrl+F` aramaya odaklan.

## i18n

Tüm arayüz metinleri Slint `L` global'i üzerinden (varsayılan İngilizce);
durum çubuğu mesajları Rust `sx()` yardımcısı üzerinden. **İngilizce / Türkçe /
İspanyolca** arasında **canlı geçiş**, ilk açılışta sistem yerel ayarından
(`LANG`/`LC_*`) otomatik (`Ayarlar`'da seçilir, config'e kaydedilir).

## Slint 1.16'nın açık olmayan iki jesti (taşınabilir ikameler)

- **Uygulamalar-arası OS sürükleme** yok → sistem panosu (gnome-copied-files).
- **İki parmak pinch** yok (çoklu-dokunma yok) → Ctrl+tekerlek + ＋／－ butonları
  aynı `grid-scale`'i sürer.

## Derle, çalıştır, kur

```sh
cargo test               # güvenlik + mantık (37 test)
cargo run                # başlat (Wayland yerel veya DISPLAY ile X11)
sudo make install        # sistem geneli: ikili + .desktop + hicolor ikon
make install-user        # ~/.local altında, root'suz
scripts/build-deb.sh     # lintian-temiz .deb üret
```
