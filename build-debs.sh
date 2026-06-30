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
# Ayarlar: bluetooth ve sistem ayarları uygulaması
# ---------------------------------------------------------------------------
log "=== bacak-ayarlar derleniyor ==="
AYARLAR_SRC="$HERE/Ayarlar"
(
    cd "$AYARLAR_SRC"

    log "bacak-ayarlar derleniyor…"
    cargo build --release -p bacak-ayarlar

    log "bacak-ayarlar deb oluşturuluyor"
    cargo deb --no-build -p bacak-ayarlar -o "$DIST"
)

# ---------------------------------------------------------------------------
echo ""
ok "=== Tüm paketler hazır: $DIST ==="
ls -lh "$DIST"/*.deb
echo ""
log "Hedef makinede kurmak için:"
log "  sudo apt install $DIST/*.deb"
