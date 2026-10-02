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
#   4. isohybrid + 自检（MBR 签名）—— 少了这步 ISO 只能光盘引导、不能装机
set -euo pipefail

# Rust 不在非交互式 SSH 的 PATH 里（2026-09-29 实测：无人值守跑本脚本会
# `rustup: command not found` 而立刻失败）。显式补上，让脚本在 CI/ssh 下也能跑。
export PATH="$HOME/.cargo/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BR_VERSION="2024.02.1"
BUILD="$ROOT/platform/build"
OVERLAY="$ROOT/platform/overlay"
BR_EXT="$ROOT/platform/br2-external"

# ── 认证基线（2026-10-02 审计 M-4）──────────────────────────────────────────
# 出厂镜像 root **无口令**：控制台/串口拿到就是 root。对 Live 演示系统这是刻意的
# （没有账号体系才谈得上"一条命令跑起来"），但必须**显式可见**，而且要有上锁的路：
#   AETHER_ROOT_PW_HASH="$(openssl passwd -6)" platform/build-iso.sh
# 设了就由 post-build.sh 写进镜像的 /etc/shadow（0600），登录需要口令。
#
# ⚠️ 口令哈希**不写进 overlay 目录**（对抗审查发现）：`platform/overlay/etc/` 已被
# git 跟踪，写在那里会让 `/etc/shadow` 出现在 `git status` 里，一次 `git add -A`
# 就把口令哈希提交进仓库（离线爆破素材）。改为把哈希经环境变量交给 Buildroot 的
# post-build 脚本，只在**构建产物**里落盘。
if [ -n "${AETHER_ROOT_PW_HASH:-}" ]; then
    export AETHER_ROOT_PW_HASH
    echo "==> 认证基线：root 口令哈希已交给 post-build 写入镜像（不会落进仓库）"
else
    echo "!! 认证基线：出厂镜像 root 无口令 —— 控制台/串口拿到即 root。"
    echo "   这是 Live 演示系统的刻意取舍；要上锁请设 AETHER_ROOT_PW_HASH=\"\$(openssl passwd -6)\" 重新构建。"
    echo "   （提醒会同时出现在启动横幅与 /etc/issue 里，不会「悄悄」存在。）"
fi

echo "==> [1/3] musl 静态编译 Aether 组件"
rustup target add x86_64-unknown-linux-musl
COMPONENTS=(aether-init aetherd aether-ops aether-compositor)
for c in "${COMPONENTS[@]}"; do
    # --locked：按仓库里的 Cargo.lock 精确构建。不加的话 cargo 会顺手升级依赖，
    # 于是"同一个提交"在不同时间构建出的镜像可以不同（2026-10-02 审计 M-9）。
    cargo build --release --locked --target x86_64-unknown-linux-musl -p "$c"
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

# 工具链来源必须可对账（2026-10-02 审计 M-9）：此前是直接解包，下载到什么都用。
# 做法是 TOFU：第一次构建把哈希记下来，之后每次都对账，变了就停。
# （Buildroot 官方只发 .sign 的 GPG 签名，校验它需要导入构建机的 keyring，
#  对一台一次性构建机来说不如"首次记录 + 之后对账"实用。）
#
# ⚠️ 这段**必须在 `if [ ! -d ... ]` 之外**（对抗审查发现）：原来它被包在"还没解包"
# 的分支里，于是删掉解包目录之外的任何路径（比如解包目录已存在）都不会对账 ——
# 只要压缩包还在，就能拿它绕过校验。现在只要 br.tar.gz 存在就核对。
SUM_FILE="$BUILD/buildroot.sha256"
if [ -f "$BUILD/br.tar.gz" ]; then
    SUM_NOW="$(sha256sum "$BUILD/br.tar.gz" | awk '{print $1}')"
    if [ -f "$SUM_FILE" ]; then
        SUM_OLD="$(awk '{print $1}' "$SUM_FILE")"
        if [ "$SUM_OLD" != "$SUM_NOW" ]; then
            echo "!! Buildroot 压缩包哈希与上次构建不一致："
            echo "     上次: $SUM_OLD"
            echo "     本次: $SUM_NOW"
            echo "   要么官方重新发布了该版本（几乎不会），要么下载被篡改/损坏。请人工确认。"
            exit 1
        fi
        echo "    Buildroot 压缩包哈希对账通过（$SUM_NOW）"
    else
        echo "$SUM_NOW  buildroot-$BR_VERSION.tar.gz" > "$SUM_FILE"
        echo "    首次构建：已记录 Buildroot 压缩包哈希 $SUM_NOW → $SUM_FILE"
    fi
fi
cd "buildroot-$BR_VERSION"

echo "==> [3/3] Buildroot 构建（首次 30-60 分钟，后续增量）"
make BR2_EXTERNAL="$BR_EXT" O="$BUILD/output" aetheros_defconfig
make BR2_EXTERNAL="$BR_EXT" O="$BUILD/output" -j"$(nproc)"

ISO="$BUILD/output/images/rootfs.iso9660"
if [ -f "$ISO" ]; then
    cp "$ISO" "$ROOT/aetheros-0.1-amd64.iso"

    # isohybrid：让 ISO 可以被 dd 到磁盘后直接引导（M6 安装器依赖这一点）。
    #
    # 为什么必须有这一步（2026-10-02 审计 H-6）：aether-install 的前提是"源镜像
    # 带 MBR 签名与隐藏 ISO 分区表"，而纯 ISO9660 + El Torito **没有** MBR ——
    # 装机时 dd 会毁掉目标盘首 32MB，盘却起不来，程序还会打印"安装完成"。
    # 实测：未经本步骤产出的 ISO 前 512 字节全 0、无 0x55AA。
    ISOHYBRID="$BUILD/output/host/bin/isohybrid"
    if [ ! -x "$ISOHYBRID" ]; then
        echo "!! 未找到 $ISOHYBRID —— 缺 syslinux 主机工具。"
        echo "   这份 ISO 只能光盘引导，装机（aether-install）会拒绝执行。"
        exit 1
    fi
    "$ISOHYBRID" "$ROOT/aetheros-0.1-amd64.iso"

    # 自检：签名必须真的写进去了 —— 否则"看起来做了"和"真的做了"会分不清。
    SIGN="$(dd if="$ROOT/aetheros-0.1-amd64.iso" bs=1 skip=510 count=2 2>/dev/null \
            | od -An -tx1 | tr -d ' \n')"
    if [ "$SIGN" != "55aa" ]; then
        echo "!! isohybrid 之后仍无 MBR 签名（实得 '$SIGN'）—— 安装器会拒绝装机"
        exit 1
    fi
    echo "==> isohybrid 完成，MBR 签名自检通过（可 dd 到磁盘引导）"

    # 产出哈希文件（审计 M-9）：发布、下载校验、site/release.json 三处都要用到它，
    # 让"用户下载到的字节"和"我们公布的哈希"出自同一次构建、同一行命令。
    ( cd "$ROOT" && sha256sum "aetheros-0.1-amd64.iso" > "aetheros-0.1-amd64.iso.sha256" )
    echo "==> 完成: $ROOT/aetheros-0.1-amd64.iso"
    echo "    $(cat "$ROOT/aetheros-0.1-amd64.iso.sha256")"
    echo "    下一步：上传 Release 后把这份哈希同步进 site/release.json，再跑"
    echo "            python scripts/gen-site.py --refresh"
else
    echo "!! 未找到 ISO 产物，检查 $BUILD/output/images/"
    exit 1
fi
