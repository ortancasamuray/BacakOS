#!/usr/bin/env bash
# setup-deps.sh — BacakOS derleme bağımlılıklarını kurar.
#
# bacakos-install.sh çalıştırmadan önce bir kez çalıştırın:
#   sudo bash setup-deps.sh
#
# arm64 / riscv64 deb'leri de derlenecekse ayrıca bir kez:
#   sudo bash setup-deps.sh --cross
#
# Desteklenen dağıtımlar: Debian 12/13, Ubuntu 22.04/24.04
set -euo pipefail

c_blue=$'\033[1;34m'; c_grn=$'\033[1;32m'; c_red=$'\033[1;31m'; c_rst=$'\033[0m'
log()  { printf '%s[setup-deps]%s %s\n' "$c_blue" "$c_rst" "$*"; }
ok()   { printf '%s[setup-deps] %s%s\n' "$c_grn" "$*" "$c_rst"; }
die()  { printf '%s[setup-deps] HATA: %s%s\n' "$c_red" "$*" "$c_rst" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "root olarak çalıştırın: sudo bash $0"
command -v apt-get >/dev/null || die "Bu script yalnızca Debian/Ubuntu sistemlerinde çalışır"

# ---------------------------------------------------------------------------
# --cross: arm64 + riscv64 deb'leri için çapraz derleme ortamı (build-debs.sh
# arm64 riscv64). Debian multiarch: çapraz gcc + hedef mimarinin -dev paketleri.
# ---------------------------------------------------------------------------
if [ "${1:-}" = "--cross" ]; then
    CROSS_ARCHES="arm64 riscv64"
    CROSS_DEV="libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
        libgbm-dev libdrm-dev libinput-dev libseat-dev libudev-dev \
        libxkbcommon-dev libwayland-dev libegl-dev libgl-dev libgles-dev \
        libdbus-1-dev libfontconfig-dev libfreetype-dev libharfbuzz-dev \
        libbz2-dev liblzma-dev zlib1g-dev libpam0g-dev"
    for a in $CROSS_ARCHES; do
        dpkg --add-architecture "$a"
    done
    log "Paket listesi güncelleniyor (çapraz mimariler: $CROSS_ARCHES)…"
    apt-get update -qq
    pkgs="clang libclang-dev lld python3 m4 cmake"
    for a in $CROSS_ARCHES; do
        pkgs="$pkgs crossbuild-essential-$a"
        for p in $CROSS_DEV; do pkgs="$pkgs $p:$a"; done
    done
    # --no-remove: amd64 paketlerinden biri kaldırılacaksa dur (ana sistem bozulmasın).
    # shellcheck disable=SC2086
    apt-get install -y --no-remove $pkgs

    REAL_USER="${SUDO_USER:-${USER:-root}}"
    REAL_HOME="$(eval echo ~"$REAL_USER")"
    log "Rust hedefleri ekleniyor — kullanıcı: $REAL_USER"
    sudo -u "$REAL_USER" "$REAL_HOME/.cargo/bin/rustup" target add \
        aarch64-unknown-linux-gnu riscv64gc-unknown-linux-gnu
    ok "Çapraz derleme ortamı hazır: bash build-debs.sh arm64 riscv64"
    exit 0
fi

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
    debhelper dh-dkms dkms \
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
    libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
    libfreetype-dev libharfbuzz-dev \
    libbz2-dev liblzma-dev zlib1g-dev \
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

# Wi-Fi: kartı NetworkManager yönetiyorsa compositor nmcli kullanır (WPA3 dahil);
# yönetmiyorsa wpa_cli + dhcpcd yoluna düşer — bu yüzden ikisi de kurulur.
apt-get install -y \
    network-manager \
    wpasupplicant \
    dhcpcd \
    iproute2

# Eski sürümler Wi-Fi'ı NetworkManager'dan çıkarıyordu; artık NM yönetmeli.
NM_CONF=/etc/NetworkManager/conf.d/99-bacak-wifi.conf
if [ -f "$NM_CONF" ]; then
    rm -f "$NM_CONF"
    systemctl reload NetworkManager 2>/dev/null || true
    log "Wi-Fi yönetimi NetworkManager'a geri verildi ($NM_CONF silindi)"
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
