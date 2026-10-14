#!/usr/bin/env python3
"""台账门禁：`功能缺口清单` §9.1 的**类别数与表内实际行数**必须一致。

## 为什么要有它

§9.1 表头写着「**本表共 50 行，按类别数**（由脚本按上表实际统计，不是手数）」——
2026-10-11 复核发现：**仓库里没有任何脚本在统计它**，那 5 个数字是人手更新的。

这正是本仓库反复踩过的坑（数字靠抄、不靠来源）：`doc-numbers-check.py` 管住了
测试数 / 门禁数 / 审计附录数，README 与 INDEX 的规模数由 `repo-stats.py --check` 管住，
**唯独这张台账的类别数是"自称由脚本统计、实际没人统计"**。

## 判什么

1. §9.1 表里每一行的类别 → 归到 ✅/🟡/⛔/🚫/📋 五类，逐类计数；
2. 与表头那句声明的五个数字 + 合计逐一比对；
3. 行数声明（「本表共 N 行」）与表内行数比对。

不一致 → 判红并逐项列出（**多一处、少一处都报**，不允许"差不多"）。

## 用法

    python scripts/ledger-count-check.py             # 门禁
    python scripts/ledger-count-check.py --selftest  # 判定逻辑自检（含正向对照）
"""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LEDGER = ROOT / "docs" / "报告" / "功能缺口清单（FEATURE-GAPS）-2026-10-02.md"

#: 类别前缀 → 表头声明里用的名字（顺序即打印顺序）
CATEGORIES = [
    ("✅", "✅ 已完成"),
    ("🟡", "🟡 部分"),
    ("⛔", "⛔ 阻塞"),
    ("🚫", "🚫 有意不做"),
    ("📋", "📋 待办/已记录"),
]

SECTION_RE = re.compile(r"^###\s*9\.1\s*(.*?)^###\s*9\.2", re.S | re.M)
ROW_RE = re.compile(r"^\|\s*(\d+)\s*\|(.+?)\|(.+?)\|", re.M)
#: 声明句原文形如：`**本表共 50 行，按类别数**（由脚本…）：`
#: —— `本表共 N 行` 两边被 `**` 包着，但 N 本身没有星号（初版正则错写成 `\*\*(\d+)`，
#: 于是永远解析不出来，自检当场抓住）。
DECL_ROW_RE = re.compile(r"本表共\s*(\d+)\s*行")
DECL_CAT_RE = re.compile(r"(✅ 已完成|🟡 部分|⛔ 阻塞|🚫 有意不做|📋 待办/已记录)\s*\*\*(\d+)\*\*")
DECL_TOTAL_RE = re.compile(r"合计\s*\*\*(\d+)\*\*")


def parse(text: str) -> tuple[dict[str, int], dict[str, int], int, list[str]]:
    """返回 (实际计数, 声明计数, 声明的行数, 问题列表)。"""
    problems: list[str] = []
    m = SECTION_RE.search(text)
    if not m:
        return {}, {}, 0, ["找不到 §9.1 章节（标题格式可能被改动）"]
    body = m.group(1)

    actual = {name: 0 for _, name in CATEGORIES}
    rows = 0
    for _num, _item, cat in ROW_RE.findall(body):
        cat = cat.strip()
        rows += 1
        hit = next((name for prefix, name in CATEGORIES if cat.startswith(prefix)), None)
        if hit is None:
            problems.append(f"第 {_num} 行的类别无法归类：{cat[:40]!r}")
            continue
        actual[hit] += 1

    declared = {name: int(n) for name, n in DECL_CAT_RE.findall(body)}
    total_m = DECL_TOTAL_RE.search(body)
    declared_total = int(total_m.group(1)) if total_m else 0
    row_m = DECL_ROW_RE.search(body)
    declared_rows = int(row_m.group(1)) if row_m else 0
    if not declared or declared_total == 0 or declared_rows == 0:
        problems.append("表头声明句没解析出来（「本表共 N 行」/ 五个类别 / 合计）")
    return actual, declared, declared_rows, problems


