#!/bin/bash
# 归位 defconfig + grub 内嵌配置 + 修 EOL + 重编 grub2 + 重新打包 ISO
set -e
cd /home/aether
[ -f aetheros_defconfig ] && cp -f aetheros_defconfig platform/br2-external/configs/
[ -f grub-embedded.cfg ] && cp -f grub-embedded.cfg platform/br2-external/
grep -q iso9660 platform/br2-external/configs/aetheros_defconfig && echo "defconfig ok"
cd platform
sed -i 's/\r$//' br2-external/configs/aetheros_defconfig br2-external/grub-embedded.cfg
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output aetheros_defconfig > /dev/null
grep iso9660 /home/aether/platform/build/output/.config | head -1 | cut -c1-90
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output grub2-rebuild > /dev/null 2>&1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1
echo "make exit: $?"
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
ls -la /home/aether/aetheros-0.1-amd64.iso
