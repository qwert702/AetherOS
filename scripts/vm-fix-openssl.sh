#!/bin/bash
# openssl 卡死修复：从官方站直取进 dl 缓存，重启构建
DL=/home/aether/platform/build/buildroot-2024.02.1/dl/openssl
mkdir -p "$DL"
F="$DL/openssl-3.2.1.tar.gz"
if [ ! -s "$F" ]; then
    pkill wget 2>/dev/null || true
    echo "==> 从 openssl.org 直取"
    curl -fL --retry 2 --max-time 400 -o "$F" \
        "https://www.openssl.org/source/openssl-3.2.1.tar.gz" && echo "OK $(du -h "$F" | cut -f1)"
fi
ls -la "$F" 2>/dev/null
pkill -f buildroot-2024.02.1 2>/dev/null || true
sleep 2
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build pid $!"
