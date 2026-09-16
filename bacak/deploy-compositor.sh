#!/bin/sh
# Deploy the freshly-built bacak-compositor.
# Run as root in a real terminal:  sudo sh /home/os/bacak/deploy-compositor.sh
#
# Safe sequence: stop the display manager, clear any stale compositor still
# holding the DRM master, install the new binary, then start fresh — so the new
# compositor comes up on a free card0. (The BDM daemon already tears its
# greeter compositor down on SIGTERM, but the explicit pkill is belt-and-braces.)
set -eu

REL=/home/os/bacak/target/release
ts=$(date +%Y%m%d-%H%M%S)

[ -x "$REL/bacak-compositor" ] || { echo "missing $REL/bacak-compositor — build it first"; exit 1; }

echo "[deploy] stopping display manager"
systemctl stop bacak-display-manager.service || true

echo "[deploy] clearing any stale compositor (frees the DRM master)"
pkill -9 -f '/usr/bin/bacak-compositor' || true

echo "[deploy] backing up current compositor (.bak-$ts)"
cp -a /usr/bin/bacak-compositor "/usr/bin/bacak-compositor.bak-$ts"

echo "[deploy] installing new compositor"
install -m755 "$REL/bacak-compositor" /usr/bin/bacak-compositor

echo "[deploy] starting display manager (relaunches the greeter on the new compositor)"
systemctl start bacak-display-manager.service

echo "DEPLOY_OK (backup suffix: $ts)"
