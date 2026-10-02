#!/bin/bash
# 内核 objtool 需要 libelf-dev；安装后重启构建
set -e
sudo apt install -y -q libelf-dev elfutils 2>&1 | tail -1
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build pid $!"
