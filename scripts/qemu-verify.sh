#!/bin/bash
# 启动 AetherOS ISO 于 QEMU（后台，CWD=/home/aether 以便 pmemsave 用相对路径）
VGA="${1:-std}"
pkill -f qemu-system-x86_64 2>/dev/null; sleep 1
cd /home/aether
rm -f qemu-serial.log qmp.sock qemu.pid vram.bin screen.ppm
setsid /usr/bin/qemu-system-x86_64 \
    -m 512 -smp 2 \
    -cdrom /home/aether/aetheros-0.1-amd64.iso -boot d \
    -vga "$VGA" -vnc 127.0.0.1:0 \
    -serial file:/home/aether/qemu-serial.log \
    -qmp unix:/home/aether/qmp.sock,server,nowait \
    < /dev/null > /home/aether/qemu-stdout.log 2>&1 &
echo $! > /home/aether/qemu.pid
echo "QEMU pid=$(cat /home/aether/qemu.pid) vga=$VGA vnc=:5900 cwd=$(readlink /proc/$!/cwd 2>/dev/null)"
