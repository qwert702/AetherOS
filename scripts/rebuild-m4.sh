#!/bin/bash
# M4 重建：aetherd + compositor（musl 静态）→ cpio → initramfs 重嵌 → ISO
# 在构建机 /home/aether 内执行；本地仓库为唯一真源，源码已单独同步。
set -e
source ~/.cargo/env
cd /home/aether
OUT=/home/aether/platform/build/output

echo "==> [1/3] musl 编译 aetherd + aether-compositor + aether-ops"
cargo build --release --target x86_64-unknown-linux-musl -p aetherd 2>&1 | tail -1
cargo build --release --target x86_64-unknown-linux-musl -p aether-compositor 2>&1 | tail -1
cargo build --release --target x86_64-unknown-linux-musl -p aether-ops 2>&1 | tail -1
for b in aetherd aether-compositor aether-ops; do
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
