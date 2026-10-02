#!/bin/bash
# 给 Buildroot 配置 TUNA 主下载源（下载优先走国内，404 自动回退官方源），
# 并把当前卡住的 flex 包从 TUNA 直接补进 dl 缓存。
set -e
pkill -f buildroot-2024.02.1 2>/dev/null || true
pkill wget 2>/dev/null || true
sleep 2

CFG=/home/aether/platform/build/output/.config
grep -q BR2_PRIMARY_SITE "$CFG" || cat >> "$CFG" <<'EOF'
BR2_PRIMARY_SITE="https://mirrors.tuna.tsinghua.edu.cn"
BR2_PRIMARY_SITE_ONLY=n
BR2_BACKUP_SITE=""
EOF

cd /home/aether/platform/build/buildroot-2024.02.1
make O=/home/aether/platform/build/output olddefconfig > /dev/null 2>&1

# 补 flex（TUNA gnu 目录下是 .tar.bz2/gz 任一个存在即用）
DL=/home/aether/platform/build/buildroot-2024.02.1/dl/flex
mkdir -p "$DL"
cd "$DL"
if ! ls flex-2.6.4* >/dev/null 2>&1; then
    for f in flex-2.6.4.tar.gz flex-2.6.4.tar.bz2 flex-2.6.4.tar.xz; do
        if curl -sfL --max-time 120 -o "$f" "https://mirrors.tuna.tsinghua.edu.cn/gnu/flex/$f"; then
            echo "fetched $f"; break
        fi
        rm -f "$f"
    done
fi
ls -la

# 重启构建
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build pid $!"
