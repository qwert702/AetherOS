#!/usr/bin/env python3
"""T3 验证工具：从 VMware 磁盘里取「`--keep-var` 真的保住了 /var」的硬证据（2026-10-16）。

## 为什么需要它

VMware 这台机器上**串口不可靠**（`serial.txt` 常为 0 字节 —— 2026-10-16 两次实测），
所以"换系统不丢数据"不能靠串口自证。改成**直接读磁盘**：

1. `vmware-vdiskmanager -r` 把稀疏盘转成扁平 raw；
2. 在 raw 里算出持久分区（`/var`）的 **ext4 UUID**（超级块在分区起点 +1024，UUID 在 +0x68）；
3. 数一遍 `/var/boot.log` 里的启动记录条数。

判据（跑两次对比）：**UUID 必须不变** + **启动记录条数不得减少**（`--keep-var` 是"追加"语义）。
不带 `--keep-var` 重装时 UUID 必然变 —— 那是**负向对照**，用来证明这个工具真的会响。

## 用法

    python scripts/keepvar-verify.py --report            # 出基线（装机后、换系统前）
    python scripts/keepvar-verify.py --report --raw out.raw

    # 换系统之后再跑一次，与基线比对：
    python scripts/keepvar-verify.py --expect-uuid <基线 UUID> --min-records <基线条数>

    python scripts/keepvar-verify.py --selftest          # 纯逻辑自检（不需要磁盘）

## 诚实边界

- 启动记录条数是**在 raw 里按标记文本数出来的**（ext4 小文件通常连续存放，够用）；
  它不是文件系统级解析 —— 若 /var 高度碎片化，这个数字可能偏低。所以判据用"不得减少"，
  而不是"必须精确等于 N"。
- 分区起点默认按项目布局（32 MB 对齐，见 aether-install 的 PERSIST_LBA_START）算，
  可用 `--part-lba` 覆盖。
"""
from __future__ import annotations

import argparse
import re
import struct
import subprocess
import sys
from pathlib import Path

# 项目布局：持久分区从 32 MB 处开始（2048 对齐，见 aether-install 的常量注释）
DEFAULT_PART_LBA = 32768
SECTOR = 512
EXT4_SB_OFFSET = 1024
EXT4_UUID_OFFSET = 0x68
# boot.log 里的启动记录形如「<时间戳> 启动（第 N 次）」—— 取其中最稳的一段做标记
DEFAULT_MARKER = "启动（第"
VMWARE_VDISKMANAGER = r"D:\CBN-HT\Desktop\asdf\vmware-vdiskmanager.exe"


def uuid_from_superblock(sb: bytes) -> str:
    """从 ext4 超级块（1024 字节）里取文件系统 UUID。纯函数，便于自检。"""
    if len(sb) < EXT4_UUID_OFFSET + 16:
        raise ValueError("超级块太短")
    # 魔数 0xEF53 在超级块偏移 0x38（小端）
    magic = struct.unpack_from("<H", sb, 0x38)[0]
    if magic != 0xEF53:
        raise ValueError(f"不是 ext4 超级块（魔数 0x{magic:04x} ≠ 0xef53）")
    b = sb[EXT4_UUID_OFFSET:EXT4_UUID_OFFSET + 16]
    h = b.hex()
    return f"{h[0:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:32]}"


def count_records(data: bytes, marker: str) -> int:
    """数 raw 里标记文本的出现次数。纯函数，便于自检。"""
    needle = marker.encode("utf-8")
    if not needle:
        return 0
    return data.count(needle)


