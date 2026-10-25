#!/usr/bin/env bash
# build-debs.sh — tüm BacakOS bileşenlerini deb paketi olarak derler.
#
# Kullanım:
#   sudo bash setup-deps.sh          # bir kez: derleme + çalışma zamanı bağımlılıkları
#   sudo bash setup-deps.sh --cross  # bir kez: arm64/riscv64 çapraz derleme ortamı
#   bash build-debs.sh               # yalnız amd64 — root gerekmez
#   bash build-debs.sh arm64 riscv64 # verilen mimariler, sırayla
#   BACAK_CROSS_TARGET_DIR=/mnt/disk2/bacak-target bash build-debs.sh arm64
#                                    # çapraz derleme çıktısı başka diske
#   ls dist/<mimari>/                # oluşan paketler (dist/amd64, dist/arm64, dist/riscv64)
#
# Kurmak için (hedef makinede):
#   sudo apt install ./dist/amd64/*.deb
#   — veya tek paketle (bağımlılık listesi üzerinden aynı şeyi yapar):
#   sudo apt install ./dist/amd64/bacakos_*.deb
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

# ---------------------------------------------------------------------------
# Mimari seçimi: her mimarinin paketleri dist/<mimari>/ altına yazılır.
# Ana makine mimarisi --target'sız derlenir (target/release); diğerleri Debian
# multiarch çapraz gcc'si ve /usr/lib/<üçlü>/pkgconfig ile.
# ---------------------------------------------------------------------------
HOST_ARCH="$(dpkg --print-architecture)"
ARCHES=("$@")
[ ${#ARCHES[@]} -gt 0 ] || ARCHES=("$HOST_ARCH")

TARGET_ARGS=()
OUT=""

# setup_arch <deb-mimarisi>: TARGET_ARGS + OUT'u ayarlar, çapraz ortamı export eder.
setup_arch() {
    local arch="$1" triple gnu
    case "$arch" in
        amd64)   triple=x86_64-unknown-linux-gnu;    gnu=x86_64-linux-gnu ;;
        arm64)   triple=aarch64-unknown-linux-gnu;   gnu=aarch64-linux-gnu ;;
        riscv64) triple=riscv64gc-unknown-linux-gnu; gnu=riscv64-linux-gnu ;;
        *) die "bilinmeyen mimari: $arch (amd64 | arm64 | riscv64)" ;;
    esac
    OUT="$DIST/$arch"
    mkdir -p "$OUT"
    unset LD
    if [ "$arch" = "$HOST_ARCH" ]; then
        TARGET_ARGS=()
        return
    fi
    command -v "$gnu-gcc" >/dev/null \
        || die "$gnu-gcc yok — önce: sudo bash setup-deps.sh --cross"
    rustup target list --installed | grep -qx "$triple" \
        || die "Rust hedefi $triple yok — önce: sudo bash setup-deps.sh --cross"
    TARGET_ARGS=(--target "$triple")
    local t="${triple//-/_}"
    # tr + C yerel ayarı: ${t^^} Türkçe ortamda i → İ yapar.
    export "CARGO_TARGET_$(LC_ALL=C tr a-z A-Z <<<"$t")_LINKER=$gnu-gcc"
    export "CC_$t=$gnu-gcc" "CXX_$t=$gnu-g++" "AR_$t=$gnu-ar"
    export "PKG_CONFIG_LIBDIR_$t=/usr/lib/$gnu/pkgconfig:/usr/share/pkgconfig"
    export "PKG_CONFIG_SYSROOT_DIR_$t=/"
    export "BINDGEN_EXTRA_CLANG_ARGS_$t=--target=$gnu"
    export PKG_CONFIG_ALLOW_CROSS=1

    # mupdf-sys make'e CC/CXX geçirir ama LD'yi geçirmez; gömülü fontları
    # `$(LD) -r -b binary` ile nesneye çevirirken make'in varsayılanı olan ana
    # makine ld'si x86_64 nesne üretir ve hedef bağlayıcı bunları reddeder.
    export LD="$gnu-ld"

    # ffmpeg-sys-next sürüm/özellik denetimini ana makine gcc'siyle derleyip
    # çalıştırır; pkg-config'in -I/usr/include/<üçlü>'sü oraya glibc'nin hedef
    # başlıklarını da sokar ve derleme kırılır. FFMPEG_DIR'e yalnız libav*
    # başlıklarını içeren bir include dizini verilir.
    local ff="${XDG_CACHE_HOME:-$HOME/.cache}/bacak-cross/ffmpeg-$arch"
    mkdir -p "$ff/include"
    ln -sfn "/usr/lib/$gnu" "$ff/lib"
    for h in /usr/include/"$gnu"/libav* /usr/include/"$gnu"/libsw*; do
        ln -sfn "$h" "$ff/include/$(basename "$h")"
    done
    export FFMPEG_DIR="$ff"
}

