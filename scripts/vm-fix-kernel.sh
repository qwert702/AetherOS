#!/bin/bash
# 内核源码包改走 TUNA 镜像：停掉慢速下载 → 直取内核包 → 重启构建
set -e
pkill -f "buildroot-2024.02.1" 2>/dev/null || true
pkill wget 2>/dev/null || true
sleep 2
mkdir -p /home/aether/platform/build/buildroot-2024.02.1/dl/linux
cd /home/aether/platform/build/buildroot-2024.02.1/dl/linux
echo "==> TUNA 下载 linux-6.6.22.tar.xz"
curl -L --retry 3 -o linux-6.6.22.tar.xz \
    "https://mirrors.tuna.tsinghua.edu.cn/kernel/v6.x/linux-6.6.22.tar.xz"
ls -la
echo "==> 重启构建"
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build pid $!"
