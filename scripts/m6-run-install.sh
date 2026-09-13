#!/bin/bash
# M6 安装器实测驱动：登录 guest → 跑安装器 → 查串口
cd "$(dirname "$0")/.."
MSYS2_ARG_CONV_EXCL="*" python scripts/vm.py sh 'python3 /home/aether/qmp-verify.py key a e t h e r minus i n s t a l l spc minus minus d i s k spc slash d e v slash v d a spc minus minus y e s; sleep 0.5; python3 /home/aether/qmp-verify.py key ret' 120
sleep 15
MSYS2_ARG_CONV_EXCL="*" python scripts/vm.py sh 'grep -a "aether-install" /home/aether/qemu-serial.log | tail -6' 30
