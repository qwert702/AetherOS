#!/usr/bin/env python3
"""一条命令换系统：把 keepvar 版 ISO 挂上、无人安装（保留 /var）、再改回磁盘引导。

## 为什么需要它

"以后换系统不麻烦"= 不能每次手工改 vmx、盯屏幕、再改回去。
这台便携版 VMware 的 `vmrun` 用不了（拉不起/看不到 GUI 启动的 VM），所以宿主侧只能：
  ① 改 `.vmx`（光驱换成 keepvar ISO + 引导顺序改光驱）
  ② `vmware.exe -x <vmx>` 启动 → guest 自己装（aether.keepvar=1）→ 装完**自己关机**
  ③ 把 `.vmx` 改回（引导顺序改磁盘 + 断开光驱）→ 再启动
  ④ 用 `keepvar-verify.py` 出硬证据（/var 的 ext4 UUID 不变、启动记录不减）

本脚本把 ①②③ 串起来，④ 在你给出基线时自动跑。

## 可逆优先

* 默认**干跑**：只打印将要改什么，不碰任何文件；
* 真执行要 `--apply`，且**开始前把 vmx 备份**成 `.vmx.bak-<时间戳>`；
* 失败/中断时**自动把 vmx 改回磁盘引导**（`finally`），不会留下"从光驱引导"的状态。

## 前置条件（脚本会检查）

* VM **必须已关机**（VMware 会在关机时改写 vmx，运行中改无效且会被覆盖）；
* `E:\\aether-vm\\AetherOS\\aetheros-keepvar.iso` 存在（`--iso` 可指定别的）。

## 用法

    python scripts/vmware-keepvar.py --dry-run                  # 看要改什么（默认）
    python scripts/vmware-keepvar.py --apply                    # 真执行
    python scripts/vmware-keepvar.py --apply --expect-uuid <U> --min-records <N>   # 装完自动验证
    python scripts/vmware-keepvar.py --selftest                 # 纯逻辑自检（不碰 VM）
"""
from __future__ import annotations

import argparse
import datetime
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

VMX = Path(r"E:\aether-vm\AetherOS\AetherOS.vmx")
ISO = Path(r"E:\aether-vm\AetherOS\aetheros-keepvar.iso")
VMWARE = Path(r"D:\CBN-HT\Desktop\asdf\vmware.exe")
VMXPROC = "vmware-vmx"
BOOT_TIMEOUT = 15 * 60  # 装完会自己关机；最多等 15 分钟


def patch_vmx(text: str, *, cd_iso: str, connect_cd: bool, boot: str) -> str:
    """改 vmx 里的三行：光驱文件名 / 光驱是否连接 / 引导顺序。**纯函数**，便于自检。

    三行都必须存在（缺了说明 vmx 不是本项目期望的布局 —— 宁可报错也不猜）。
    """
    assert boot in ("cdrom", "disk")
    out = text
    for key, value in (
        ("sata0:1.fileName", cd_iso),
        ("sata0:1.startConnected", "TRUE" if connect_cd else "FALSE"),
        ("bios.bootOrder", boot),
    ):
        pat = re.compile(rf'^{re.escape(key)}\s*=\s*"[^"]*"\s*$', re.M)
        if not pat.search(out):
            raise ValueError(f"vmx 里找不到 {key} 这一行（布局变了？）")
        out = pat.sub(f'{key} = "{value}"', out)
    return out