# BACAK_CROSS_TARGET_DIR verilirse çapraz derleme çıktısı ana diski doldurmasın
# diye oraya yazılır (çalışma alanı başına bir alt dizin: bacak, kur, …).
use_cross_target_dir() {
    if [ -n "${BACAK_CROSS_TARGET_DIR:-}" ] && [ ${#TARGET_ARGS[@]} -gt 0 ]; then
        local ws="${PWD#"$HERE"/}"
        export CARGO_TARGET_DIR="$BACAK_CROSS_TARGET_DIR/${ws//\//_}"
    fi
}

cbuild() { use_cross_target_dir; cargo build --release "${TARGET_ARGS[@]}" "$@"; }
cdeb()   { use_cross_target_dir; cargo deb --no-build "${TARGET_ARGS[@]}" -o "$OUT" "$@"; }

build_arch() {
local ARCH="$1"
setup_arch "$ARCH"
log "##### mimari: $ARCH → $OUT #####"

# ---------------------------------------------------------------------------
# bacak workspace: compositor, plugin'ler, CLI
# ---------------------------------------------------------------------------
log "=== bacak workspace derleniyor ==="
(
    cd "$BACAK_SRC"

    log "compositor derleniyor (udev feature)…"
    cbuild -p bacak-compositor --features udev

    log "CLI derleniyor…"
    cbuild -p bacak-cli

    log "bacak-compositor deb oluşturuluyor"
    cdeb -p bacak-compositor

    log "bacak-plugin-audio deb oluşturuluyor"
    cdeb -p bacak-plugin-audio

    log "bacak-plugin-network deb oluşturuluyor"
    cdeb -p bacak-plugin-network

    log "bacak-plugin-desktop-settings deb oluşturuluyor"
    cdeb -p bacak-plugin-desktop-settings

    log "bacak-icons deb oluşturuluyor"
    cdeb -p bacak-icons

    log "bacak-desktop-defaults deb oluşturuluyor"
    cdeb -p bacak-desktop-defaults

    log "bacak-grub-theme deb oluşturuluyor"
    cdeb -p bacak-grub-theme

    log "bacak-plymouth-theme deb oluşturuluyor"
    cdeb -p bacak-plymouth-theme

    log "bacak-plugin-uzakel deb oluşturuluyor"
    cdeb -p bacak-plugin-uzakel

    log "bacak (CLI) deb oluşturuluyor"
    cdeb -p bacak-cli
)

# ---------------------------------------------------------------------------
# turan workspace: display manager, greeter, session launcher
# ---------------------------------------------------------------------------
log "=== turan workspace derleniyor ==="
(
    cd "$TURAN_SRC"

    log "bacak-display-manager derleniyor (system-pam)…"
    cbuild -p bacak-display-manager --features system-pam

    log "bacak-greeter derleniyor (gui)…"
    cbuild -p bacak-greeter --features gui

    log "bacak-session-launcher derleniyor…"
    cbuild -p bacak-session-launcher

    log "bacak-display-manager deb oluşturuluyor"
    cdeb -p bacak-display-manager
)

# ---------------------------------------------------------------------------
# bacakonay: Turan girişi için iki adımlı doğrulama (bacakonay CLI + PAM modülü)
# ---------------------------------------------------------------------------
log "=== bacakonay derleniyor ==="
BACAKONAY_SRC="$HERE/bacakonay/linux"
(
    cd "$BACAKONAY_SRC"

    log "bacakonay derleniyor (CLI + pam_bacakonay.so)…"
    cbuild

    log "bacakonay deb oluşturuluyor"
    cdeb -p bacakonay-cli
)

# ---------------------------------------------------------------------------
# uzakyonetim: filo yönetimi — ajan (her BacakOS) + sunucu (merkez makine)
# ---------------------------------------------------------------------------
log "=== uzakyonetim derleniyor ==="
UZAKYONETIM_SRC="$HERE/uzakyonetim"
(
    cd "$UZAKYONETIM_SRC"

    log "uzakyonetim derleniyor (ajan + sunucu)…"
    cbuild

    log "uzakyonetim-ajan deb oluşturuluyor"
    cdeb -p uzy-ajan

    log "uzakyonetim-sunucu deb oluşturuluyor"
    cdeb -p uzy-sunucu
)

# ---------------------------------------------------------------------------
# uzakel-daemon: Uzakel telefon uygulamasının BacakOS tarafı (girdi + dosya)
# ---------------------------------------------------------------------------
log "=== uzakel-daemon derleniyor ==="
UZAKEL_SRC="$HERE/uzakel/uzakel-android/daemon"
(
    cd "$UZAKEL_SRC"

    log "uzakel-daemon derleniyor…"
    cbuild

    log "uzakel-daemon deb oluşturuluyor"
    cdeb
)

# ---------------------------------------------------------------------------
# altay: dosya yöneticisi
# ---------------------------------------------------------------------------
log "=== altay derleniyor ==="
(
    cd "$ALTAY_SRC"

    log "altay derleniyor…"
    cbuild

    log "altay deb oluşturuluyor"
    cdeb
)

# ---------------------------------------------------------------------------
# Belgeler: birleşik belge ve resim görüntüleyici
# ---------------------------------------------------------------------------
log "=== bacak-belge derleniyor ==="
BELGELER_SRC="$HERE/Belgeler"
(
    cd "$BELGELER_SRC"

    log "bacak-belge derleniyor…"
    cbuild -p bacak-belge

    log "bacak-belge deb oluşturuluyor"
    cdeb -p bacak-belge
)

# ---------------------------------------------------------------------------
# tahta: dijital beyaz tahta
# ---------------------------------------------------------------------------
log "=== tahta derleniyor ==="
TAHTA_SRC="$HERE/tahta"
(
    cd "$TAHTA_SRC"

    log "tahta derleniyor…"
    cbuild

    log "tahta deb oluşturuluyor"
    cdeb
)

# ---------------------------------------------------------------------------
# kur: sistem yükleyici
# ---------------------------------------------------------------------------
log "=== kur derleniyor ==="
KUR_SRC="$HERE/kur"
(
    cd "$KUR_SRC"

    log "kur derleniyor…"
    cbuild

    log "kur deb oluşturuluyor"
    cdeb
)

# ---------------------------------------------------------------------------
# eta-touchdrv: Fatih akıllı tahta dokunmatik sürücüsü (Pardus kaynağı, DKMS)
# ---------------------------------------------------------------------------
# İç içe ayrı bir git deposu (bacakos'ta takip edilmez). Kaynak ağacı kirletmemek
# ve dpkg-buildpackage'ın ../ altına yazdığı dosyalar depo köküne düşmesin diye
# geçici bir kopyada derlenir. debhelper + dh-dkms gerekir (setup-deps.sh).
ETA_SRC="$HERE/eta-touchdrv"
if [ "$ARCH" != amd64 ]; then
    log "eta-touchdrv ATLANDI: yalnız amd64 (Vestel sunucu programları x86 ikili)"
elif [ -d "$ETA_SRC/debian" ]; then
    log "=== eta-touchdrv derleniyor ==="
    if command -v dh >/dev/null && [ -x /usr/bin/dh_dkms ]; then
        ETA_TMP=$(mktemp -d)
        cp -r "$ETA_SRC" "$ETA_TMP/eta-touchdrv"
        rm -rf "$ETA_TMP/eta-touchdrv/.git"
        (cd "$ETA_TMP/eta-touchdrv" && dpkg-buildpackage -b -us -uc)
        cp "$ETA_TMP"/eta-touchdrv_*_amd64.deb "$OUT/"
        rm -rf "$ETA_TMP"
        log "ISO'ya almak için: cp $OUT/eta-touchdrv_*_amd64.deb Buildeba/config/packages.chroot/"
    else
        log "eta-touchdrv ATLANDI: debhelper/dh-dkms yok (sudo bash setup-deps.sh)"
    fi
fi

# ---------------------------------------------------------------------------
# bacakos: içeriksiz meta-paket — tek `apt install` ile tüm bileşenleri kurar
# ---------------------------------------------------------------------------
log "=== bacakos meta-paketi oluşturuluyor ==="
(
    cd "$BACAK_SRC"
    cdeb -p bacakos
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
ok "=== $ARCH paketleri hazır: $OUT ==="
}

for a in "${ARCHES[@]}"; do
    build_arch "$a"
done

echo ""
for a in "${ARCHES[@]}"; do
    ok "=== $a ==="
    ls -lh "$DIST/$a"/*.deb
done
echo ""
log "Hedef makinede kurmak için:"
log "  sudo apt install $DIST/<mimari>/*.deb"
