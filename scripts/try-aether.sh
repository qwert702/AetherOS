#!/usr/bin/env bash
# 一条命令把 AetherOS 跑起来：没有 ISO 就下载 Release 里的，然后用 QEMU 引导。
#
#   # 直接跑（先看一眼内容再执行更稳妥）：
#   curl -fsSL https://raw.githubusercontent.com/qwert702/AetherOS/main/scripts/try-aether.sh -o try-aether.sh
#   less try-aether.sh && bash try-aether.sh
#
#   # 或者克隆下来跑：
#   bash scripts/try-aether.sh
#
# 想调内存/版本：AETHER_MEM=2048 AETHER_VERSION=v0.1.0 bash scripts/try-aether.sh
# 额外参数会原样传给 QEMU：bash scripts/try-aether.sh -vnc :0
set -euo pipefail

VERSION="${AETHER_VERSION:-v0.1.0}"
ISO="${AETHER_ISO:-aetheros-0.1-amd64.iso}"
MEM="${AETHER_MEM:-1024}"
URL="https://github.com/qwert702/AetherOS/releases/download/${VERSION}/${ISO}"

QEMU=""
for cand in qemu-system-x86_64 qemu-system-x86; do
    if command -v "$cand" >/dev/null 2>&1; then
        QEMU="$cand"
        break
    fi
done
if [ -z "$QEMU" ]; then
    cat >&2 <<'EOF'
[try-aether] 没找到 qemu-system-x86_64。三种办法：

  Ubuntu / Debian : sudo apt install qemu-system-x86
  macOS (Homebrew): brew install qemu
  Windows         : winget install SoftwareFreedomConservancy.QEMU
                    —— 或者干脆不用 QEMU：下载 ISO 用 VirtualBox / VMware 挂载也能开

  ISO 直链：https://github.com/qwert702/AetherOS/releases/latest
EOF
    exit 1
fi

if [ ! -f "$ISO" ]; then
    echo "[try-aether] 下载 ISO（约 33 MB）→ $ISO"
    if command -v curl >/dev/null 2>&1; then
        curl -fL --progress-bar -o "$ISO" "$URL"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --show-progress -O "$ISO" "$URL"
    else
        echo "[try-aether] 需要 curl 或 wget 之一来下载" >&2
        exit 1
    fi
fi

echo "[try-aether] 引导 $ISO（内存 ${MEM} MB）。在窗口里操作，关掉窗口就退出。"
echo "[try-aether] 桌面提示：Alt+1–4 切换布局，终端里可以直接敲命令。"
exec "$QEMU" -cdrom "$ISO" -m "$MEM" -boot d -vga std "$@"
