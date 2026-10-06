#!/usr/bin/env bash
# build-debs.sh — tüm BacakOS bileşenlerini deb paketi olarak derler.
#
# Kullanım:
#   sudo bash setup-deps.sh   # bir kez: derleme + çalışma zamanı bağımlılıkları
#   bash build-debs.sh         # root gerekmez
#   ls dist/                   # oluşan paketler
#
# Kurmak için (hedef makinede):
#   sudo apt install ./dist/*.deb
#   — veya tek paketle (bağımlılık listesi üzerinden aynı şeyi yapar):
#   sudo apt install ./dist/bacakos_*.deb
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BACAK_SRC="$HERE/bacak"
ALTAY_SRC="$HERE/altay"
TURAN_SRC="$HERE/turan"
DIST="$HERE/dist"

c_blue=$'\033[1;34m'; c_grn=$'\033[1;32m'; c_red=$'\033[1;31m'; c_rst=$'\033[0m'
log()  { printf '%s[build-debs]%s %s\n' "$c_blue" "$c_rst" "$*"; }
ok()   { printf '%s[build-debs] %s%s\n' "$c_grn" "$*" "$c_rst"; }
die()  { printf '%s[build-debs] HATA: %s%s\n' "$c_red" "$*" "$c_rst" >&2; exit 1; }

# sudo altında cargo PATH'te olmayabilir
for dir in "$HOME/.cargo/bin" "/home/${SUDO_USER:-}/.cargo/bin" "/root/.cargo/bin"; do
    [ -x "$dir/cargo" ] && export PATH="$dir:$PATH" && break
done
command -v cargo >/dev/null || die "cargo bulunamadı — önce: sudo bash setup-deps.sh"

if ! command -v cargo-deb >/dev/null 2>&1; then
    log "cargo-deb kuruluyor…"
    cargo install cargo-deb
fi

mkdir -p "$DIST"

# ---------------------------------------------------------------------------
# bacak workspace: compositor, plugin'ler, CLI
# ---------------------------------------------------------------------------
log "=== bacak workspace derleniyor ==="
(
    cd "$BACAK_SRC"

    log "compositor derleniyor (udev feature)…"
    cargo build --release -p bacak-compositor --features udev

    log "CLI derleniyor…"
    cargo build --release -p bacak-cli

    log "bacak-compositor deb oluşturuluyor"
    cargo deb --no-build -p bacak-compositor -o "$DIST"

    log "bacak-plugin-audio deb oluşturuluyor"
    cargo deb --no-build -p bacak-plugin-audio -o "$DIST"

    log "bacak-plugin-network deb oluşturuluyor"
    cargo deb --no-build -p bacak-plugin-network -o "$DIST"

    log "bacak-plugin-desktop-settings deb oluşturuluyor"
    cargo deb --no-build -p bacak-plugin-desktop-settings -o "$DIST"

    log "bacak-icons deb oluşturuluyor"
    cargo deb --no-build -p bacak-icons -o "$DIST"

    log "bacak-desktop-defaults deb oluşturuluyor"
    cargo deb --no-build -p bacak-desktop-defaults -o "$DIST"

    log "bacak-grub-theme deb oluşturuluyor"
    cargo deb --no-build -p bacak-grub-theme -o "$DIST"

    log "bacak-plymouth-theme deb oluşturuluyor"
    cargo deb --no-build -p bacak-plymouth-theme -o "$DIST"

    log "bacak (CLI) deb oluşturuluyor"
    cargo deb --no-build -p bacak-cli -o "$DIST"
)

# ---------------------------------------------------------------------------
# turan workspace: display manager, greeter, session launcher
# ---------------------------------------------------------------------------
log "=== turan workspace derleniyor ==="
(
    cd "$TURAN_SRC"

    log "bacak-display-manager derleniyor (system-pam)…"
    cargo build --release -p bacak-display-manager --features system-pam

    log "bacak-greeter derleniyor (gui)…"
    cargo build --release -p bacak-greeter --features gui

    log "bacak-session-launcher derleniyor…"
    cargo build --release -p bacak-session-launcher

    log "bacak-display-manager deb oluşturuluyor"
    cargo deb --no-build -p bacak-display-manager -o "$DIST"
)

