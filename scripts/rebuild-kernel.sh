#!/bin/bash
# 内核重编（linux.fragment 变更后）：重配 → 清 stamp → 全量重编 → 复用 rebuild-m4 出 ISO
# 在构建机 /home/aether 内执行；约 15-25 分钟。
set -e
source ~/.cargo/env
cd /home/aether
OUT=/home/aether/platform/build/output

echo "==> [1/2] Buildroot 重配并重编内核"
cd platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" aetheros_defconfig
rm -f "$OUT"/build/linux-*/.stamp_configured "$OUT"/build/linux-*/.stamp_built \
      "$OUT"/build/linux-*/.config "$OUT"/images/bzImage
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" -j2 > /home/aether/rk.log 2>&1

echo "==> [2/2] 复用 rebuild-m4.sh 出 ISO"
cd /home/aether
bash rebuild-m4.sh
echo KERNEL-REBUILD-DONE
