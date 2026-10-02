#!/bin/bash
# M3 收尾：编 aether-ops(ops) musl → 三组件入 overlay/target → compositor 延后自启 → 重打包 ISO
set -e
source ~/.cargo/env
cd /home/aether
grep -vE "^\s*$" aetheros-0.1-amd64.iso.bak 2>/dev/null || true
# 1. 三组件 musl 编译
for c in aether-init aetherd aether-ops; do
    cargo build --release --target x86_64-unknown-linux-musl -p "$c" 2>&1 | tail -1
done
mkdir -p platform/overlay/usr/bin platform/build/output/target/usr/bin
for c in aether-init aetherd aether-ops; do
    cp "target/x86_64-unknown-linux-musl/release/$c" "platform/overlay/usr/bin/$c"
    cp "target/x86_64-unknown-linux-musl/release/$c" "platform/build/output/target/usr/bin/$c"
done
echo "=== 组件入位 ==="
ls -la platform/overlay/usr/bin/
# 2. compositor 等 smithay 后端就绪再自启
cat > platform/overlay/etc/aether/services/compositor.json <<'EOF'
{"name": "compositor", "deps": ["network", "aetherd"], "essential": true, "autostart": false}
EOF
sed -i 's/\r$//' platform/overlay/etc/aether/services/compositor.json
# 3. 强制重打 rootfs + ISO
rm -rf platform/build/output/build/buildroot-fs platform/build/output/images/rootfs.*
cd /home/aether/platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O=/home/aether/platform/build/output -j2 2>&1 | tail -2
cp /home/aether/platform/build/output/images/rootfs.iso9660 /home/aether/aetheros-0.1-amd64.iso
/home/aether/platform/build/output/host/bin/xorriso -indev /home/aether/aetheros-0.1-amd64.iso -find /usr/bin 2>/dev/null | head -6
