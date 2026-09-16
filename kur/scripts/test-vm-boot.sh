#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>
#
# test-vm-boot.sh — kur ile kurulan imajı QEMU'da önyükler ve PNG ekran
# görüntüleri alır. kur, kernel'i seri konsola yönlendirmediği için çıktı seri
# porttan değil, QEMU monitörünün `screendump` komutuyla VGA çerçeve tamponundan
# yakalanır. QEMU 10 doğrudan PNG üretebildiği için dönüştürme gerekmez.
#
# Kullanım:  bash scripts/test-vm-boot.sh [bios|uefi] [imaj-yolu]
#
# root GEREKMEZ (KVM erişimi ACL ile verilmişse). Çıktı: target/vm/shot-$FW-*.png
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
FW="${1:-bios}"
case "$FW" in bios|uefi) ;; *) echo "firmware bios veya uefi olmalı"; exit 1;; esac
IMG="${2:-$HERE/target/vm/bacakos-test-$FW.img}"
OUT="$HERE/target/vm"
MON_PORT=55557

[ -f "$IMG" ] || { echo "imaj yok: $IMG (önce test-vm-install.sh $FW)"; exit 1; }

ACCEL="tcg"; [ -w /dev/kvm ] && ACCEL="kvm"
echo "[boot] firmware=$FW hızlandırma=$ACCEL"

# UEFI: OVMF gerekir. CODE salt-okunur pflash; VARS'ın yazılabilir bir kopyası
# (misafirin NVRAM'i) her koşuda sıfırlanır ki test tekrarlanabilir olsun.
FW_ARGS=()
if [ "$FW" = "uefi" ]; then
    OVMF_CODE="/usr/share/OVMF/OVMF_CODE_4M.fd"
    OVMF_VARS_SRC="/usr/share/OVMF/OVMF_VARS_4M.fd"
    [ -f "$OVMF_CODE" ] || { echo "OVMF yok: $OVMF_CODE (apt install ovmf)"; exit 1; }
    VARS="$OUT/OVMF_VARS-$FW.fd"
    cp -f "$OVMF_VARS_SRC" "$VARS"
    FW_ARGS=(
        -drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_CODE"
        -drive "if=pflash,format=raw,unit=1,file=$VARS"
    )
fi

# -snapshot: imaja yazma, geçici katmana yaz — imaj root'a ait ve salt-okunur
# erişimimiz yeterli. QMP kullanılır, HMP değil: HMP `screendump -f png` bu QEMU
# sürümünde PNG üretmiyor; QMP format argümanını net alır ve hata döndürür.
qemu-system-x86_64 \
    -machine "accel=$ACCEL" \
    -m 2048 \
    "${FW_ARGS[@]}" \
    -drive file="$IMG",format=raw \
    -snapshot \
    -display none \
    -qmp "tcp:127.0.0.1:$MON_PORT,server,nowait" \
    -serial file:"$OUT/serial-$FW.log" &
QEMU_PID=$!
trap 'kill $QEMU_PID 2>/dev/null || true' EXIT

# QMP üzerinden PNG ekran görüntüsü al. İlk çağrı capabilities el sıkışmasını
# da yapar; her çağrı yeni bağlantı açar (basit ve durum tutmaz).
shot() {
    python3 - "$MON_PORT" "$1" <<'PY'
import socket, json, sys, time
port, path = int(sys.argv[1]), sys.argv[2]
try:
    s = socket.create_connection(("127.0.0.1", port), timeout=10)
except OSError as e:
    print(f"  [qmp bağlanamadı: {e}]"); raise SystemExit(1)
f = s.makefile("rwb", buffering=0)
f.readline()                                   # greeting
f.write(b'{"execute":"qmp_capabilities"}\n'); f.readline()
cmd = {"execute": "screendump", "arguments": {"filename": path, "format": "png"}}
f.write((json.dumps(cmd) + "\n").encode())
resp = json.loads(f.readline())
s.close()
raise SystemExit(0 if "return" in resp else 1)
PY
}

# Önyükleme kilometre taşlarında kareler yakala: GRUB menüsü, çekirdek, giriş.
prev=0
for t in 5 25 50 80; do
    sleep $((t - prev)); prev=$t
    png="$OUT/shot-$FW-${t}s.png"
    if shot "$png"; then echo "[boot] ${t}s → $png"; else echo "[boot] uyarı: ${t}s karesi alınamadı"; fi
done

echo "[boot] tamamlandı. Kareler: $OUT/shot-$FW-*.png"
echo "[boot] seri günlük (varsa): $OUT/serial-$FW.log"
