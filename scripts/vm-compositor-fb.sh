#!/bin/bash
# 合成器（fbdev 后端）入列 → 重打 rootfs + ISO
set -e
source ~/.cargo/env
cd /home/aether
for c in aether-compositor; do
    cargo build --release --target x86_64-unknown-linux-musl -p "$c" 2>&1 | tail -1
    cp "target/x86_64-unknown-linux-musl/release/$c" "platform/overlay/usr/bin/$c"
    cp "target/x86_64-unknown-linux-musl/release/$c" "platform/build/output/target/usr/bin/$c"
done
ls -la platform/build/output/target/usr/bin/aether-compositor
rm -rf platform/build/output/build/buildroot-fs platform/build/output/images/rootfs.*
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output rootfs-iso9660 -j2 2>&1 | tail -2
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
/home/aether/platform/build/output/host/bin/xorriso -indev /home/aether/aetheros-0.1-amd64.iso -find / 2>/dev/null
