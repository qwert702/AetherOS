#!/bin/bash
# M3 终章：合成器 fbdev 后端 + 内核帧缓冲支持 → 开机直达自研桌面
set -e
source ~/.cargo/env
cd /home/aether

# 0. 归位上传的文件
[ -f fbdev.rs ] && cp -f fbdev.rs aether-compositor/src/fbdev.rs
[ -f compositor-main.rs ] && cp -f compositor-main.rs aether-compositor/src/main.rs
[ -f compositor-Cargo.toml ] && cp -f compositor-Cargo.toml aether-compositor/Cargo.toml
[ -f linux.fragment ] && cp -f linux.fragment platform/br2-external/board/aether/linux.fragment
[ -f isolinux.cfg ] && cp -f isolinux.cfg platform/br2-external/isolinux.cfg
cd platform && sed -i 's/\r$//' br2-external/board/aether/linux.fragment br2-external/isolinux.cfg
cd /home/aether

# 1. musl 编译三组件（含 fbdev 后端的合成器）
for c in aether-init aetherd aether-ops aether-compositor; do
    cargo build --release --target x86_64-unknown-linux-musl -p "$c" 2>&1 | tail -1
    cp "target/x86_64-unknown-linux-musl/release/$c" "platform/overlay/usr/bin/$c"
    cp "target/x86_64-unknown-linux-musl/release/$c" "platform/build/output/target/usr/bin/$c"
done

# 2. defconfig 加内核配置片
grep -q CONFIG_FRAGMENT_FILE /home/aether/platform/br2-external/configs/aetheros_defconfig || \
    echo 'BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILE="$(BR2_EXTERNAL_AETHER_PATH)/board/aether/linux.fragment"' >> /home/aether/platform/br2-external/configs/aetheros_defconfig
sed -i 's/\r$//' /home/aether/platform/br2-external/configs/aetheros_defconfig

# 3. 全量重编（内核带新配置 + rootfs + ISO），日志落盘
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output aetheros_defconfig > /dev/null
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 \
    > /home/aether/build.log 2>&1
echo "make exit: $?"
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
ls -la /home/aether/aetheros-0.1-amd64.iso
