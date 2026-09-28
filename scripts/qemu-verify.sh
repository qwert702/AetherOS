#!/bin/bash
# 启动 AetherOS ISO 于 QEMU（后台，CWD=/home/aether 以便 pmemsave 用相对路径）
#
# MEM 环境变量可覆盖内存（默认 512）。**长时间稳定性观测请用 MEM=1024** ——
# 512MB 下 guest 只剩几 MB 可用，aether-ops 每轮都会报"内存紧张"，
# 那些告警会淹没日志、也掩盖真实问题。
VGA="${1:-std}"
MEM="${MEM:-512}"
# 测试用 scratch 磁盘（virtio → guest /dev/vda，供 aether-install 实测）
DISK=/home/aether/dist.raw
[ -f "$DISK" ] || truncate -s 512M "$DISK"
# 按进程名精确匹配来关（**不要**用 `pkill -f qemu-system-x86_64`：-f 匹配整条命令行，
# 会连"发起它的那条命令"一起杀掉，输出凭空消失，看起来像命令没跑）
pgrep -x qemu-system-x86_64 | while read -r p; do kill "$p" 2>/dev/null; done; sleep 1
cd /home/aether
rm -f qemu-serial.log qmp.sock qemu.pid vram.bin screen.ppm
setsid /usr/bin/qemu-system-x86_64 \
    -m "$MEM" -smp 2 \
    -cdrom /home/aether/aetheros-0.1-amd64.iso -boot d \
    -drive file="$DISK",format=raw,if=virtio \
    -vga "$VGA" -vnc 127.0.0.1:0 \
    -serial file:/home/aether/qemu-serial.log \
    -qmp unix:/home/aether/qmp.sock,server,nowait \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:17311-:7311 \
    -device virtio-net-pci,netdev=n0 \
    < /dev/null > /home/aether/qemu-stdout.log 2>&1 &
echo $! > /home/aether/qemu.pid
echo "QEMU pid=$(cat /home/aether/qemu.pid) vga=$VGA vnc=:5900 cwd=$(readlink /proc/$!/cwd 2>/dev/null)"
