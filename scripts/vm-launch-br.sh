#!/bin/bash
# 在 Build VM 内启动 Buildroot 完整重建（幂等，可反复调用）
cd /home/aether/platform/build/buildroot-2024.02.1
setsid bash -c 'make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 && echo BR-ALL-DONE' < /dev/null > /home/aether/br-rebuild.log 2>&1 &
echo launched
