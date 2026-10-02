#!/bin/bash
# 强制重建文件系统引导层（isolinux），观察全过程输出
set -e
cd /home/aether/platform/build/buildroot-2024.02.1
ls output/build/ | grep -iE "syslinux|isolinux" || echo "NO syslinux built yet"
rm -rf output/build/buildroot-fs output/images/rootfs.*
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 2>&1 | tail -6
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
/home/aether/platform/build/output/host/bin/xorriso -indev /home/aether/aetheros-0.1-amd64.iso -find / 2>/dev/null | head -10
