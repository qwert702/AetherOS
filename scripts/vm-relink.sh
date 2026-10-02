#!/bin/bash
# 强制内核重链接以嵌入最新 initramfs（含 24bpp 合成器），重出 ISO
set -e
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
cd "$BR"
rm -f "$OUT/build/linux-6.6.22/.stamp_built" "$OUT/images/bzImage" "$OUT/images/rootfs.iso9660"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" -j2 2>&1 | tail -2
cp "$OUT/images/rootfs.iso9660" /home/aether/aetheros-0.1-amd64.iso
ls -la /home/aether/aetheros-0.1-amd64.iso | awk '{print $5, $8}'
