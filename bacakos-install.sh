#!/usr/bin/env bash
# bacakos-install.sh — her bileşeni kaynaktan derleyip kurar.
#
# Kullanım:
#   sudo bash bacakos-install.sh            # hepsini kur
#   sudo bash bacakos-install.sh compositor # sadece compositor
#   sudo bash bacakos-install.sh altay      # sadece altay
#   sudo bash bacakos-install.sh bdm        # sadece display manager
#   sudo bash bacakos-install.sh enable     # BDM'i etkin display manager yap
#   sudo bash bacakos-install.sh revert     # Önceki display manager'a geri dön
#
# Her çalıştırmada kaynak koddan yeniden derleme yapılır; eski binary/paket kullanılmaz.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BACAK_SRC="$HERE/bacak"
ALTAY_SRC="$HERE/altay"
TURAN_SRC="$HERE/turan"

c_blue=$'\033[1;34m'; c_grn=$'\033[1;32m'; c_red=$'\033[1;31m'; c_rst=$'\033[0m'
log()  { printf '%s[bacakos]%s %s\n' "$c_blue" "$c_rst" "$*"; }
ok()   { printf '%s[bacakos] %s%s\n' "$c_grn" "$*" "$c_rst"; }
die()  { printf '%s[bacakos] HATA: %s%s\n' "$c_red" "$*" "$c_rst" >&2; exit 1; }

need_root() { [ "$(id -u)" -eq 0 ] || die "root olarak çalıştırın: sudo $0 $*"; }

# sudo altında ~/.cargo/bin PATH'te olmayabilir; SUDO_USER'ın ortamını ara.
_setup_cargo_path() {
    command -v cargo >/dev/null && return 0
    local cargo_home
    # rustup varsayılan konumu
    for dir in "$HOME/.cargo/bin" "/home/$SUDO_USER/.cargo/bin" "/root/.cargo/bin"; do
        if [ -x "$dir/cargo" ]; then
            export PATH="$dir:$PATH"
            return 0
        fi
    done
    return 1
}

need_cargo() {
    _setup_cargo_path || die "cargo (Rust toolchain) kurulu değil — önce rustup ile kurun: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
}

# ---------------------------------------------------------------------------
install_compositor() {
    log "=== bacak-compositor derleniyor ==="
    need_cargo
    ( cd "$BACAK_SRC" && cargo build --release -p bacak-compositor --features udev )
    local bin="$BACAK_SRC/target/release/bacak-compositor"
    [ -x "$bin" ] || die "derleme başarısız: $bin bulunamadı"

    local ts; ts="$(date +%Y%m%d-%H%M%S)"
    [ -x /usr/bin/bacak-compositor ] && cp -a /usr/bin/bacak-compositor "/usr/bin/bacak-compositor.bak-$ts"
    install -m755 "$bin" /usr/bin/bacak-compositor
    ok "bacak-compositor kuruldu ($(du -h "$bin" | cut -f1))"
}

# ---------------------------------------------------------------------------
install_altay() {
    log "=== altay derleniyor ==="
    need_cargo
    ( cd "$ALTAY_SRC" && cargo build --release )
    local bin="$ALTAY_SRC/target/release/altay"
    [ -x "$bin" ] || die "derleme başarısız: $bin bulunamadı"

    install -Dm755 "$bin"                          /usr/local/bin/altay
    install -Dm644 "$ALTAY_SRC/altay.desktop"      /usr/local/share/applications/altay.desktop
    install -Dm644 "$ALTAY_SRC/assets/altay.png"   /usr/local/share/icons/hicolor/256x256/apps/altay.png
    update-desktop-database /usr/local/share/applications 2>/dev/null || true
    gtk-update-icon-cache   /usr/local/share/icons/hicolor 2>/dev/null || true
    ok "altay kuruldu ($(du -h "$bin" | cut -f1))"
}

