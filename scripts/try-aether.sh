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

# 已发布 ISO 的 SHA256（与 site/data.json 的 iso_sha256 同源）。
#
# 为什么必须校验（2026-10-02 审计 M-11）：这个脚本会把下载到的东西**当作完整
# 操作系统在 QEMU 里引导**。下载链路上任何一环（Release CDN、代理、企业 TLS
# 拦截设备）被替换，用户就会开一个攻击者构造的系统。而哈希本来就有、就公开在
# 官网下载页上，没有理由不用。
#
# 换版本时同步更新；自建镜像用 AETHER_SHA256=<你的哈希> 覆盖；
# 显式置空（AETHER_SHA256=）表示知情地跳过校验。
SHA256="${AETHER_SHA256-43c53f4c8f8b16ebbd5e1a782b024d1fee1912015e0fc3c4c4e1c7481b1bd8aa}"

# 校验 $ISO 的 SHA256；通过返回 0，不通过返回 1。
verify_iso() {
    if [ -z "$SHA256" ]; then
        echo "[try-aether] 注意：AETHER_SHA256 被置空 —— 跳过完整性校验（知情选择）"
        return 0
    fi
    local tool=""
    if command -v sha256sum >/dev/null 2>&1; then
        tool="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        tool="shasum -a 256"
    else
        echo "[try-aether] 错误：系统里没有 sha256sum / shasum，无法校验 ISO 完整性" >&2
        echo "           装一个（coreutils / perl），或用 AETHER_SHA256= 显式跳过。" >&2
        return 1
    fi
    local actual
    actual="$($tool "$ISO" | awk '{print $1}')"
    if [ "$actual" != "$SHA256" ]; then
        echo "[try-aether] ✗ ISO 完整性校验失败 —— 已拒绝引导它" >&2
        echo "   文件    : $ISO" >&2
        echo "   期望    : $SHA256" >&2
        echo "   实得    : $actual" >&2
        echo "   可能原因: 下载被截断/被替换，或这是自建镜像（哈希不同属正常）。" >&2
        echo "   自建镜像: AETHER_SHA256=<你的哈希> bash scripts/try-aether.sh" >&2
        echo "   确认无误要跳过: AETHER_SHA256= bash scripts/try-aether.sh" >&2
        return 1
    fi
    echo "[try-aether] ✓ ISO SHA256 校验通过（$actual）"
}

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
    echo "[try-aether] 下载 ISO（约 38.5 MB）→ $ISO"
    if command -v curl >/dev/null 2>&1; then
        curl -fL --progress-bar -o "$ISO" "$URL"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --show-progress -O "$ISO" "$URL"
    else
        echo "[try-aether] 需要 curl 或 wget 之一来下载" >&2
        exit 1
    fi
    # 刚下载的文件校验不过 → 删掉它，否则下次运行会拿这份坏文件去引导
    if ! verify_iso; then
        rm -f "$ISO"
        echo "[try-aether] 已删除校验失败的下载文件" >&2
        exit 1
    fi
else
    # 已存在的文件同样要校验：它可能是上次被截断的下载，也可能被人替换过
    verify_iso || exit 1
fi

echo "[try-aether] 引导 $ISO（内存 ${MEM} MB）。在窗口里操作，关掉窗口就退出。"
echo "[try-aether] 桌面提示：Alt+1–4 切换布局，终端里可以直接敲命令。"
exec "$QEMU" -cdrom "$ISO" -m "$MEM" -boot d -vga std "$@"
