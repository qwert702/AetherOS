#!/bin/bash
# 给 overlay /init 加执行位（rootfs 打包会保留权限），重建 ISO
set -e
chmod +x /home/aether/platform/overlay/init
rm -rf /home/aether/platform/build/output/build/buildroot-fs /home/aether/platform/build/output/images/rootfs.*
cd /home/aether/platform/build/buildroot-2024.02.1
source ~/.cargo/env
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1
echo "make exit: $?"
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
/home/aether/platform/build/output/host/bin/xorriso -indev /home/aether/aetheros-0.1-amd64.iso -find /init 2>/dev/null