# ---------------------------------------------------------------------------
install_bdm() {
    log "=== bacak-display-manager derleniyor ==="
    need_cargo
    command -v cargo-deb >/dev/null || cargo install cargo-deb
    ( cd "$TURAN_SRC"
      cargo build --release -p bacak-display-manager --features system-pam
      cargo build --release -p bacak-greeter --features gui --bin bacak-greeter
      cargo build --release -p bacak-session-launcher
      cargo deb --no-build -p bacak-display-manager )
    local deb; deb="$(ls -t "$TURAN_SRC"/target/debian/bacak-display-manager_*_amd64.deb | head -n1)"
    [ -f "$deb" ] || die "deb paketi oluşturulamadı"
    apt-get install -y "$deb" weston dbus || { dpkg -i "$deb" || true; apt-get -f install -y; }

    # Compositor'ı başlatıp oturum ortamını ayarlayan sarmalayıcı betik.
    cat > /usr/bin/bacak-session << 'EOF'
#!/bin/sh
export GTK_IM_MODULE=wayland QT_IM_MODULE=wayland XMODIFIERS=@im=none
exec /usr/bin/bacak-compositor "$@"
EOF
    chmod 0755 /usr/bin/bacak-session

    # Display manager oturum girişi — SDDM/GDM/greetd bu dosyayı listeler.
    mkdir -p /usr/share/wayland-sessions
    cat > /usr/share/wayland-sessions/bacak.desktop << 'EOF'
[Desktop Entry]
Name=Bacak Desktop
Comment=The Bacak Wayland desktop session
Exec=bacak-session
Type=Application
DesktopNames=Bacak
EOF

    ok "bacak-display-manager kuruldu ($deb)"
}

# ---------------------------------------------------------------------------
enable_bdm() {
    log "=== bacak-display-manager etkinleştiriliyor ==="
    command -v bacak-display-manager >/dev/null || die "BDM kurulu değil — önce çalıştırın: sudo $0 bdm"
    [ -x /usr/bin/bacak-compositor ]  || die "bacak-compositor bulunamadı — önce çalıştırın: sudo $0 compositor"

    # Mevcut display manager'ı kaydet (geri dönüş için).
    local state_dir="/var/lib/bacak-display-manager"
    mkdir -p "$state_dir"
    local prev
    prev="$(basename "$(readlink -f /etc/systemd/system/display-manager.service 2>/dev/null)" .service 2>/dev/null || true)"
    if [ -n "$prev" ] && [ "$prev" != "bacak-display-manager" ]; then
        echo "$prev" > "$state_dir/previous-dm"
        log "Önceki display manager kaydedildi: $prev"
    fi

    systemctl disable "$prev.service" 2>/dev/null || true
    systemctl enable bacak-display-manager.service
    systemctl daemon-reload
    ok "bacak-display-manager etkinleştirildi (reboot sonrası otomatik başlar)"

    log "Hemen başlatmak için: sudo systemctl start bacak-display-manager"
    log "Geri dönmek için:     sudo $0 revert"
}

# ---------------------------------------------------------------------------
revert_bdm() {
    log "=== Önceki display manager'a dönülüyor ==="
    local state_dir="/var/lib/bacak-display-manager"
    local prev; prev="$(cat "$state_dir/previous-dm" 2>/dev/null || true)"
    [ -n "$prev" ] || die "Önceki display manager kaydı bulunamadı ($state_dir/previous-dm)"

    systemctl stop bacak-display-manager.service 2>/dev/null || true
    systemctl disable bacak-display-manager.service 2>/dev/null || true
    systemctl enable "$prev.service" 2>/dev/null || true
    rm -f /etc/systemd/system/display-manager.service
    local frag; frag="$(systemctl show "$prev.service" -p FragmentPath --value 2>/dev/null || true)"
    [ -f "$frag" ] && ln -sf "$frag" /etc/systemd/system/display-manager.service || true
    systemctl daemon-reload
    systemctl start "$prev.service" 2>/dev/null || true
    ok "Geri dönüldü: $prev"
}

# ---------------------------------------------------------------------------
need_root "$@"

TARGET="${1:-all}"
case "$TARGET" in
    compositor) install_compositor ;;
    altay)      install_altay ;;
    bdm)        install_bdm ;;
    enable)     enable_bdm ;;
    revert)     revert_bdm ;;
    all)
        install_compositor
        install_altay
        install_bdm
        log ""
        ok "=== Tüm bileşenler kuruldu ==="
        log "Display manager'ı etkinleştirmek için:"
        log "  sudo bash $0 enable"
        ;;
    *) die "Bilinmeyen hedef: $TARGET (compositor | altay | bdm | enable | revert | all)" ;;
esac
