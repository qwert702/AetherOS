#!/usr/bin/env python3
"""vm-smoke.py —— QEMU 启动冒烟（E3 硬件 CI / VM 冒烟）。

## 为什么单独做一个脚本

既有 `iso-smoke.yml` 把 QEMU 命令与串口判据写在 workflow 的 shell 步骤里；
本脚本把同一条流程收成可在本地与 CI 复用的单一入口，并显式区分三种结局：

| 退出码 | 含义 | CI 语义 |
|---|---|---|
| 0 | PASS：串口日志命中就绪关键字 | green |
| 1 | FAIL：启动超时 / 未命中关键字 / QEMU 非零退出 | red |
| 2 | 配置错误：参数或环境不可用 | red |
| 77 | SKIP：环境不具备（无 QEMU，或未提供 ISO） | neutral / skip，**不算 failure** |

> 77 是约定的 skip 码：本机 / 干净 checkout 常常没有 QEMU 或构建产物，
> 这时必须明确跳过，**不能 exit 0 假成功**。

## 用法

    python scripts/vm-smoke.py                      # 自动找 ISO，默认 120s 超时
    python scripts/vm-smoke.py --iso dist/xxx.iso
    python scripts/vm-smoke.py --timeout 180 --keep-logs

QEMU 启动参数与既有 `iso-smoke.yml` / `qemu-verify.sh` 保持同一套：
TCG 加速、headless（`-display none`）、串口落文件、`-no-reboot`。
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 与 `.github/workflows/iso-smoke.yml` 的串口判据保持一致，不另起一套关键字。
READY_RE = re.compile(r"aether-init|AetherOS|aether-compositor|login:")

SKIP = 77
FAIL = 1
CONFIG_ERROR = 2

# 候选 ISO 位置（与 release-readiness.py 的查找口径一致）。
ISO_CANDIDATES = (
    ROOT / "aetheros-0.1-amd64.iso",
)


def find_iso() -> Path | None:
    """按"根目录当前产物优先，否则 dist 下最新"的顺序找 ISO。"""
    existing = [p for p in ISO_CANDIDATES if p.is_file()]
    for d in (ROOT / "dist", ROOT / "dist" / "archive", ROOT / "dist" / "candidate"):
        if d.is_dir():
            existing.extend(p for p in d.glob("*.iso") if p.is_file())
    if not existing:
        return None
    return max(existing, key=lambda p: (p.parent == ROOT, p.stat().st_mtime))


def tail_lines(text: str, n: int = 40) -> str:
    lines = text.splitlines()
    return "\n".join(lines[-n:]) if lines else ""


def run_qemu(iso: Path, timeout: float, log_dir: Path) -> tuple[int, str]:
    """启动 QEMU 并等待串口就绪；返回 (退出码, 日志尾部摘要)。

    退出码：0 PASS，1 FAIL。
    """
    serial_log = log_dir / "serial.log"
    stdout_log = log_dir / "qemu-stdout.log"

    cmd = [
        "qemu-system-x86_64",
        "-machine", "accel=tcg",
        "-cpu", "max",
        "-m", "1024",
        "-smp", "2",
        "-cdrom", str(iso),
        "-boot", "d",
        "-display", "none",
        "-serial", f"file:{serial_log}",
        "-no-reboot",
    ]

    print(f"[INFO] QEMU: {' '.join(cmd)}")
    print(f"[INFO] ISO: {iso}")
    print(f"[INFO] 超时: {timeout:.0f}s · 串口日志: {serial_log}")

    stdout_f = stdout_log.open("w", encoding="utf-8", errors="replace")
    try:
        proc = subprocess.Popen(
            cmd, stdout=stdout_f, stderr=subprocess.STDOUT,
            cwd=ROOT,
        )
    except FileNotFoundError as exc:
        stdout_f.close()
        return FAIL, f"QEMU 无法启动：{exc}"

    deadline = time.time() + timeout
    early_exit: int | None = None
    try:
        while time.time() < deadline:
            ret = proc.poll()
            if ret is not None:
                early_exit = ret
                break
            if serial_log.is_file() and READY_RE.search(serial_log.read_text(encoding="utf-8", errors="replace")):
                return 0, "串口日志命中就绪关键字"
            time.sleep(1.0)
    finally:
        if proc.poll() is None:
            # 超时 / 命中后收尾：停掉 guest，避免冒烟进程挂住 CI。
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
        stdout_f.close()

    serial_text = serial_log.read_text(encoding="utf-8", errors="replace") if serial_log.is_file() else ""
    stdout_text = stdout_log.read_text(encoding="utf-8", errors="replace") if stdout_log.is_file() else ""

    if early_exit is not None and early_exit != 0:
        return FAIL, f"QEMU 提前退出，code={early_exit}"
    if READY_RE.search(serial_text):
        return 0, "串口日志命中就绪关键字（QEMU 随后退出）"

    reason = "超时未命中就绪关键字" if early_exit is None else f"QEMU 退出 code={early_exit}，串口未命中关键字"
    tail = tail_lines(serial_text or stdout_text)
    return FAIL, f"{reason}\n--- 日志尾部 ---\n{tail}"


def main() -> int:
    ap = argparse.ArgumentParser(description="AetherOS QEMU 启动冒烟")
    ap.add_argument("--timeout", type=float, default=120.0,
                    help="等待串口就绪的秒数（默认 120）")
    ap.add_argument("--iso", type=Path, default=None,
                    help="待启动 ISO；省略则在仓库内自动查找")
    ap.add_argument("--keep-logs", action="store_true",
                    help="保留日志目录（默认 PASS 后清理临时目录；FAIL 始终保留）")
    args = ap.parse_args()

    qemu = shutil.which("qemu-system-x86_64")
    if not qemu:
        print("SKIP: 无 QEMU（未找到 qemu-system-x86_64）；环境不具备，跳过本次冒烟。")
        return SKIP
    print(f"[INFO] QEMU 路径: {qemu}")

    iso = args.iso.resolve() if args.iso else find_iso()
    if iso is None or not iso.is_file():
        print("SKIP: 未找到 ISO（用 --iso 指定，或先构建产物）；环境不具备，跳过本次冒烟。")
        return SKIP

    log_dir = Path(tempfile.mkdtemp(prefix="aether-vm-smoke-"))
    print(f"[INFO] 日志目录: {log_dir}")

    code, detail = run_qemu(iso, args.timeout, log_dir)

    if code == 0:
        print(f"[PASS] {detail}")
        if not args.keep_logs:
            shutil.rmtree(log_dir, ignore_errors=True)
            print("[INFO] 临时日志已清理（--keep-logs 可保留）")
        else:
            print(f"[INFO] 日志保留于: {log_dir}")
        return 0

    print(f"[FAIL] {detail}")
    # 失败日志落到 dist/vm-smoke-logs/ 便于 artifact 上传与本地排查。
    keep_dir = ROOT / "dist" / "vm-smoke-logs"
    keep_dir.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    dest = keep_dir / f"{stamp}"
    shutil.copytree(log_dir, dest, dirs_exist_ok=True)
    print(f"[INFO] 失败日志已保存: {dest}")
    return FAIL


if __name__ == "__main__":
    raise SystemExit(main())
