#!/usr/bin/env python3
"""照判据的输出精确回填文档数字（**不要自己造正则**）。

## 为什么有这个脚本

2026-10-16 我连续三次用"宽正则替换"同步文档数字，每次都改坏别处：
- 把 INDEX 测试分布表的合计行改成带逗号的 `**51,522**` → 判据的正则 `(\\d+)` 不认逗号，
  报出来的是「没找到合计行」（不是数字对不上，更难查）；
- 把 `| **合计** | **443** | **799** |` 顺带改成了别的值 → 合计与各行求和对不上；
- 把逐文件表里 `aetherd` 的 `#[test]` 数改成 238 却把同一行的其它列也带偏。

根因是**我在"猜"判据要什么格式**。这个脚本反过来做：
先把判据（`scripts/repo-stats.py --check`、`scripts/doc-numbers-check.py`）跑一遍，
**从它打印的「声明 X / 实测 Y」里取值**，再按值精确替换 —— 判据说要什么，就写什么。

## 用法

    python scripts/follow-gate-numbers.py            # 迭代回填直到判据转绿（最多 4 轮）
    python scripts/follow-gate-numbers.py --dry-run  # 只打印打算改什么
    python scripts/follow-gate-numbers.py --selftest # 解析逻辑自检（不碰文件）

## 边界（诚实写清楚）

- 它**只改数字**（声明值 → 实测值），不改措辞、不加行、不删行；
- 判据说"漏了 1 个源文件"这类**结构性**问题它不处理 —— 那要人登记新文件；
- 值必须**逐个字面量**出现才替换（避免宽正则误伤）。
"""
from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PY = sys.executable

# 判据输出里的三种形态（见模块文档：都是"声明 X / 实测 Y"）
# 注意：`file` 这份要求**文件名里不含斜杠** —— 否则它会把"逐文件"那种
# `aetherd/src/main.rs: 声明 863 / 实测 879` 也匹配一遍，同一行被解析成两类发现
# （自检正是靠这条负向对照抓出来的）。
PAT_FILE = re.compile(
    r"\[FAIL\] (?P<file>[^\s/]+?): (?!测试表)(?P<what>[^\n]*?)声明 (?P<decl>[\d,]+)(?: 行)?[^\n]*?实测 (?P<real>[\d,]+)"
)
# 逐文件那类必须**带**斜杠，两者互补
PAT_PERFILE = re.compile(r"^\s+(?P<rel>\S+/\S+\.rs): 声明 (?P<decl>[\d,]+) / 实测 (?P<real>[\d,]+)", re.M)
PAT_TESTS = re.compile(r"\[FAIL\] (?P<file>\S+?): 测试表：`(?P<crate>[\w-]+)` 声明 (?P<decl>[\d,]+) 项[^\n]*实测 (?P<real>[\d,]+)")
PAT_SUM = re.compile(r"\[FAIL\] (?P<file>\S+?): 测试表合计：声明 \((?P<d1>[\d,]+), (?P<d2>[\d,]+)\)，各行相加为 \((?P<r1>[\d,]+), (?P<r2>[\d,]+)\)")


def parse_findings(text: str) -> list[dict]:
    """把判据输出解析成「哪个文件、哪个值、声明多少、实测多少」。**纯函数**，便于自检。"""
    out: list[dict] = []
    for m in PAT_SUM.finditer(text):
        out.append({"kind": "sum", "file": m.group("file"),
                    "decl": (m.group("d1"), m.group("d2")), "real": (m.group("r1"), m.group("r2"))})
    for m in PAT_TESTS.finditer(text):
        out.append({"kind": "tests", "file": m.group("file"), "crate": m.group("crate"),
                    "decl": (m.group("decl"),), "real": (m.group("real"),)})
    for m in PAT_PERFILE.finditer(text):
        out.append({"kind": "perfile", "rel": m.group("rel"),
                    "decl": (m.group("decl"),), "real": (m.group("real"),)})
    for m in PAT_FILE.finditer(text):
        out.append({"kind": "file", "file": m.group("file"), "what": m.group("what").strip(),
                    "decl": (m.group("decl"),), "real": (m.group("real"),)})
    return out


