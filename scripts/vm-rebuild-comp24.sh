#!/bin/bash
# 编译合成器（24bpp）→ 重打 ISO
set -e
source ~/.cargo/env
cd /home/aether
cp -f fbdev.rs aether-compositor/src/fbdev.rs
cargo build --release --target x86_64-unknown-linux-musl -p aether-compositor 2>&1 | tail -1
cp target/x86_64-unknown-linux-musl/release/aether-compositor platform/overlay/usr/bin/
cp target/x86_64-unknown-linux-musl/release/aether-compositor platform/build/output/target/usr/bin/
rm -rf platform/build/output/build/buildroot-fs platform/build/output/images/rootfs.*
cd platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output rootfs-iso9660 -j2 2>&1 | tail -1
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
echo "ISO updated: $(ls -la /home/aether/aetheros-0.1-amd64.iso | awk '{print $5}')"
