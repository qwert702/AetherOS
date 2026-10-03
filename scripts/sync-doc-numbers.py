#!/usr/bin/env python3
"""文档数字同步：把 `repo-stats.py --check` 指出的差异写回文档。

## 为什么要有这个脚本（而不是每次临时写）

这些同步逻辑此前以临时脚本形式存在 `%TEMP%`，被系统清理过**三次**，
每次都得重写 —— 而且清理后调用方还会"静默跳过"（`if 文件存在` 之类的守卫），
于是"我跑了同步"变成"我跑了什么都没做的命令"。放进仓库就没有这个问题。

## 它做什么

读 `repo-stats.py --check` 的输出，逐类替换并**循环到收敛**（数字之间存在联动：
总量由逐 crate 相加得出，逐 crate 又由逐文件相加得出，所以单趟修不干净）。

覆盖五类，每一类都对应报告里出现过的一种漂移：
1. 总量（四种措辞 + INDEX 合计行）
2. 逐 crate 行数/文件数
3. 逐文件行数
4. 测试分布表逐 crate 的 `#[test]` 计数
5. 测试分布表的合计行（**按各行实际相加**，不信报告里写的）

用法：
    python scripts/sync-doc-numbers.py            # 同步
    python scripts/sync-doc-numbers.py --dry-run  # 只报要改什么
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run_check() -> str:
    return subprocess.run(
        [sys.executable, "scripts/repo-stats.py", "--check"],
        cwd=ROOT, capture_output=True, text=True, encoding="utf-8",
    ).stdout


def find_doc(name: str) -> Path | None:
    """按文件名在仓库里找（文档搬过家，路径不能写死）。"""
    direct = ROOT / name
    if direct.exists():
        return direct
    hits = [q for q in ROOT.rglob(name) if "target" not in q.parts and ".git" not in q.parts]
    return hits[0] if hits else None


def read(p: Path) -> str:
    return p.read_text(encoding="utf-8")


def write(p: Path, s: str, dry: bool) -> None:
    if not dry:
        p.write_text(s, encoding="utf-8")


def sync_once(dry: bool) -> int:
    """跑一趟同步，返回改动处数。"""
    out = run_check()
    changed = 0

    # ---- 1) 总量 ----
    for m in re.finditer(r"\[FAIL\] (.+?): 声明 ([\d,]+) 行 / (\d+) 文件，实测 ([\d,]+) 行 / (\d+) 文件", out):
        fname, ol, of, nl, nf = m.groups()
        p = find_doc(fname.strip())
        if p is None:
            continue
        s = before = read(p)
        for pat, rep in [
            (f"{ol} 行 / {of} 文件", f"{nl} 行 / {nf} 文件"),
            (f"{ol} 行 / {of} 个源文件", f"{nl} 行 / {nf} 个源文件"),
            (f"{ol} 行 / {of} 个 .rs", f"{nl} 行 / {nf} 个 .rs"),
            (f"{ol} 行 Rust / {of} 个源文件", f"{nl} 行 Rust / {nf} 个源文件"),
            (f"{ol} 行 Rust / {of} 个源文件 /", f"{nl} 行 Rust / {nf} 个源文件 /"),
            (f"| **合计** | **{ol}** | **{of}** | |", f"| **合计** | **{nl}** | **{nf}** | |"),
            (f"| **合计** | **{ol}** | **{of}** |", f"| **合计** | **{nl}** | **{nf}** |"),
        ]:
            s = s.replace(pat, rep)
        if s != before:
            write(p, s, dry)
            changed += 1
            print(f"  总量 {p.name}: {ol}/{of} → {nl}/{nf}")

    # ---- 2) 逐 crate ----
    # 两种 FAIL 形态都要认：带文件数（INDEX/ROADMAP）与不带（README 的"声明 X 行，实测 Y 行"）。
    crate_fails = []
    for m in re.finditer(r"\[FAIL\] (.+?): `([a-z\-]+)` 声明 ([\d,]+) 行 / (\d+) 文件，实测 ([\d,]+) 行 / (\d+) 文件", out):
        crate_fails.append((m.group(1), m.group(2), m.group(3), m.group(4), m.group(5), m.group(6)))
    for m in re.finditer(r"\[FAIL\] (.+?): `([a-z\-]+)` 声明 ([\d,]+) 行，实测 ([\d,]+) 行", out):
        crate_fails.append((m.group(1), m.group(2), m.group(3), None, m.group(4), None))
    for fname, crate, ol, of, nl, nf in crate_fails:
        p = find_doc(fname.strip())
        if p is None:
            continue
        s = before = read(p)
        for pat, rep in [
            (f"| `{crate}/` | {ol} |", f"| `{crate}/` | {nl} |"),
            (f"`{crate}` {ol} 行", f"`{crate}` {nl} 行"),
        ] + ([
            (f"| `{crate}` | {ol} | {of} |", f"| `{crate}` | {nl} | {nf} |"),
            (f"| `{crate}/` | {ol} | {of} |", f"| `{crate}/` | {nl} | {nf} |"),
        ] if of and nf else []) + [
        ]:
            s = s.replace(pat, rep)
        if s != before:
            write(p, s, dry)
            changed += 1
            print(f"  逐 crate {p.name} {crate}: {ol}/{of} → {nl}/{nf}")

    # ---- 3) 逐文件 ----
    for m in re.finditer(r"^\s+(\S+\.rs): 声明 ([\d,]+) / 实测 ([\d,]+)\s*$", out, re.M):
        path, old_n, new_n = m.groups()
        leaf = path.split("/")[-1]
        # 出现在哪份文档里就把哪份改掉（只改含该行的那份）
        for q in [find_doc("代码索引（INDEX）-2026-09-11.md")]:
            if q is None:
                continue
            lines = read(q).split("\n")
            hit = False
            for i, l in enumerate(lines):
                if not l.startswith("|") or leaf not in l:
                    continue
                if f"({old_n})" in l:
                    lines[i] = l.replace(f"({old_n})", f"({new_n})")
                    hit = True
                else:
                    mm = re.match(r"^(\| `[^`]*" + re.escape(leaf) + r"` \| )([\d,]+)( \|.*)$", l)
                    if mm and mm.group(2) == old_n:
                        lines[i] = f"{mm.group(1)}{new_n}{mm.group(3)}"
                        hit = True
                if hit:
                    break
            if hit:
                write(q, "\n".join(lines), dry)
                changed += 1
                print(f"  逐文件 {leaf}: {old_n} → {new_n}")

    # ---- 4) 测试分布表：逐 crate 的 #[test] 计数 ----
    for m in re.finditer(r"测试表：`([a-z\-]+)` 声明 (\d+) 项 `#\[test\]`，实测 (\d+)", out):
        crate, new_n = m.group(1), m.group(3)
        q = find_doc("代码索引（INDEX）-2026-09-11.md")
        if q is None:
            continue
        lines = read(q).split("\n")
        hdr = next((i for i, l in enumerate(lines) if l.startswith("| crate |") and "Windows（实测）" in l), None)
        if hdr is None:
            continue
        for i in range(hdr, min(hdr + 14, len(lines))):
            if lines[i].startswith(f"| `{crate}` |"):
                parts = lines[i].split("|")
                if len(parts) >= 5 and parts[3].strip() != new_n:
                    parts[3] = f" {new_n} "
                    lines[i] = "|".join(parts)
                    write(q, "\n".join(lines), dry)
                    changed += 1
                    print(f"  测试表 {crate} → {new_n}")
                break

    # ---- 5) 测试分布表：合计行（按各行相加，不信报告里写的）----
    q = find_doc("代码索引（INDEX）-2026-09-11.md")
    if q is not None:
        lines = read(q).split("\n")
        hdr = next((i for i, l in enumerate(lines) if l.startswith("| crate |") and "Windows（实测）" in l), None)
        if hdr is not None:
            w = a = 0
            for i in range(hdr + 2, min(hdr + 14, len(lines))):
                if lines[i].startswith("| **合计** |"):
                    parts = lines[i].split("|")
                    # 比较时要去掉 `**`（行里是 `| **443** | **500** |`，直接比字符串会永远"不同"→ 反复重写）
                    if len(parts) >= 5 and (
                        parts[2].strip().strip("*") != str(w) or parts[3].strip().strip("*") != str(a)
                    ):
                        lines[i] = f"| **合计** | **{w}** | **{a}** |"
                        write(q, "\n".join(lines), dry)
                        changed += 1
                        print(f"  测试表合计 → {w} / {a}")
                    break
                parts = lines[i].split("|")
                if len(parts) >= 5 and parts[2].strip().isdigit() and parts[3].strip().isdigit():
                    w += int(parts[2])
                    a += int(parts[3])

    return changed


def main() -> int:
    dry = "--dry-run" in sys.argv
    total = 0
    for i in range(1, 7):
        n = sync_once(dry)
        total += n
        if n == 0:
            print(f"第 {i} 轮无改动，停止")
            break
        if dry:
            break
    out = run_check()
    bad = [l for l in out.splitlines() if "FAIL" in l]
    if bad:
        print(f"仍有 {len(bad)} 条未同步：")
        for l in bad[:8]:
            print("  " + l.strip()[:150])
        return 1
    print(f"已收敛：共改动 {total} 处，`repo-stats.py --check` 全部通过 ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())
