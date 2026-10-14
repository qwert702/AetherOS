#!/bin/bash
# 装机验证一键驱动（**在构建机内跑**）—— 对应《镜像重建手册》§4 的四步：
#   ① 起 ISO（qemu-verify.sh）→ ② 走安装器（QMP 注入）→ ③ 从盘引导（qemu-disk-boot.sh）
#   → ④ 查持久化（loop 挂载 + inspect-persist.sh）
#
# 为什么要有这一层（2026-10-14 实测的教训）：
#   宿主逐条 SSH 驱动是**不可行**的 —— QEMU 一起来，构建机（三层嵌套虚拟化）里
#   sshd 就饿到无法完成认证（paramiko 与 OpenSSH 客户端都会超时），于是
#   "起 QEMU → 注入按键 → 查串口" 这套需要多次往返的流程必然卡死。
#   这个脚本把整套流程放在**服务端一次跑完**，宿主只在开始时发一条命令、
#   结束时取一次报告（那时 QEMU 已被停掉，SSH 恢复正常）。
#
# 用法（宿主侧）:
#   python scripts/vm.py sh "cd /home/aether && nohup bash scripts/vm-install-verify.sh >/dev/null 2>&1 & echo LAUNCHED"
#   之后取报告:
#   python scripts/vm.py sh "cat /home/aether/install-verify.log"
#
# 产物: /home/aether/install-verify.log（含 RESULT=PASS/FAIL 与逐项判据）
set -uo pipefail

cd /home/aether || exit 1
REPORT=/home/aether/install-verify.log
ISO=/home/aether/aetheros-0.1-amd64.iso
DISK=/home/aether/dist.raw
SERIAL=/home/aether/qemu-serial.log

#: 判据阈值（与手册 §4 的"怎么算过"一一对应）
BOOT_FRAMES_MIN=60      # 起 ISO 后至少要渲染这么多帧才算到了桌面
DISKBOOT_FRAMES_MIN=30  # 从盘引导同样要求
PARTS_MIN=2             # 目标盘至少 2 个分区（EFI/引导 + 持久化）

log() { echo "[$(date -Is)] $*" >>"$REPORT"; }
kill_qemu() { pkill -f qemu-system-x86_64 2>/dev/null; sleep 2; }

# 数串口里的帧数（文件可能还不存在 → 记 0，不要报错中断）
frames() { grep -c '已渲染' "$SERIAL" 2>/dev/null || echo 0; }

: >"$REPORT"
log "=== 装机验证开始（RESULT 见文件末尾）==="
log "ISO: $(stat -c '%s' "$ISO") 字节 · sha256=$(sha256sum "$ISO" | cut -c1-16)"
log "宿主：$(uname -srm) · QEMU $(qemu-system-x86_64 --version | head -1 | awk '{print $4}')"

# ---- 0) 干净的 scratch 盘（安装目标）----
kill_qemu
rm -f "$DISK"
truncate -s 512M "$DISK"
log "目标盘已重置: $DISK 512M（未分区）"

# ---- ① 起 ISO ----
MEM=1024 bash scripts/qemu-verify.sh >>"$REPORT" 2>&1
sleep 5
for i in $(seq 1 90); do
    [ "$(frames)" -ge "$BOOT_FRAMES_MIN" ] && break
    sleep 5
done
BOOT_FRAMES=$(frames)
log "① 起 ISO：已渲染 $BOOT_FRAMES 帧（等了 $((i * 5)) 秒）"
grep -a 'aether-ops: 巡检' "$SERIAL" 2>/dev/null | tail -2 >>"$REPORT"

# ---- ② 走安装器（QMP 注入整条命令 + 回车）----
python3 qmp-verify.py key a e t h e r minus i n s t a l l spc minus minus d i s k \
    spc slash d e v slash v d a spc minus minus y e s >>"$REPORT" 2>&1
