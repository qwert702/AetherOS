#!/bin/bash
# 切换到 isolinux 引导：应用新 defconfig → 重打包 ISO
set -e
cd /home/aether
[ -f aetheros_defconfig ] && cp -f aetheros_defconfig platform/br2-external/configs/
cd platform && sed -i 's/\r$//' br2-external/configs/aetheros_defconfig
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output aetheros_defconfig > /dev/null
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build started"
