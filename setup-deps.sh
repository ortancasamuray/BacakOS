#!/usr/bin/env bash
# setup-deps.sh — BacakOS derleme bağımlılıklarını kurar.
#
# bacakos-install.sh çalıştırmadan önce bir kez çalıştırın:
#   sudo bash setup-deps.sh
#
# Desteklenen dağıtımlar: Debian 12/13, Ubuntu 22.04/24.04
set -euo pipefail

c_blue=$'\033[1;34m'; c_grn=$'\033[1;32m'; c_red=$'\033[1;31m'; c_rst=$'\033[0m'
log()  { printf '%s[setup-deps]%s %s\n' "$c_blue" "$c_rst" "$*"; }
ok()   { printf '%s[setup-deps] %s%s\n' "$c_grn" "$*" "$c_rst"; }
die()  { printf '%s[setup-deps] HATA: %s%s\n' "$c_red" "$*" "$c_rst" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "root olarak çalıştırın: sudo bash $0"
command -v apt-get >/dev/null || die "Bu script yalnızca Debian/Ubuntu sistemlerinde çalışır"

log "Paket listesi güncelleniyor…"
apt-get update -qq

# ---------------------------------------------------------------------------
# Sistem paketleri
# ---------------------------------------------------------------------------
log "Derleme bağımlılıkları kuruluyor…"
apt-get install -y \
    git curl \
    build-essential pkg-config clang \
    dpkg-dev \
    \
    libdbus-1-dev \
    libpam0g-dev \
    libseat-dev \
    libinput-dev \
    libgbm-dev \
    libudev-dev \
    libdrm-dev \
    libxkbcommon-dev \
    libwayland-dev \
    libfontconfig1-dev \
    libgl1-mesa-dev \
    libgles-dev \
    \
    weston dbus

ok "Derleme bağımlılıkları kuruldu"

# ---------------------------------------------------------------------------
# Çalışma zamanı bağımlılıkları
# ---------------------------------------------------------------------------
log "Çalışma zamanı bağımlılıkları kuruluyor…"

# Ses: compositor wpctl + pactl ile PipeWire'ı kontrol eder.
# bacak-session başlarken pipewire/wireplumber user servislerini başlatır.
apt-get install -y \
    pipewire pipewire-pulse pipewire-audio \
    wireplumber

# Wi-Fi: wpa_cli üzerinden bağlantı, dhcpcd ile IP alımı.
# NetworkManager wpa_supplicant'ı kendi içinde yönettiğinden
# /run/wpa_supplicant/<dev> soketini açmaz; standalone kurulum gerekir.
apt-get install -y \
    wpasupplicant \
    dhcpcd \
    iproute2

# NetworkManager varsa wpa_supplicant'ı bırakması için yönetimden çıkar.
if command -v nmcli >/dev/null 2>&1; then
    log "NetworkManager bulundu — Wi-Fi yönetimi wpa_supplicant'a devrediliyor"
    NM_CONF=/etc/NetworkManager/conf.d/99-bacak-wifi.conf
    if [ ! -f "$NM_CONF" ]; then
        mkdir -p /etc/NetworkManager/conf.d
        cat > "$NM_CONF" << 'EOF'
# BacakOS: Wi-Fi'ı wpa_supplicant/wpa_cli üzerinden yönetir.
[keyfile]
unmanaged-devices=type:wifi
EOF
        systemctl reload NetworkManager 2>/dev/null || true
        log "NetworkManager Wi-Fi yönetiminden çıkarıldı ($NM_CONF)"
    fi
fi

# Bluetooth: bluetoothctl üzerinden eşleme ve bağlantı.
apt-get install -y bluez

ok "Çalışma zamanı bağımlılıkları kuruldu"

# ---------------------------------------------------------------------------
# Rust / cargo
# ---------------------------------------------------------------------------
# Hangi kullanıcı için kuracağız?
REAL_USER="${SUDO_USER:-${USER:-root}}"
REAL_HOME="$(eval echo ~"$REAL_USER")"
CARGO_BIN="$REAL_HOME/.cargo/bin"

if [ -x "$CARGO_BIN/cargo" ]; then
    ok "Rust zaten kurulu: $($CARGO_BIN/rustup show active-toolchain 2>/dev/null || echo 'stable')"
else
    log "Rust (rustup) kuruluyor — kullanıcı: $REAL_USER"
    sudo -u "$REAL_USER" bash -c \
        'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable'
    ok "Rust kuruldu: $($CARGO_BIN/cargo --version)"
fi

# Önceki root çalışmaları .cargo altına root sahipli dosya bırakmış olabilir; düzelt.
chown -R "$REAL_USER:" "$REAL_HOME/.cargo" 2>/dev/null || true

# cargo-deb — Debian paketi oluşturmak için
if ! sudo -u "$REAL_USER" "$CARGO_BIN/cargo" deb --version >/dev/null 2>&1; then
    log "cargo-deb kuruluyor…"
    sudo -u "$REAL_USER" "$CARGO_BIN/cargo" install cargo-deb
    ok "cargo-deb kuruldu"
else
    ok "cargo-deb zaten mevcut"
fi

# ---------------------------------------------------------------------------
echo ""
ok "=== Tüm bağımlılıklar hazır ==="
log "Kuruluma geçmek için:"
log "  sudo bash bacakos-install.sh"