def apply_findings(findings: list[dict], dry_run: bool) -> int:
    changed = 0
    for f in findings:
        if f["kind"] == "perfile":
            p = ROOT / "docs" / "代码索引（INDEX）-2026-09-11.md"
            base = f["rel"].split("/", 1)[1]
            s = p.read_text(encoding="utf-8")
            new = re.sub(rf"(\| `{re.escape(base)}` \()\s*{re.escape(f['decl'][0])}\s*(\))",
                         rf"\g<1>{f['real'][0]}\g<2>", s, count=1)
            if new != s and not dry_run:
                p.write_text(new, encoding="utf-8", newline="")
            if new != s:
                changed += 1
                print(f"  [改] INDEX 逐文件 {f['rel']}：{f['decl'][0]} → {f['real'][0]}")
            continue
        p = ROOT / f["file"]
        if not p.is_file():
            continue
        s = p.read_text(encoding="utf-8")
        before = s
        if f["kind"] == "sum":
            # 合计行的两个数：只替换紧挨着出现的那对字面量
            before_pair = f"**{f['decl'][0]}** | **{f['decl'][1]}**"
            after_pair = f"**{f['real'][0]}** | **{f['real'][1]}**"
            s = s.replace(before_pair, after_pair)
        else:
            for d, r in zip(f["decl"], f["real"]):
                if d == r:
                    continue
                # 只替换"成对出现"的那一处（声明值 + 其所在行/文档内的位置）
                if f["kind"] == "tests":
                    s = re.sub(rf"(\| `{re.escape(f['crate'])}` \| )\d+( \| ){re.escape(d)}( \|)",
                               rf"\g<1>\g<2>{r}\g<3>", s, count=1)
                else:
                    s = s.replace(d, r, 1)
        if s != before:
            changed += 1
            if not dry_run:
                p.write_text(s, encoding="utf-8", newline="")
            print(f"  [改] {f['file']}：{f['decl']} → {f['real']}")
    return changed


def run_checks() -> tuple[str, str]:
    stats = subprocess.run([PY, "scripts/repo-stats.py", "--check"], cwd=ROOT,
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
    nums = subprocess.run([PY, "scripts/doc-numbers-check.py"], cwd=ROOT,
                          capture_output=True, text=True, encoding="utf-8", errors="replace")
    return stats.stdout, nums.stdout


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="照判据输出精确回填文档数字")
    ap.add_argument("--dry-run", action="store_true", help="只打印打算改什么")
    ap.add_argument("--selftest", action="store_true", help="解析逻辑自检（不碰文件）")
    ap.add_argument("--rounds", type=int, default=4, help="最多迭代轮数（默认 4）")
    args = ap.parse_args(argv)

    if args.selftest:
        sample = (
            "[FAIL] README.md: 声明 51,849 行 / 72 文件，实测 51,849 行 / 73 文件\n"
            "[FAIL] 代码索引（INDEX）-2026-09-11.md 逐文件行数：2 条不一致（已核对 73 条）\n"
            "        aetherd/src/main.rs: 声明 863 / 实测 879\n"
            "[FAIL] 代码索引（INDEX）-2026-09-11.md: 测试表：`aetherd` 声明 234 项 `#[test]`，实测 238\n"
            "[FAIL] 代码索引（INDEX）-2026-09-11.md: 测试表合计：声明 (443, 799)，各行相加为 (443, 803)\n"
        )
        fs = parse_findings(sample)
        kinds = sorted(x["kind"] for x in fs)
        checks = [
            (kinds == ["file", "perfile", "sum", "tests"], f"四类发现都能解析（实得 {kinds}）"),
            (any(x["kind"] == "perfile" and x["rel"] == "aetherd/src/main.rs" and x["real"] == ("879",) for x in fs),
             "逐文件行数解析出 (aetherd/src/main.rs, 879)"),
            (any(x["kind"] == "tests" and x["crate"] == "aetherd" and x["real"] == ("238",) for x in fs),
             "测试表行解析出 (aetherd, 238)"),
            (any(x["kind"] == "sum" and x["real"] == ("443", "803") for x in fs),
             "合计行解析出 (443, 803)"),
            (parse_findings("") == [] and parse_findings("[OK  ] 一切正常") == [],
             "没有 FAIL 时不产生任何发现（负向对照）"),
        ]
        ok = True
        for cond, what in checks:
            print(("  [OK  ] " if cond else "  [FAIL] ") + what)
            ok = ok and cond
        print("自检" + ("全部通过" if ok else "失败"))
        return 0 if ok else 1

    total = 0
    for i in range(1, args.rounds + 1):
        stats, nums = run_checks()
        findings = parse_findings(stats) + parse_findings(nums)
        if not findings:
            print(f"第 {i} 轮：判据已一致，无需回填")
            break
        print(f"第 {i} 轮：按判据回填 {len(findings)} 条")
        total += apply_findings(findings, args.dry_run)
        if args.dry_run:
            break
    else:
        print("已达最大轮数，仍有不一致 —— 请人看一眼（可能是结构性问题，不是数字问题）", file=sys.stderr)
        return 1
    print(f"共回填 {total} 处")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
