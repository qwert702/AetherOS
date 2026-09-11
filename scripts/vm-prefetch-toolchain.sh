#!/bin/bash
# 预取工具链大件源码包进 Buildroot dl 缓存（USTC/TUNA 镜像）
set -e
pkill -f buildroot-2024.02.1 2>/dev/null || true
pkill wget 2>/dev/null || true
sleep 2
DL=/home/aether/platform/build/buildroot-2024.02.1/dl
fetch() { # fetch <子目录> <文件名> <URL>
    local dir="$DL/$1" file="$2" url="$3"
    mkdir -p "$dir"; cd "$dir"
    if [ -s "$file" ]; then echo "已有 $file"; return 0; fi
    echo "取 $file ..."
    curl -sfL --retry 2 --max-time 500 -o "$file" "$url" && echo "  OK $(du -h "$file" | cut -f1)" || { echo "  FAIL $url"; rm -f "$file"; }
}
fetch gcc "gcc-12.3.0.tar.xz"        "https://mirrors.ustc.edu.cn/gcc/releases/gcc-12.3.0/gcc-12.3.0.tar.xz"
fetch binutils "binutils-2.40.tar.xz" "https://mirrors.tuna.tsinghua.edu.cn/gnu/binutils/binutils-2.40.tar.xz"
fetch gmp "gmp-6.2.1.tar.xz"          "https://mirrors.tuna.tsinghua.edu.cn/gnu/gmp/gmp-6.2.1.tar.xz"
fetch mpfr "mpfr-4.2.1.tar.xz"        "https://mirrors.tuna.tsinghua.edu.cn/gnu/mpfr/mpfr-4.2.1.tar.xz"
fetch mpc "mpc-1.3.1.tar.gz"          "https://mirrors.tuna.tsinghua.edu.cn/gnu/mpc/mpc-1.3.1.tar.gz"
flex_dir="$DL/flex"; mkdir -p "$flex_dir"; cd "$flex_dir"
if ! ls flex-2.6.4* >/dev/null 2>&1; then
    curl -sfL --retry 2 --max-time 200 -o flex-2.6.4.tar.gz \
        "https://github.com/westes/flex/releases/download/v2.6.4/flex-2.6.4.tar.gz" \
        && echo "flex OK" || echo "flex FAIL(走官方回退)"
fi
echo "==> 重启构建"
source ~/.cargo/env
cd /home/aether/platform/build/buildroot-2024.02.1
nohup make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1 &
echo "build pid $!"
