#!/bin/bash
# 单独重建 iso9660 目标，观察引导布局选择
set -e
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
cd "$BR"
rm -f "$OUT/images/rootfs.iso9660"
rm -rf "$OUT/build/buildroot-fs/iso9660"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" rootfs-iso9660 2>&1 | grep -vE "^make\[[0-9]" | tail -8
echo "=== images ==="
ls "$OUT/images/" | grep -iE "isolinux|iso9660|rootfs"
echo "=== ISO 布局 ==="
"$OUT/host/bin/xorriso" -indev "$OUT/images/rootfs.iso9660" -find / 2>/dev/null | head -10
cp "$OUT/images/rootfs.iso9660" /home/aether/aetheros-0.1-amd64.iso
