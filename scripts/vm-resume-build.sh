#!/bin/bash
# 幂等续跑：完成则秒退，未完成则继续；结束时打印产物状态
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build2.log 2>&1
echo "make exit: $?"
tail -3 /home/aether/build2.log
ls -la /home/aether/platform/build/output/images/
