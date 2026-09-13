#!/usr/bin/env python3
"""M6 安装器实测驱动：向 guest 注入安装命令并回读串口结果。"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import vm  # noqa: E402

KEYS = "a e t h e r minus i n s t a l l spc minus minus d i s k spc slash d e v slash v d a spc minus minus y e s"

c = vm.client()
for k in KEYS.split():
    vm.run(c, f"python3 /home/aether/qmp-verify.py key {k}", timeout=30)
    time.sleep(0.05)
vm.run(c, "python3 /home/aether/qmp-verify.py key ret", timeout=30)
print("install command injected")
time.sleep(15)
rc, out, err = vm.run(c, "grep -a aether-install /home/aether/qemu-serial.log | tail -8", timeout=60)
print(out, err)
c.close()