# ---------------------------------------------------------------------------
# bacakonay: Turan girişi için iki adımlı doğrulama (bacakonay CLI + PAM modülü)
# ---------------------------------------------------------------------------
log "=== bacakonay derleniyor ==="
BACAKONAY_SRC="$HERE/bacakonay/linux"
(
    cd "$BACAKONAY_SRC"

    log "bacakonay derleniyor (CLI + pam_bacakonay.so)…"
    cargo build --release

    log "bacakonay deb oluşturuluyor"
    cargo deb --no-build -p bacakonay-cli -o "$DIST"
)

# ---------------------------------------------------------------------------
# uzakyonetim: filo yönetimi — ajan (her BacakOS) + sunucu (merkez makine)
# ---------------------------------------------------------------------------
log "=== uzakyonetim derleniyor ==="
UZAKYONETIM_SRC="$HERE/uzakyonetim"
(
    cd "$UZAKYONETIM_SRC"

    log "uzakyonetim derleniyor (ajan + sunucu)…"
    cargo build --release

    log "uzakyonetim-ajan deb oluşturuluyor"
    cargo deb --no-build -p uzy-ajan -o "$DIST"

    log "uzakyonetim-sunucu deb oluşturuluyor"
    cargo deb --no-build -p uzy-sunucu -o "$DIST"
)

# ---------------------------------------------------------------------------
# altay: dosya yöneticisi
# ---------------------------------------------------------------------------
log "=== altay derleniyor ==="
(
    cd "$ALTAY_SRC"

    log "altay derleniyor…"
    cargo build --release

    log "altay deb oluşturuluyor"
    cargo deb --no-build -o "$DIST"
)

# ---------------------------------------------------------------------------
# Belgeler: birleşik belge ve resim görüntüleyici
# ---------------------------------------------------------------------------
log "=== bacak-belge derleniyor ==="
BELGELER_SRC="$HERE/Belgeler"
(
    cd "$BELGELER_SRC"

    log "bacak-belge derleniyor…"
    cargo build --release -p bacak-belge

    log "bacak-belge deb oluşturuluyor"
    cargo deb --no-build -p bacak-belge -o "$DIST"
)

# ---------------------------------------------------------------------------
# tahta: dijital beyaz tahta
# ---------------------------------------------------------------------------
log "=== tahta derleniyor ==="
TAHTA_SRC="$HERE/tahta"
(
    cd "$TAHTA_SRC"

    log "tahta derleniyor…"
    cargo build --release

    log "tahta deb oluşturuluyor"
    cargo deb --no-build -o "$DIST"
)

# ---------------------------------------------------------------------------
# kur: sistem yükleyici
# ---------------------------------------------------------------------------
log "=== kur derleniyor ==="
KUR_SRC="$HERE/kur"
(
    cd "$KUR_SRC"

    log "kur derleniyor…"
    cargo build --release

    log "kur deb oluşturuluyor"
    cargo deb --no-build -o "$DIST"
)

# ---------------------------------------------------------------------------
# bacakos: içeriksiz meta-paket — tek `apt install` ile tüm bileşenleri kurar
# ---------------------------------------------------------------------------
log "=== bacakos meta-paketi oluşturuluyor ==="
(
    cd "$BACAK_SRC"
    cargo deb --no-build -p bacakos -o "$DIST"
)

# NOT: Ayarlar/ dizini kaldırıldı — BT/Wi-Fi/Ses/Ayarlar artık ayrı uygulama
# değil, bacak-compositor içindedir. (bkz. project-tree.md)

# NOT: dist/*.deb artık ISO'nun config/packages.chroot'una kopyalanmıyor.
# BacakOS paketlerinin tamamı ISO build'i sırasında depo.anadolupanteri.org.tr
# APT deposundan kuruluyor (bkz. config/hooks/normal/anadolupanteri.chroot ve
# config/package-lists/bacakos.list.chroot). packages.chroot'ta hem yerel
# hem de depo kopyası aynı anda bulunursa, versiyonlar eşitse apt hangisini
# kuracağına karar veremiyor/eskiyi seçebiliyor — bu yüzden yeni deb'leri
# ISO'ya değil, depo.anadolupanteri.org.tr'ye yüklemek gerekiyor.
#
# ---------------------------------------------------------------------------
echo ""
ok "=== Tüm paketler hazır: $DIST ==="
ls -lh "$DIST"/*.deb
echo ""
log "Hedef makinede kurmak için:"
log "  sudo apt install $DIST/*.deb"
