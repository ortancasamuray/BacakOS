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
log "Sistem paketleri kuruluyor…"
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

ok "Sistem paketleri kuruldu"

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
