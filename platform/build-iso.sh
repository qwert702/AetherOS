#!/usr/bin/env bash
# AetherOS ISO 一键构建脚本（在 Linux/Ubuntu VM 内运行）
#
# 用法:  ./platform/build-iso.sh
# 产物:  platform/build/images/rootfs.iso9660  →  aetheros-<ver>-amd64.iso
#
# 步骤:
#   1. musl 静态交叉编译 Aether 组件（aether-init/aetherd/...）
#   2. 拷贝二进制进 rootfs overlay
#   3. 下载/解压 Buildroot，应用 defconfig，构建
set -euo pipefail

# Rust 不在非交互式 SSH 的 PATH 里（2026-09-29 实测：无人值守跑本脚本会
# `rustup: command not found` 而立刻失败）。显式补上，让脚本在 CI/ssh 下也能跑。
export PATH="$HOME/.cargo/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BR_VERSION="2024.02.1"
BUILD="$ROOT/platform/build"
OVERLAY="$ROOT/platform/overlay"
BR_EXT="$ROOT/platform/br2-external"

echo "==> [1/3] musl 静态编译 Aether 组件"
rustup target add x86_64-unknown-linux-musl
COMPONENTS=(aether-init aetherd aether-ops aether-compositor)
for c in "${COMPONENTS[@]}"; do
    cargo build --release --target x86_64-unknown-linux-musl -p "$c"
    mkdir -p "$OVERLAY/usr/bin"
    cp "target/x86_64-unknown-linux-musl/release/$c" "$OVERLAY/usr/bin/$c"
    echo "    $c -> overlay/usr/bin/"
done

# 中文屏显字体（文泉驿微米黑，来自宿主 fonts-wqy-microhei 包）：
# compositor 在 Linux 下从这里加载字体，缺了则桌面文字不显示。
FONT_SRC=/usr/share/fonts/truetype/wqy/wqy-microhei.ttc
if [ -f "$FONT_SRC" ]; then
    mkdir -p "$OVERLAY/usr/share/fonts/truetype/wqy"
    cp "$FONT_SRC" "$OVERLAY/usr/share/fonts/truetype/wqy/"
    echo "    wqy-microhei.ttc -> overlay/usr/share/fonts/"
else
    echo "    !! 未找到 $FONT_SRC —— 请先 apt-get install fonts-wqy-microhei"
fi

# Aether 自带 UI 字体（Noto Sans CJK SC 子集，**含真粗体**）—— 2026-09-29 起 compositor 优先用它：
# wqy-microhei 没有粗体（标题"加粗"实际没加粗）且偏点阵屏显风格。子集 = GB2312 全量汉字
# + ASCII + 常用标点，两个字重合计约 6 MB；fontTools 只是构建机工具，不进镜像。
# **失败不阻塞构建**：text.rs 的候选表里 wqy 仍在，会自动回退（代价是回到伪粗体）。
if command -v python3 >/dev/null 2>&1 && python3 "$ROOT/scripts/mkfont.py" --out "$OVERLAY/usr/share/fonts/truetype/aether"; then
    echo "    NotoSansSC-{Regular,Bold}.otf -> overlay/usr/share/fonts/truetype/aether/"
else
    echo "    !! 字体子集生成失败（需 python3 + fontTools + fonts-noto-cjk）—— 退回 wqy-microhei"
fi

echo "==> [2/3] 获取 Buildroot $BR_VERSION"
mkdir -p "$BUILD"
cd "$BUILD"
if [ ! -d "buildroot-$BR_VERSION" ]; then
    URL="https://buildroot.org/downloads/buildroot-$BR_VERSION.tar.gz"
    wget -q "$URL" -O br.tar.gz
    tar -xzf br.tar.gz
fi
cd "buildroot-$BR_VERSION"

echo "==> [3/3] Buildroot 构建（首次 30-60 分钟，后续增量）"
make BR2_EXTERNAL="$BR_EXT" O="$BUILD/output" aetheros_defconfig
make BR2_EXTERNAL="$BR_EXT" O="$BUILD/output" -j"$(nproc)"

ISO="$BUILD/output/images/rootfs.iso9660"
if [ -f "$ISO" ]; then
    cp "$ISO" "$ROOT/aetheros-0.1-amd64.iso"
    echo "==> 完成: $ROOT/aetheros-0.1-amd64.iso"
else
    echo "!! 未找到 ISO 产物，检查 $BUILD/output/images/"
    exit 1
fi
