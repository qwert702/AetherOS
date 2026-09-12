#!/bin/bash
# M4 重建：aether-init + aetherd + compositor + ops（musl 静态）→ cpio → initramfs 重嵌 → ISO
# 在构建机 /home/aether 内执行；本地仓库为唯一真源，源码已单独同步。
# pipefail：cargo 编译失败必须炸掉脚本，绝不能拿旧二进制混出新 ISO。
set -euo pipefail
source ~/.cargo/env
cd /home/aether
OUT=/home/aether/platform/build/output

echo "==> [1/3] musl 编译 aether-init + aetherd + aether-compositor + aether-ops"
for p in aether-init aetherd aether-compositor aether-ops; do
    cargo build --release --target x86_64-unknown-linux-musl -p "$p" 2>&1 | tail -1
done
for b in aether-init aetherd aether-compositor aether-ops; do
    cp "target/x86_64-unknown-linux-musl/release/$b" platform/overlay/usr/bin/
    cp "target/x86_64-unknown-linux-musl/release/$b" "$OUT/target/usr/bin/"
done

echo "==> [2/3] rootfs.cpio + initramfs 重嵌内核"
rm -rf "$OUT/build/buildroot-fs"
rm -f "$OUT/images/rootfs.cpio" "$OUT/images/rootfs.iso9660" "$OUT/images/bzImage"
cd platform/build/buildroot-2024.02.1
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" rootfs-cpio -j2 > /home/aether/rc.log 2>&1
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" linux-rebuild-with-initramfs -j2 >> /home/aether/rc.log 2>&1

echo "==> [3/3] ISO"
rm -rf "$OUT/build/buildroot-fs"
make BR2_EXTERNAL=/home/aether/platform/br2-external O="$OUT" rootfs-iso9660 -j2 >> /home/aether/rc.log 2>&1
cp "$OUT/images/rootfs.iso9660" /home/aether/aetheros-0.1-amd64.iso
echo "ISO: $(ls -la /home/aether/aetheros-0.1-amd64.iso | awk '{print $5}')"
echo REBUILD-DONE
