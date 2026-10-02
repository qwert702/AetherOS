#!/bin/sh
# AetherOS 镜像后处理（Buildroot BR2_ROOTFS_POST_BUILD_SCRIPT）。
#
# 为什么需要它（2026-10-02 审计 I-2 / I-4）：
#   1. Buildroot 默认给 `/bin/busybox` 带上 setuid 位（04755）。本镜像**没有非 root
#      用户**，也没有任何需要 setuid 的场景，保留它只是白送一个提权面
#      （busybox 的 applet 里带 su/passwd 等）。这里直接去掉 setuid 位。
#   2. 镜像指纹还是 Buildroot 的默认值（hostname=buildroot、os-release 也是 Buildroot）。
#      对外发布的系统不该自称 Buildroot —— 既影响识别，也让"这是什么系统"难以回答。
#
# 参数：$1 = 目标 rootfs 目录（Buildroot 约定）
set -eu

TARGET="$1"
[ -d "$TARGET" ] || { echo "post-build: 目标目录不存在: $TARGET" >&2; exit 1; }

echo "==> post-build: 去掉 busybox 的 setuid 位（镜像无普通用户，不需要它）"
for p in "$TARGET/bin/busybox" "$TARGET/usr/bin/busybox"; do
    if [ -e "$p" ]; then
        chmod 0755 "$p"
        echo "    $p -> 0755"
    fi
done

echo "==> post-build: 写入系统指纹（不再是 Buildroot 默认值）"printf 'aether\n' > "$TARGET/etc/hostname"
cat > "$TARGET/etc/os-release" <<'EOF'
NAME="AetherOS"
ID=aetheros
ID_LIKE=buildroot
VERSION="0.1"
PRETTY_NAME="AetherOS 0.1"
HOME_URL="https://github.com/qwert702/AetherOS"
VERSION_ID="0.1"
EOF
# /etc/issue 由 overlay 提供（含"控制台访问 = root"的提示）；这里只兜底：
# 若 overlay 没带上，就写一句明确的，而不是让 Buildroot 默认的欢迎语留在那儿。
if [ ! -s "$TARGET/etc/issue" ]; then
    printf 'AetherOS 0.1 — 无账号体系，控制台访问即 root（详见 README「边界在哪」）\n' \
        > "$TARGET/etc/issue"
fi

# 认证基线（2026-10-02 审计 M-4）：口令哈希**只在构建产物里落盘**，不进仓库。
# 为什么放在这里而不是 build-iso.sh 写 overlay：`platform/overlay/etc/` 已被 git
# 跟踪，写在那里会让 /etc/shadow 出现在 `git status` 里（一次 git add -A 就入库）。
if [ -n "${AETHER_ROOT_PW_HASH:-}" ]; then
    printf 'root:%s:0:0:99999:7:::\n' "$AETHER_ROOT_PW_HASH" > "$TARGET/etc/shadow"
    chmod 600 "$TARGET/etc/shadow"
    echo "==> post-build: 已写入 root 口令哈希（镜像需要登录）"
else
    echo "==> post-build: root 无口令（Live 演示系统的刻意取舍，见 /etc/issue）"
fi

echo "==> post-build 完成"
