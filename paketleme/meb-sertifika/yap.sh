#!/bin/sh
# meb-sertifika paketini oluşturur: sh yap.sh <çıktı-dizini>
set -eu
SURUM=0.1.0-1
BURADA=$(cd "$(dirname "$0")" && pwd)
CIKTI=${1:?çıktı dizini}
# Sertifika değişmemiş olmalı (parmak izi copyright'takiyle aynı).
openssl x509 -in "$BURADA/kok/usr/share/ca-certificates/meb/meb-fatihca.crt" -noout -fingerprint -sha256 \
  | grep -q 'C4:F8:04:D0:93:BA:78:5D:EB:C6:9B:59:17:F2:99:FE:6E:A9:E8:AA:2E:66:83:E8:3F:4D:C9:CD:51:C0:8E:75' \
  || { echo "meb-sertifika: sertifika parmak izi beklenenden farklı" >&2; exit 1; }
GECICI=$(mktemp -d)
trap 'rm -rf "$GECICI"' EXIT
cp -a "$BURADA/kok/." "$GECICI/"
mkdir -p "$GECICI/DEBIAN"
sed "s/@SURUM@/$SURUM/" "$BURADA/debian/control" > "$GECICI/DEBIAN/control"
install -m 755 "$BURADA/debian/postinst" "$BURADA/debian/postrm" "$GECICI/DEBIAN/"
find "$GECICI" -type d -exec chmod 755 {} +
find "$GECICI/usr" -type f -exec chmod 644 {} +
mkdir -p "$CIKTI"
dpkg-deb --root-owner-group -Zxz --build "$GECICI" "$CIKTI/meb-sertifika_${SURUM}_all.deb" >/dev/null
echo "$CIKTI/meb-sertifika_${SURUM}_all.deb"
