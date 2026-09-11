#!/bin/bash
# 修复 Windows CRLF 换行符：Buildroot 对 CRLF 敏感
set -e
cd /home/aether/platform
sed -i 's/\r$//' br2-external/external.desc br2-external/Config.in \
    br2-external/external.mk br2-external/configs/aetheros_defconfig \
    build-iso.sh overlay/init || true
file br2-external/external.desc br2-external/configs/aetheros_defconfig
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output list-defconfigs 2>&1 | grep -i aether || echo "NOT DETECTED"
