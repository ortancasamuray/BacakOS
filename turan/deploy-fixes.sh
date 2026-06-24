#!/bin/sh
# Deploy the freshly-built BDM daemon + greeter.
# Run as root in a real terminal:  sudo sh /home/os/turan/deploy-fixes.sh
#
# Safe sequence: stop the service, then clear any orphaned compositor/greeter
# from the previous session (they hold the DRM master and would make the new
# daemon crash-loop / flash the screen), then install + start fresh. From the
# NEXT restart on, the daemon's own SIGTERM handler tears the compositor down,
# so this manual clear is only needed for this transition.
set -eu

REL=/home/os/turan/target/release
ts=$(date +%Y%m%d-%H%M%S)

echo "[deploy] stopping service"
systemctl stop bacak-display-manager.service || true

echo "[deploy] clearing any stale compositor/greeter (frees the DRM master)"
# Match the binary path so the rustdesk helper running *as* user bacak-greeter
# (cmdline has 'bacak-greeter' but not '/usr/bin/bacak-greeter') is left alone.
pkill -9 -f '/usr/bin/bacak-compositor' || true
pkill -9 -f '/usr/bin/bacak-greeter'    || true

echo "[deploy] backing up current binaries (.bak-$ts)"
cp -a /usr/bin/bacak-display-manager "/usr/bin/bacak-display-manager.bak-$ts"
cp -a /usr/bin/bacak-greeter         "/usr/bin/bacak-greeter.bak-$ts"

echo "[deploy] installing new binaries"
install -m755 "$REL/bacak-display-manager" /usr/bin/bacak-display-manager
install -m755 "$REL/bacak-greeter"         /usr/bin/bacak-greeter

echo "[deploy] starting service"
systemctl start bacak-display-manager.service

echo "DEPLOY_OK (backup suffix: $ts)"