sleep 1
python3 qmp-verify.py key ret >>"$REPORT" 2>&1
sleep 60
log "② 安装器串口输出（含 aether-install 的行）："
grep -a -i 'aether-install' "$SERIAL" 2>/dev/null | tail -20 >>"$REPORT"
INSTALL_HITS=$(grep -a -c -i 'aether-install' "$SERIAL" 2>/dev/null || echo 0)

# ---- ③ 目标盘分区 + 从盘引导 ----
kill_qemu
PARTS=$(fdisk -l "$DISK" 2>/dev/null | grep -c "^$DISK[0-9]" || echo 0)
log "③ 目标盘分区数: $PARTS"
fdisk -l "$DISK" >>"$REPORT" 2>&1

bash scripts/qemu-disk-boot.sh >>"$REPORT" 2>&1
sleep 5
for i in $(seq 1 72); do
    [ "$(frames)" -ge "$DISKBOOT_FRAMES_MIN" ] && break
    sleep 5
done
DISKBOOT_FRAMES=$(frames)
log "③ 从盘引导：已渲染 $DISKBOOT_FRAMES 帧（等了 $((i * 5)) 秒）"
grep -a 'aether-ops: 巡检' "$SERIAL" 2>/dev/null | tail -2 >>"$REPORT"
kill_qemu

# ---- ④ 持久化：loop 挂持久化分区，看 boot.log 行数（每次启动追加一行）----
BOOTLOG_LINES=0
mkdir -p /mnt/pp
LOOP=$(losetup -Pf --show "$DISK" 2>>"$REPORT")
log "④ loop 设备: ${LOOP:-（losetup 失败）}"
if [ -n "${LOOP:-}" ]; then
    # 分区 2 = 持久化分区（见 qemu-disk-boot.sh 的说明）
    if sudo -n mount "${LOOP}p2" /mnt/pp 2>>"$REPORT" || mount "${LOOP}p2" /mnt/pp 2>>"$REPORT"; then
        bash scripts/inspect-persist.sh >>"$REPORT" 2>&1
        BOOTLOG_LINES=$(wc -l </mnt/pp/boot.log 2>/dev/null || echo 0)
        sudo -n umount /mnt/pp 2>/dev/null || umount /mnt/pp 2>/dev/null
    else
        log "④ 挂载失败（需要 root：losetup/mount）—— 持久化**未能验证**，如实记为 FAIL"
    fi
    sudo -n losetup -d "$LOOP" 2>/dev/null || losetup -d "$LOOP" 2>/dev/null
fi

# ---- 判据汇总（不猜：每条都是上面实测出来的）----
FAILED=0
[ "$BOOT_FRAMES" -ge "$BOOT_FRAMES_MIN" ] || { log "判据① 不满足：起 ISO 帧数 $BOOT_FRAMES < $BOOT_FRAMES_MIN"; FAILED=1; }
[ "$INSTALL_HITS" -ge 1 ] || { log "判据② 不满足：串口里没有 aether-install 的输出（安装器可能没跑起来）"; FAILED=1; }
[ "$PARTS" -ge "$PARTS_MIN" ] || { log "判据③ 不满足：目标盘分区数 $PARTS < $PARTS_MIN"; FAILED=1; }
[ "$DISKBOOT_FRAMES" -ge "$DISKBOOT_FRAMES_MIN" ] || { log "判据③ 不满足：从盘引导帧数 $DISKBOOT_FRAMES < $DISKBOOT_FRAMES_MIN"; FAILED=1; }
[ "$BOOTLOG_LINES" -ge 1 ] || { log "判据④ 不满足：持久化分区 boot.log 行数 $BOOTLOG_LINES（0 = 没挂上或没写）"; FAILED=1; }

log "汇总：帧数(ISO)=$BOOT_FRAMES 安装器命中=$INSTALL_HITS 分区数=$PARTS 帧数(盘)=$DISKBOOT_FRAMES boot.log 行数=$BOOTLOG_LINES"
if [ "$FAILED" -eq 0 ]; then
    echo "RESULT=PASS" >>"$REPORT"
else
    echo "RESULT=FAIL" >>"$REPORT"
fi
log "=== 结束 ==="
