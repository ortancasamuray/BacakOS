#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>
#
# test-vm-install.sh — kur'u gerçek bir disk imajına başlıksız kurar.
#
# GUI yerine kur'un `KUR_HEADLESS` yolunu kullanır: aynı motor, aynı komutlar,
# pencere yok. Hedef fiziksel bir disk değil, bir NBD aygıtına bağlanmış seyrek
# (sparse) imaj dosyasıdır — hiçbir gerçek disk silinmez.
#
# NBD kullanılır, loop DEĞİL: kur bir loop aygıtını hedef olarak kabul etmez
# (lsblk onu type=loop gösterir, kur yalnızca type=disk olanları listeler — bu
# canlı imajın squashfs'ini hedef olmaktan korur). qemu-nbd ise imajı type=disk
# bir aygıt olarak sunar, böylece kur onu gerçek bir disk gibi görür ve hiçbir
# kod değişikliği gerekmez.
#
# Kullanım:
#   sudo bash scripts/test-vm-install.sh [bios|uefi] [imaj-yolu]
#
# firmware varsayılanı: bios. Her firmware ayrı imaja kurulur, böylece BIOS ve
# UEFI düzenleri karışmaz. Kurulum bitince imaj test-vm-boot.sh ile önyüklenir.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
KUR_BIN="$HERE/target/release/kur"
FW="${1:-bios}"
case "$FW" in bios|uefi) ;; *) echo "firmware bios veya uefi olmalı"; exit 1;; esac
IMG="${2:-$HERE/target/vm/bacakos-test-$FW.img}"
IMG_SIZE_GB=25

c_b=$'\033[1;34m'; c_g=$'\033[1;32m'; c_r=$'\033[1;31m'; c_0=$'\033[0m'
log() { printf '%s[test]%s %s\n' "$c_b" "$c_0" "$*"; }
ok()  { printf '%s[test] %s%s\n' "$c_g" "$*" "$c_0"; }
die() { printf '%s[test] HATA: %s%s\n' "$c_r" "$*" "$c_0" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "root gerekli: sudo bash $0"
[ -x "$KUR_BIN" ] || die "kur derlenmemiş: $KUR_BIN yok (cargo build --release)"

# --- Bağımlılık: kur debootstrap'ı çağırır, host'ta olmalı.
if ! command -v debootstrap >/dev/null; then
    log "debootstrap kuruluyor…"
    apt-get update && apt-get install -y debootstrap
fi

# --- İmaj + NBD aygıtı
mkdir -p "$(dirname "$IMG")"
log "seyrek $IMG_SIZE_GB GiB imaj oluşturuluyor: $IMG"
rm -f "$IMG"
truncate -s "${IMG_SIZE_GB}G" "$IMG"

# nbd çekirdek modülü; max_part bölüm düğümlerinin (/dev/nbd0p1) oluşmasını sağlar.
modprobe nbd max_part=16 2>/dev/null || die "nbd modülü yüklenemedi"

# İlk boş nbd aygıtını bul.
NBD=""
for dev in /dev/nbd{0..15}; do
    if ! losetup -a 2>/dev/null | grep -q "$dev" && [ "$(blockdev --getsize64 "$dev" 2>/dev/null || echo 0)" = "0" ]; then
        NBD="$dev"; break
    fi
done
[ -n "$NBD" ] || die "boş nbd aygıtı bulunamadı"

qemu-nbd --connect="$NBD" --format=raw "$IMG" || die "qemu-nbd bağlanamadı"
udevadm settle 2>/dev/null || true
ok "nbd aygıtı: $NBD"

# lsblk gerçekten type=disk gösteriyor mu? kur bunu gerektirir.
NBD_TYPE="$(lsblk -ndo TYPE "$NBD" 2>/dev/null || echo '?')"
[ "$NBD_TYPE" = "disk" ] || die "$NBD tipi 'disk' değil ('$NBD_TYPE') — kur onu hedef görmez"

# Ne olursa olsun aygıtı çöz ve yarım bağlamaları temizle.
cleanup() {
    log "temizleniyor…"
    umount -R -l /mnt/kur-target 2>/dev/null || true
    # NBD'yi koparmadan önce sayfa önbelleğini imaja boşalt; kur kendi
    # cleanup'ında da yapıyor, bu ikinci güvenlik katmanı.
    sync
    qemu-nbd --disconnect "$NBD" 2>/dev/null || true
}
trap cleanup EXIT

# --- Başlıksız kurulum.
#   bios → KUR_UEFI=0: GPT + BIOS boot bölümü + grub-pc
#   uefi → KUR_UEFI=1: GPT + ESP + grub-efi. KUR_GRUB_REMOVABLE=1 grub'u fallback
#          yola kurar ve NVRAM'e dokunmaz — taze OVMF'nin bulması ve host boot
#          menüsünün kirlenmemesi için (bkz. stages.rs bootloader).
if [ "$FW" = "uefi" ]; then
    UEFI_FLAG=1; REMOVABLE=1
else
    UEFI_FLAG=0; REMOVABLE=0
fi

log "kur başlıksız kurulumu başlatıyor (firmware=$FW, hedef $NBD)…"
echo
KUR_HEADLESS=1 \
KUR_TARGET_DISK="$NBD" \
KUR_UEFI="$UEFI_FLAG" \
KUR_GRUB_REMOVABLE="$REMOVABLE" \
KUR_HOSTNAME=bacakos-vm \
KUR_USERNAME=deneme \
KUR_PASSWORD='DenemeParola1!' \
KUR_LOCALE=tr_TR.UTF-8 \
KUR_KEYMAP=tr \
KUR_TIMEZONE=Europe/Istanbul \
RUST_LOG=info \
    "$KUR_BIN"

echo
ok "kurulum bitti ($FW). İmaj önyüklemeye hazır: $IMG"
log "QEMU ile önyüklemek için:  bash scripts/test-vm-boot.sh $FW"
