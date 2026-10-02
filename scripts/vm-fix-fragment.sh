#!/bin/bash
# 内核配置碎片路径修正 → 重编内核（FB_VESA）→ ISO 重打
set -e
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
cd "$BR"
sed -i 's|^BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILES=.*|BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILES="/home/aether/platform/br2-external/board/aether/linux.fragment"|' "$OUT/.config"
make O="$OUT" olddefconfig > /dev/null
grep -E "^CONFIG_FB_VESA=y|^CONFIG_FB=y" "$OUT/build/linux-6.6.22/.config" 2>/dev/null || true
rm -f "$OUT/build/linux-6.6.22/.stamp_built" "$OUT/images/bzImage"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" -j2 2>&1 | tail -2
# isolinux.cfg 去掉 INITRD 行（initramfs 已嵌入 bzImage）
sed -i '/INITRD/d' /home/aether/platform/br2-external/isolinux.cfg
rm -rf "$OUT/build/buildroot-fs" "$OUT/images/rootfs."*
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" rootfs-iso9660 -j2 2>&1 | tail -2
cp "$OUT/images/rootfs.iso9660" /home/aether/aetheros-0.1-amd64.iso
"$OUT/host/bin/xorriso" -indev /home/aether/aetheros-0.1-amd64.iso -find / 2>/dev/null | head -10
