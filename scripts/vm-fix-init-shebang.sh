#!/bin/bash
# 修 /init：shebang 首行 + LF + 执行位，重建 ISO
set -e
chmod +x /home/aether/platform/overlay/init
sed -i '1!b' /dev/null 2>/dev/null || true
sed -i 's/\r$//' /home/aether/platform/overlay/init
head -1 /home/aether/platform/overlay/init
rm -rf /home/aether/platform/build/output/build/buildroot-fs /home/aether/platform/build/output/images/rootfs.* /home/aether/platform/build/output/target/init
cd /home/aether/platform/build/buildroot-2024.02.1
source ~/.cargo/env
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1
echo "make exit: $?"
ls -la /home/aether/platform/build/output/target/init
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
