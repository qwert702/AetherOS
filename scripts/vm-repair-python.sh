#!/bin/bash
# 排查磁盘满期间产生的 0 字节损坏文件，重建受影响的 host 包
set -e
BR=/home/aether/platform/build/buildroot-2024.02.1
OUT=/home/aether/platform/build/output
cd "$BR"
echo "=== 0 字节损坏文件清单 ==="
find "$OUT/host" "$OUT/build/syslinux-6.03" -type f -size 0 2>/dev/null | grep -v "\.lock$" | head -10
echo "=== 重建 host-python3 ==="
rm -rf "$OUT/build/host-python3-"* "$OUT/build/.host-python3"* 
find "$OUT/host" -lname "*python3*" -delete 2>/dev/null || true
rm -f "$OUT/host/bin/python3" "$OUT/host/bin/python3.11" "$OUT/host/bin/python3-config"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" -j2 2>&1 | tail -3
echo "=== python3 健康检查 ==="
echo "print(1)" | "$OUT/host/bin/python3" && echo "python3 OK"
