#!/bin/bash
# 把自定义 isolinux.cfg（vga=0x318）接进 ISO 打包配置
set -e
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
grep -q "br2-external/isolinux.cfg" "$OUT/.config" || echo 'BR2_TARGET_ROOTFS_ISO9660_BOOT_MENU="/home/aether/platform/br2-external/isolinux.cfg"' >> "$OUT/.config"
cd "$BR"
make O="$OUT" olddefconfig > /dev/null
grep ISO9660_BOOT_MENU "$OUT/.config" | cut -c1-110
rm -rf "$OUT/build/buildroot-fs" "$OUT/images/rootfs."*
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" rootfs-iso9660 -j2 2>&1 | tail -1
cp "$OUT/images/rootfs.iso9660" /home/aether/aetheros-0.1-amd64.iso
"$OUT/host/bin/xorriso" -osirrox on -indev /home/aether/aetheros-0.1-amd64.iso -extract /isolinux/isolinux.cfg /tmp/isocfg.txt > /dev/null 2>&1
cat /tmp/isocfg.txt