def vm_running() -> bool:
    r = subprocess.run(["tasklist", "/FI", f"IMAGENAME eq {VMXPROC}.exe"],
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    return VMXPROC in r.stdout


def start_vm(vmx: Path) -> None:
    # `vmware.exe -x` 是这台便携版唯一可靠的启动方式（vmrun 会报 Postpone the command）
    subprocess.Popen([str(VMWARE), "-x", str(vmx)])


def wait_power_off(timeout: int) -> bool:
    """装完 guest 会自己关机 ⇒ 等 vmware-vmx 进程消失。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not vm_running():
            return True
        time.sleep(5)
    return False


def run(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="一条命令换系统（keepvar 版 ISO）")
    ap.add_argument("--vmx", default=str(VMX))
    ap.add_argument("--iso", default=str(ISO))
    ap.add_argument("--dry-run", action="store_true", help="只打印要改什么（默认行为，可显式写）")
    ap.add_argument("--apply", action="store_true", help="真的执行（默认只干跑）")
    ap.add_argument("--expect-uuid", default="", help="装完自动比对 /var 的 ext4 UUID")
    ap.add_argument("--min-records", type=int, default=-1, help="装完自动比对启动记录数")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()

    vmx, iso = Path(args.vmx), Path(args.iso)
    print(f"vmx : {vmx}")
    print(f"ISO : {iso}")
    if not vmx.is_file():
        print("✗ 找不到 vmx", file=sys.stderr)
        return 2
    if not iso.is_file():
        print(f"✗ 找不到 ISO（先构建：AETHER_AUTOINSTALL=keepvar platform/build-iso.sh）", file=sys.stderr)
        return 2

    original = vmx.read_text(encoding="utf-8")
    to_cd = patch_vmx(original, cd_iso=iso.name, connect_cd=True, boot="cdrom")
    to_disk = patch_vmx(original, cd_iso=iso.name, connect_cd=False, boot="disk")

    print("\n== 将要做的改动 ==")
    for a, b in zip(original.splitlines(), to_cd.splitlines()):
        if a != b:
            print(f"  - {a}\n  + {b}")
    print("== 装完之后（改回磁盘引导 + 断开光驱）==")
    for a, b in zip(original.splitlines(), to_disk.splitlines()):
        if a != b:
            print(f"  - {a}")
            print(f"  + {b}")

    if not args.apply:
        print("\n默认干跑，未做任何改动。确认无误后加 --apply。")
        return 0

    if vm_running():
        print("\n✗ 虚拟机正在运行 —— VMware 关机时会改写 vmx，现在改无效。\n"
              "  请先在 VMware 窗口里关机（虚拟机 → 电源 → 关闭客户机），再重跑本脚本。", file=sys.stderr)
        return 3

    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    backup = vmx.with_suffix(f".vmx.bak-{stamp}")
    shutil.copyfile(vmx, backup)
    print(f"\n已备份 vmx → {backup.name}")

    rc = 0
    try:
        vmx.write_text(to_cd, encoding="utf-8")
        print("== ① 已改 vmx：光驱连接 + 从光驱引导 ==")
        start_vm(vmx)
        print("== ② 已启动（guest 会自己装、装完自己关机）… ==")
        if not wait_power_off(BOOT_TIMEOUT):
            print("✗ 等超时（15 分钟）—— guest 可能卡住，请人看一眼窗口", file=sys.stderr)
            rc = 4
        else:
            print("   guest 已自动关机 ✓")
            vmx.write_text(to_disk, encoding="utf-8")
            print("== ③ 已改回磁盘引导并断开光驱 ==")
            start_vm(vmx)
            print("== ④ 已从磁盘启动（新系统 + 原 /var）==")
    finally:
        # 无论成败都保证 vmx 回到"从磁盘引导"，不留下"从光驱引导"的状态
        vmx.write_text(to_disk, encoding="utf-8")
        print("（已确保 vmx 为磁盘引导）")

    if rc == 0 and (args.expect_uuid or args.min_records >= 0):
        cmd = [sys.executable, str(Path(__file__).with_name("keepvar-verify.py")), "--report"]
        if args.expect_uuid:
            cmd += ["--expect-uuid", args.expect_uuid]
        if args.min_records >= 0:
            cmd += ["--min-records", str(args.min_records)]
        print("\n== ⑤ 取 T3 硬证据（读磁盘，不信串口）==")
        return subprocess.run(cmd).returncode
    return rc


def selftest() -> int:
    fails: list[str] = []

    def check(cond: bool, what: str) -> None:
        print(("  [OK  ] " if cond else "  [FAIL] ") + what)
        if not cond:
            fails.append(what)

    sample = (
        'firmware = "bios"\n'
        'bios.bootOrder = "disk"\n'
        'sata0:0.fileName = "disk.vmdk"\n'
        'sata0:1.fileName = "aether-autoinstall.iso"\n'
        'sata0:1.startConnected = "FALSE"\n'
    )
    to_cd = patch_vmx(sample, cd_iso="aetheros-keepvar.iso", connect_cd=True, boot="cdrom")
    check('bios.bootOrder = "cdrom"' in to_cd, "改成从光驱引导")
    check('sata0:1.fileName = "aetheros-keepvar.iso"' in to_cd, "光驱换成 keepvar ISO")
    check('sata0:1.startConnected = "TRUE"' in to_cd, "光驱设为已连接")
    check('sata0:0.fileName = "disk.vmdk"' in to_cd, "磁盘行不被误改")
    to_disk = patch_vmx(to_cd, cd_iso="aetheros-keepvar.iso", connect_cd=False, boot="disk")
    check('bios.bootOrder = "disk"' in to_disk, "改回从磁盘引导")
    check('sata0:1.startConnected = "FALSE"' in to_disk, "装完断开光驱")
    # 负向：缺关键行必须报错，不能猜
    try:
        patch_vmx('firmware = "bios"\n', cd_iso="x.iso", connect_cd=True, boot="cdrom")
        check(False, "负向：缺关键行应当报错，却没有")
    except ValueError:
        check(True, "负向：缺关键行报错（不猜）")
    print("自检" + ("全部通过" if not fails else f"失败（{len(fails)} 项）"))
    return 0 if not fails else 1


if __name__ == "__main__":
    raise SystemExit(run())
