#!/bin/bash
# 内核碎片 EOL 修复 + 内核全量重编（FB_VESA 生效）+ ISO 重打
set -e
source ~/.cargo/env
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
sed -i 's/\r$//' /home/aether/platform/br2-external/board/aether/linux.fragment
cat /home/aether/platform/br2-external/board/aether/linux.fragment
rm -rf "$OUT/build/linux-6.6.22"
cd "$BR"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" -j2 2>&1 | tail -2
grep -E "^CONFIG_FB_VESA=y|^CONFIG_FB=y" "$OUT/build/linux-6.6.22/.config" || echo "FB NOT IN KERNEL CONFIG!"
cp "$OUT/images/rootfs.iso9660" /home/aether/aetheros-0.1-amd64.iso
"$OUT/host/bin/xorriso" -indev /home/aether/aetheros-0.1-amd64.iso -find /boot 2>/dev/null