def compare(actual: dict[str, int], declared: dict[str, int], declared_rows: int) -> list[str]:
    """逐项比对；返回差异描述（空 = 一致）。"""
    problems: list[str] = []
    total = sum(actual.values())
    for _prefix, name in CATEGORIES:
        got, want = actual.get(name, 0), declared.get(name)
        if want is None:
            problems.append(f"{name}：表头没有声明这个类别（表里有 {got} 行）")
        elif got != want:
            problems.append(f"{name}：表里 {got} 行 / 表头写 {want} 行")
    if declared_rows != total:
        problems.append(f"行数：表里 {total} 行 / 表头写 {declared_rows} 行")
    dtotal = sum(declared.values())
    if dtotal != total:
        problems.append(f"合计：表里 {total} 行 / 表头五个数字相加 {dtotal} 行")
    return problems


def selftest() -> int:
    """判定逻辑自检：一致要通过，**每一类不一致都要被判出来**（正向对照）。

    断言写法：坏用例要求"**指定问题必须出现**"，而不是数问题条数 ——
    条数会被"顺带多报的一条"带偏（第一版就这么错过一次），
    而"该报的有没有报"才是判据的实质。
    """
    good_body = """### 9.1 一张表

**类表说明**：随便写点。

**本表共 3 行，按类别数**（由脚本按上表实际统计，不是手数）：
✅ 已完成 **2** · 🟡 部分 **1** · ⛔ 阻塞 **0** · 🚫 有意不做 **0** · 📋 待办/已记录 **0**。合计 **3**。

| # | 事项 | 类别 | 现状 |
|---|---|---|---|
| 1 | 甲 | ✅ 已完成 | x |
| 2 | 乙 | ✅ 已完成 | x |
| 3 | 丙 | 🟡 部分 | x |

### 9.2 别的
"""
    cases = [
        ("完全一致", good_body, []),
        ("类别数写错一处", good_body.replace("✅ 已完成 **2**", "✅ 已完成 **1**"),
         ["✅ 已完成：表里 2 行 / 表头写 1 行"]),
        ("表头漏了一个类别", good_body.replace("🟡 部分 **1**", "🟡 部分 **0**"),
         ["🟡 部分：表里 1 行 / 表头写 0 行"]),
        ("行数声明错", good_body.replace("本表共 3 行", "本表共 4 行"),
         ["行数：表里 3 行 / 表头写 4 行"]),
        ("出现无法归类的类别", good_body.replace("| 3 | 丙 | 🟡 部分 | x |", "| 3 | 丙 | ❓ 未知 | x |"),
         ["类别无法归类"]),
        ("整个 9.1 章节消失", "# 没有这一节\n", ["找不到 §9.1 章节"]),
    ]
    failed = 0
    for label, body, must_contain in cases:
        actual, declared, rows, probs = parse(body)
        probs = list(probs) + compare(actual, declared, rows)
        joined = "\n".join(probs)
        if must_contain:
            missing = [f for f in must_contain if f not in joined]
            ok = not missing
            detail = f"缺少期望问题 {missing}" if missing else f"报告了 {probs}"
        else:
            ok = not probs
            detail = f"报告了 {probs}" if probs else "无问题"
        mark = "OK  " if ok else "FAIL"
        if not ok:
            failed += 1
        print(f"  [{mark}] {label}：{detail}")
    if failed:
        print(f"[FAIL] 台账门禁自检不通过：{failed} 个用例与期望不符")
        return 1
    print(f"[ OK ] 台账门禁自检通过（{len(cases)} 个用例，含 5 条正向对照）")
    return 0


def main() -> int:
    if "--selftest" in sys.argv:
        return selftest()
    text = LEDGER.read_text(encoding="utf-8")
    actual, declared, rows, problems = parse(text)
    problems = list(problems) + compare(actual, declared, rows)
    total = sum(actual.values())
    shown = " · ".join(f"{name} {actual.get(name, 0)}" for _p, name in CATEGORIES)
    print(f"§9.1 实际统计：{shown}（合计 {total} 行）")
    if problems:
        print(f"[FAIL] 台账与表内实际统计不一致：{len(problems)} 处")
        for p in problems:
            print(f"  - {p}")
        print("  改表格，或者改那句声明 —— 但别让两者不一致。")
        return 1
    print("[ OK ] 台账类别数与表内实际一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
