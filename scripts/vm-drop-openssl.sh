#!/bin/bash
# 1) gcc 源码包入位  2) 去掉内核的 host-openssl 依赖（极简内核无模块签名）
# 3) 重启构建
set -e
DL=/home/aether/platform/build/buildroot-2024.02.1/dl
mkdir -p "$DL/gcc" "$DL/openssl"
cd /home/aether
[ -f gcc-12.3.0.tar.xz ] && mv -f gcc-12.3.0.tar.xz "$DL/gcc/" || true
ls -la "$DL/gcc/"
# 清掉卡死的残留
pkill -f buildroot-2024.02.1 2>/dev/null || true
pkill wget 2>/dev/null || true
sleep 2
CFG=/home/aether/platform/build/output/.config
sed -i '/BR2_LINUX_KERNEL_NEEDS_HOST_OPENSSL/d' "$CFG"
cd /home/aether/platform/build/buildroot-2024.02.1
make O=/home/aether/platform/build/output olddefconfig > /dev/null
grep -c HOST_OPENSSL "$CFG" || echo "openssl dep removed"
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build pid $!"
