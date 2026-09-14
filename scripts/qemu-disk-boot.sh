#!/bin/bash
# 从已安装的磁盘引导 AetherOS（无光驱），用于持久化/引导验证。
# 磁盘：/home/aether/dist.raw（virtio → guest /dev/vda，分区 2 为持久化分区）
DISK=/home/aether/dist.raw
pkill -f qemu-system-x86_64 2>/dev/null; sleep 1
cd /home/aether
rm -f qemu-serial.log qmp.sock qemu.pid
setsid /usr/bin/qemu-system-x86_64 \
    -m 512 -smp 2 \
    -drive file="$DISK",format=raw,if=virtio \
    -boot c \
    -vga std -vnc 127.0.0.1:0 \
    -serial file:/home/aether/qemu-serial.log \
    -qmp unix:/home/aether/qmp.sock,server,nowait \
    < /dev/null > /home/aether/qemu-stdout.log 2>&1 &
echo $! > /home/aether/qemu.pid
echo "disk-boot pid=$(cat /home/aether/qemu.pid) disk=$DISK"