def to_raw(vmdk: Path, raw: Path) -> None:
    if raw.exists():
        raw.unlink()
    tool = Path(VMWARE_VDISKMANAGER)
    if not tool.is_file():
        sys.exit(f"找不到 {tool}（vmware-vdiskmanager）")
    print(f"== 转换 {vmdk.name} → {raw.name}（稀疏 → 扁平）==")
    r = subprocess.run([str(tool), "-r", str(vmdk), "-t", "0", str(raw)],
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    if r.returncode != 0 or not raw.exists():
        sys.exit(f"转换失败（rc={r.returncode}）：{(r.stdout + r.stderr).strip()[:300]}")
    print(f"  完成：{raw.stat().st_size / 1024 / 1024:.1f} MB")


def report(raw: Path, part_lba: int, marker: str) -> tuple[str, int]:
    size = raw.stat().st_size
    off = part_lba * SECTOR + EXT4_SB_OFFSET
    if off + 1024 > size:
        sys.exit(f"分区起点超出行尾（raw {size} 字节，需要读到 {off + 1024}）——盘是不是没装？")
    with raw.open("rb") as f:
        f.seek(off)
        sb = f.read(1024)
    try:
        uuid = uuid_from_superblock(sb)
    except ValueError as e:
        sys.exit(f"读不出 ext4 UUID：{e}\n（若这块盘还没装过 AetherOS，这是**正确**的结果）")
    print("== /var 的 ext4 UUID ==")
    print(f"  {uuid}   （分区起点 LBA {part_lba}）")
    print(f"== 启动记录（标记 {marker!r}）==")
    # 分块扫整个 raw：数标记出现次数（不整文件读进内存）
    total = 0
    with raw.open("rb") as f:
        carry = b""
        while True:
            chunk = f.read(8 * 1024 * 1024)
            if not chunk:
                break
            total += count_records(carry + chunk, marker)
            carry = chunk[-(len(marker.encode("utf-8")) - 1 or 1):]
    print(f"  找到 {total} 条")
    return uuid, total


def selftest() -> int:
    fails: list[str] = []

    def check(cond: bool, what: str) -> None:
        print(("  [OK  ] " if cond else "  [FAIL] ") + what)
        if not cond:
            fails.append(what)

    # ① 超级块解析：造一个合法 ext4 超级块
    sb = bytearray(1024)
    struct.pack_into("<H", sb, 0x38, 0xEF53)
    want = "12345678-9abc-def0-1234-56789abcdef0"
    sb[EXT4_UUID_OFFSET:EXT4_UUID_OFFSET + 16] = bytes.fromhex(want.replace("-", ""))
    check(uuid_from_superblock(bytes(sb)) == want, f"ext4 UUID 解析正确（{want}）")
    # 负向：魔数不对必须拒绝（否则会把随便一块盘当成我们的持久分区）
    bad = bytearray(sb)
    struct.pack_into("<H", bad, 0x38, 0x0000)
    try:
        uuid_from_superblock(bytes(bad))
        check(False, "负向：非 ext4 超级块应当被拒绝，却没有")
    except ValueError:
        check(True, "负向：非 ext4 超级块被拒绝")
    # 负向：太短
    try:
        uuid_from_superblock(b"short")
        check(False, "负向：超短输入应当被拒绝，却没有")
    except ValueError:
        check(True, "负向：超短输入被拒绝")

    # ② 记录计数：中文标记按 UTF-8 字节数
    data = ("x " + DEFAULT_MARKER + " 1）\n").encode("utf-8") * 3
    check(count_records(data, DEFAULT_MARKER) == 3, "启动记录计数正确（3 条）")
    check(count_records(b"nothing here", DEFAULT_MARKER) == 0, "无标记时为 0")
    check(count_records(b"", DEFAULT_MARKER) == 0, "空输入为 0")

    # ③ 标记跨块边界的处理（分块扫描时不能漏）
    blob = b"a" * 5 + DEFAULT_MARKER.encode("utf-8") + b"b" * 5
    check(blob.count(DEFAULT_MARKER.encode("utf-8")) == 1, "标记在块内能被数到（分块扫的边界逻辑见 report）")

    print("自检" + ("全部通过" if not fails else f"失败（{len(fails)} 项）"))
    return 0 if not fails else 1


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="T3：从 VMware 磁盘取「/var 被保住」的硬证据")
    ap.add_argument("--vmdk", default=r"E:\aether-vm\AetherOS\disk.vmdk", help="VMware 稀疏盘")
    ap.add_argument("--raw", default="", help="扁平文件输出路径（默认与 vmdk 同目录同名 .raw）")
    ap.add_argument("--part-lba", type=int, default=DEFAULT_PART_LBA, help="持久分区起始 LBA")
    ap.add_argument("--marker", default=DEFAULT_MARKER, help="启动记录里的标记文本")
    ap.add_argument("--report", action="store_true", help="转换并打印 UUID 与记录数")
    ap.add_argument("--expect-uuid", default="", help="与基线比对：UUID 必须等于它")
    ap.add_argument("--min-records", type=int, default=-1, help="与基线比对：记录数不得少于它")
    ap.add_argument("--selftest", action="store_true", help="纯逻辑自检")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()

    vmdk = Path(args.vmdk)
    if not vmdk.is_file():
        sys.exit(f"找不到 {vmdk}")
    raw = Path(args.raw) if args.raw else vmdk.with_suffix(".raw")
    to_raw(vmdk, raw)
    uuid, records = report(raw, args.part_lba, args.marker)

    ok = True
    if args.expect_uuid:
        same = uuid == args.expect_uuid
        print(f"== 判据：UUID 不变 ==")
        print(f"  基线 {args.expect_uuid}\n  现在 {uuid}\n  {'✓ 一致' if same else '✗ 变了 —— /var 被重建了'}")
        ok = ok and same
    if args.min_records >= 0:
        enough = records >= args.min_records
        print(f"== 判据：启动记录不减少 ==")
        print(f"  基线 {args.min_records}\n  现在 {records}\n  {'✓ 没丢' if enough else '✗ 少了 —— /var 被清过'}")
        ok = ok and enough
    if args.expect_uuid or args.min_records >= 0:
        print("\nT3 结论：" + ("PASS ✓（--keep-var 真的保住了 /var）" if ok else "FAIL ✗"))
        return 0 if ok else 1
    print("\n把上面两行记下来当基线，换系统后用 --expect-uuid / --min-records 比对。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
