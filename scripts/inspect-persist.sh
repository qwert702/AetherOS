#!/bin/bash
# 检查持久化分区内容（宿主机 loop 挂载点 /mnt/pp，由调用方挂载）
echo "=== boot.log（每次启动追加一行，行数即持久化自证）"
cat /mnt/pp/boot.log 2>/dev/null || echo "(无 boot.log)"
echo "=== install-id（安装时写入）"
cat /mnt/pp/log/install-id 2>/dev/null || echo "(无 install-id)"
echo "=== 分区骨架"
ls /mnt/pp 2>/dev/null
echo "=== 服务日志（logtee 落盘 /var/log/aether）"
ls -la /mnt/pp/log/aether/ 2>/dev/null | head -10
echo "=== aetherd 日志尾部"
tail -3 /mnt/pp/log/aether/aetherd.log 2>/dev/null || echo "(无 aetherd 日志)"
