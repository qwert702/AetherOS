#!/bin/bash
# 续跑：目录补齐 → defconfig 片 → 重编
set -e
source ~/.cargo/env
cd /home/aether
mkdir -p platform/br2-external/board/aether platform/overlay/usr/bin platform/build/output/target/usr/bin
[ -f linux.fragment ] && cp -f linux.fragment platform/br2-external/board/aether/linux.fragment
[ -f isolinux.cfg ] && cp -f isolinux.cfg platform/br2-external/isolinux.cfg
cd platform && sed -i 's/\r$//' br2-external/board/aether/linux.fragment br2-external/isolinux.cfg
cd /home/aether
grep -q CONFIG_FRAGMENT_FILE /home/aether/platform/br2-external/configs/aetheros_defconfig || \
    echo 'BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILE="$(BR2_EXTERNAL_AETHER_PATH)/board/aether/linux.fragment"' >> /home/aether/platform/br2-external/configs/aetheros_defconfig
sed -i 's/\r$//' /home/aether/platform/br2-external/configs/aetheros_defconfig
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output aetheros_defconfig > /dev/null
rm -f /home/aether/platform/build/output/build/linux-6.6.22/.stamp_built
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1
echo "make exit: $?"
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
/home/aether/platform/build/output/host/bin/xorriso -indev /home/aether/aetheros-0.1-amd64.iso -find /usr/bin 2>/dev/null | head -6
ls -la /home/aether/aetheros-0.1-amd64.iso
