#!/bin/bash
set -e
cd /home/aether
mkdir -p platform/br2-external/configs
# 家目录下可能散落着之前传错的文件，归位
[ -f aetheros_defconfig ] && mv -f aetheros_defconfig platform/br2-external/configs/
sed -i 's/\r$//' platform/br2-external/configs/aetheros_defconfig platform/br2-external/external.desc 2>/dev/null || true
ls -la platform/br2-external/configs/
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output list-defconfigs 2>&1 | grep -i aether && echo "=== DETECTED ===" || echo "=== NOT DETECTED ==="
