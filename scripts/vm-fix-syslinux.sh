#!/bin/bash
# 应用修正后的 defconfig，验证 syslinux 选择链，重建 ISO
set -e
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
cd "$BR"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" aetheros_defconfig > /dev/null
echo "--- 选择链 ---"
grep -E "^BR2_TARGET_SYSLINUX=y|^BR2_TARGET_SYSLINUX_ISOLINUX=y|^BR2_TARGET_ROOTFS_ISO9660_ISOLINUX=y" "$OUT/.config" || echo "选择链缺失!"
rm -rf "$OUT/build/buildroot-fs" "$OUT/images/rootfs."*
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" -j2 2>&1 | tail -2
cp "$OUT/images/rootfs.iso9660" /home/aether/aetheros-0.1-amd64.iso
"$OUT/host/bin/xorriso" -indev /home/aether/aetheros-0.1-amd64.iso -find / 2>/dev/null | head -10
