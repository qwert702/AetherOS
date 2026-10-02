#!/bin/bash
# 先应用 defconfig，再启动 AetherOS ISO 构建（后台，日志 build.log）
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output aetheros_defconfig
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build pid $!"
sleep 5
tail -2 /home/aether/build.log
