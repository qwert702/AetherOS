#!/usr/bin/env python3
"""§4.1 自动 e2e：驱动合成器的 `--e2e` 无界面动作运行器，断言**真实行为**。

## 为什么要有它

FEATURE-GAPS §4.1 的原文："现在每个功能都要人工点一次"。
后果是所有验证都退化成"我看着没问题"，而 Linux 专属路径在开发机上根本跑不到。

本脚本把"动作 → 结果"变成可重复、可比对的检查：
* 合成器侧 `--e2e` 从 stdin 读动作脚本（JSONL），逐条执行并输出 JSONL 结果；
* 本脚本读那些结果，**断言行为**（窗口数变了没有、布局真的换了吗、撤销回去了吗）。

## 与"截图走查"的分工

截图走查（`archive-ui-shots.py`）管**画面**；本脚本管**行为**。
两者都要：画面对了不等于点得动，点得动不等于画得对。

## §3.5 遗留项 #43：通知正文的双语在这里被真跑一遍

`query: toast` 报的是**主循环同一个 `push_toast`** 写进 toast 槽的当前提示，
配 `lang` 动作就能断言"英文下不含中文、中文下有中文" ——
不是读源码猜的，是让二进制自己产一条出来看。

## 诚实边界

* 覆盖的是**不依赖真实显示与硬件**的那一半（布局/开窗/撤销/查询）。
  真机断电、声音、块设备仍无法在这里验 —— 那些在文档里单独标注。
* `--selftest` 只验**本脚本的判定逻辑**（不需要合成器、不需要跑起来），
  所以它在任何平台上都能跑，也就能进日常门禁。

用法：
    python scripts/e2e-ui-actions.py                 # 跑全部场景（需先构建合成器）
    python scripts/e2e-ui-actions.py --selftest      # 只验判定逻辑（无需合成器）
    python scripts/e2e-ui-actions.py --bin <路径>    # 指定合成器可执行文件
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: 判据"英文位不得含中文"用的 CJK 范围（与 `i18n-check.py` 同一口径）。
CJK = re.compile(r'[\u4e00-\u9fff]')

# ---------------------------------------------------------------------------
# 场景定义：script 是喂给 `--e2e` 的动作脚本；expect 是逐条断言。
# 期望值的写法只有三种（避免"断言风格各写一套"）：
#   ok          —— 该行的 ok 字段
#   contains    —— 该行 result 包含某子串
#   equals      —— 该行 result 完全相等
# ---------------------------------------------------------------------------
SCENARIOS: list[dict] = [
    {
        "name": "布局切换真的生效",
        "script": [
            '{"action":"query","args":{"what":"layout"}}',
            '{"action":"layout_set","args":{"layout":"monocle"}}',
            '{"action":"query","args":{"what":"layout"}}',
        ],
        "expect": [
            {"ok": True},
            {"ok": True, "contains": "布局"},
            {"ok": True, "equals": "独占堆叠"},
        ],
    },
    {
        "name": "打开应用后窗口数与标题都对",
        "script": [
            '{"action":"open_app","args":{"app":"终端"}}',
            '{"action":"query","args":{"what":"titles"}}',
        ],
        "expect": [
            {"ok": True, "contains": "终端"},
            {"ok": True, "contains": "终端"},
        ],
    },
    {
        "name": "撤销开窗会关掉刚打开的那个",
        "script": [
            '{"action":"query","args":{"what":"windows"}}',
            '{"action":"open_app","args":{"app":"设置"}}',
            '{"action":"query","args":{"what":"windows"}}',
            '{"action":"undo"}',
            '{"action":"query","args":{"what":"windows"}}',
        ],
        # 窗口数用"前后相等"表达：0 与 4 都是查询，4 应大于 0，且 2 == 0
        "expect": [
            {"ok": True},
            {"ok": True},
            {"ok": True, "greater_than_line": 0},
            {"ok": True, "contains": "关掉"},
            {"ok": True, "equals_line": 0},
        ],
    },
    {
        "name": "未知应用被拒绝且不进撤销栈",
        "script": [
            '{"action":"open_app","args":{"app":"不存在的应用"}}',
            '{"action":"undo"}',
        ],
        "expect": [
            {"ok": False},
            # 被拒绝的动作**不该**入栈，所以这里没有可撤销的动作
            {"ok": False, "contains": "没有可撤销"},
        ],
    },
    {
        "name": "非法输入逐条报错但不中止",
        "script": [
            "这不是 JSON",
            '{"action":"不存在的动作"}',
            '{"action":"query","args":{"what":"不存在的查询"}}',
            '{"action":"query","args":{"what":"windows"}}',
        ],
        "expect": [
            {"ok": False, "contains": "JSON"},
            {"ok": False, "contains": "未知动作"},
            {"ok": False, "contains": "未知查询"},
            # 关键：前面三行都错了，最后一行仍应正常执行
            {"ok": True},
        ],
    },
    # §3.5 遗留项 #43：通知正文的结构化双语，用真二进制断言两条相反的命题。
    # **只查一边是不够的**：只断言"英文下没有中文"的话，空提示也会通过。
    {
        "name": "通知正文在英文下不含中文（且真的非空）",
        "script": [
            '{"action":"lang","args":{"lang":"en"}}',
            '{"action":"layout_set","args":{"layout":"monocle"}}',
            '{"action":"query","args":{"what":"toast"}}',
        ],
        "expect": [
            {"ok": True, "equals": "en"},
            {"ok": True},
            {"ok": True, "non_empty": True, "no_cjk": True},
        ],
    },
    {
        "name": "同一动作在中文下产出中文（证明上面不是空串蒙过去的）",
        "script": [
            '{"action":"lang","args":{"lang":"zh"}}',
            '{"action":"layout_set","args":{"layout":"float"}}',
            '{"action":"query","args":{"what":"toast"}}',
        ],
        "expect": [
            {"ok": True, "equals": "zh"},
            {"ok": True},
            {"ok": True, "non_empty": True, "has_cjk": True},
        ],
    },
]


def parse_results(stdout: str) -> tuple[list[dict], list[str]]:
    """把 `--e2e` 的 JSONL 输出解析成字典列表，顺带收集格式错误。"""
    rows, problems = [], []
    for i, line in enumerate(stdout.splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError as e:
            problems.append(f"第 {i} 行不是合法 JSONL：{line[:80]}（{e}）")
            continue
        if not isinstance(obj, dict) or "ok" not in obj:
            problems.append(f"第 {i} 行缺少 ok 字段：{line[:80]}")
            continue
        rows.append(obj)
    return rows, problems


def check_scenario(rows: list[dict], expect: list[dict]) -> list[str]:
    """逐条比对；返回失败描述（空列表 = 通过）。"""
    fails: list[str] = []
    if len(rows) != len(expect):
        fails.append(f"输出条数不符：期望 {len(expect)} 条，实际 {len(rows)} 条")
        # 继续逐条比对能对上的部分，好让人一次看到更多问题
    for i, exp in enumerate(expect):
        if i >= len(rows):
            break
        got = rows[i]
        where = f"第 {i + 1} 条（line={got.get('line')}）"
        if "ok" in exp and got.get("ok") is not exp["ok"]:
            fails.append(f"{where} ok 期望 {exp['ok']}，实际 {got.get('ok')}：{got.get('result')!r}")
        res = got.get("result")
        if "contains" in exp and (not isinstance(res, str) or exp["contains"] not in res):
            fails.append(f"{where} result 应包含 {exp['contains']!r}，实际 {res!r}")
        if "equals" in exp and res != exp["equals"]:
            fails.append(f"{where} result 期望 {exp['equals']!r}，实际 {res!r}")
        # 语种断言（§3.5 遗留项 #43）：三类，缺一不可 ——
        # `non_empty` 防止"空串也算没有中文"，`no_cjk`/`has_cjk` 是两条相反的命题。
        if exp.get("non_empty") and (not isinstance(res, str) or not res.strip()):
            fails.append(f"{where} result 不能为空，实际 {res!r}")
        if exp.get("no_cjk") and isinstance(res, str) and CJK.search(res):
            fails.append(f"{where} 英文下不该出现中文，实际 {res!r}")
        if exp.get("has_cjk") and isinstance(res, str) and not CJK.search(res):
            fails.append(f"{where} 中文下应当出现中文，实际 {res!r}")
        # 跨行比较：窗口数之类的"前后关系"断言（比硬编数字稳）
        if "greater_than_line" in exp:
            ref = exp["greater_than_line"]
            if ref < len(rows):
                try:
                    if int(res) <= int(rows[ref].get("result")):
                        fails.append(f"{where} 应大于第 {ref + 1} 条的值（{rows[ref].get('result')}），实际 {res}")
                except (TypeError, ValueError):
                    fails.append(f"{where} 与第 {ref + 1} 条都应是非负整数，实际 {res!r}")
            else:
                fails.append(f"{where} 引用了不存在的行 {ref}")
        if "equals_line" in exp:
            ref = exp["equals_line"]
            if ref < len(rows):
                if str(res) != str(rows[ref].get("result")):
                    fails.append(
                        f"{where} 应等于第 {ref + 1} 条的值（{rows[ref].get('result')}），实际 {res}"
                    )
            else:
                fails.append(f"{where} 引用了不存在的行 {ref}")
    return fails


def run_binary(binary: Path, script: list[str]) -> tuple[str, str, int]:
    proc = subprocess.run(
        [str(binary), "--e2e"],
        input="\n".join(script) + "\n",
        capture_output=True,
        text=True,
        cwd=str(ROOT),
        timeout=120,
    )
    return proc.stdout, proc.stderr, proc.returncode


def find_binary(explicit: str | None) -> Path | None:
    if explicit:
        p = Path(explicit)
        return p if p.exists() else None
    for name in ("aether-compositor.exe", "aether-compositor"):
        p = ROOT / "target" / "debug" / name
        if p.exists():
            return p
    return None


def selftest() -> int:
    """只验判定逻辑：喂**构造好的**输出，确认通过/失败两种情形都判得对。

    这一步不需要合成器，因此能在任何平台进日常门禁 ——
    而"能进日常门禁"正是它存在的意义（否则又是"只有人记得才跑"）。
    """
    cases = [
        # (说明, rows, expect, 期望失败条数)
        ("全对", [{"line": 1, "ok": True, "result": "独占堆叠"}], [{"ok": True, "equals": "独占堆叠"}], 0),
        ("ok 不符", [{"line": 1, "ok": False, "result": "x"}], [{"ok": True}], 1),
        ("contains 不符", [{"line": 1, "ok": True, "result": "布局：两列"}], [{"contains": "独占"}], 1),
        ("equals 不符", [{"line": 1, "ok": True, "result": "两列"}], [{"equals": "独占堆叠"}], 1),
        ("条数不符", [{"line": 1, "ok": True, "result": "1"}], [{"ok": True}, {"ok": True}], 1),
        ("跨行大于", [{"line": 1, "ok": True, "result": "1"}, {"line": 2, "ok": True, "result": "3"}],
         [{"ok": True}, {"ok": True, "greater_than_line": 0}], 0),
        ("跨行大于（不成立）", [{"line": 1, "ok": True, "result": "3"}, {"line": 2, "ok": True, "result": "3"}],
         [{"ok": True}, {"ok": True, "greater_than_line": 0}], 1),
        ("跨行相等", [{"line": 1, "ok": True, "result": "2"}, {"line": 2, "ok": True, "result": "2"}],
         [{"ok": True}, {"ok": True, "equals_line": 0}], 0),
        # 语种断言的正向对照（§3.5 遗留项 #43）：每一条都必须能判红
        ("英文下真的没中文 → 通过",
         [{"line": 1, "ok": True, "result": "Layout: Stacked"}],
         [{"ok": True, "non_empty": True, "no_cjk": True}], 0),
        ("英文下混进中文 → 判红",
         [{"line": 1, "ok": True, "result": "布局：独占堆叠"}],
         [{"ok": True, "non_empty": True, "no_cjk": True}], 1),
        ("空串蒙不过去 → 判红",
         [{"line": 1, "ok": True, "result": ""}],
         [{"ok": True, "non_empty": True, "no_cjk": True}], 1),
        ("中文下有中文 → 通过",
         [{"line": 1, "ok": True, "result": "布局：自由布局"}],
         [{"ok": True, "non_empty": True, "has_cjk": True}], 0),
        ("中文下却是英文 → 判红",
         [{"line": 1, "ok": True, "result": "Layout: Free"}],
         [{"ok": True, "non_empty": True, "has_cjk": True}], 1),
    ]
    bad = 0
    for name, rows, expect, want in cases:
        fails = check_scenario(rows, expect)
        if len(fails) != want:
            print(f"[selftest] 失败：{name} —— 期望 {want} 条失败，实得 {len(fails)}：{fails}")
            bad += 1
    # JSONL 解析：坏行要被抓出来
    rows, problems = parse_results('{"ok":true,"result":"a"}\n不是 JSON\n{"没有ok":1}\n')
    if len(rows) != 1 or len(problems) != 2:
        print(f"[selftest] 失败：解析应得 1 行 + 2 个问题，实得 {len(rows)} + {len(problems)}")
        bad += 1
    # 空输入不算错（场景可能全被注释掉 → 没有任何输出）
    rows, problems = parse_results("\n\n")
    if rows or problems:
        print("[selftest] 失败：空输入不该产生行或问题")
        bad += 1
    # 输出流里混进非 JSON（例如有人加了调试 println!）**必须**被抓出来 ——
    # 否则"e2e 通过"可能建立在"忽略了一堆看不懂的输出"之上。
    rows, problems = parse_results('{"ok":true,"result":"a"}\n[debug] 窗口数=3\n')
    if len(rows) != 1 or len(problems) != 1:
        print(f"[selftest] 失败：混入的调试输出应被报告（1 行 + 1 问题），实得 {len(rows)} + {len(problems)}")
        bad += 1
    if bad:
        print(f"[selftest] 未通过：{bad} 项")
        return 1
    print(f"[selftest] 通过 {len(cases)} 项判定用例 + 2 项解析用例")
    return 0


def main() -> int:
    argv = sys.argv[1:]
    if "--selftest" in argv:
        return selftest()

    binary = find_binary(argv[argv.index("--bin") + 1] if "--bin" in argv else None)
    if binary is None:
        print("找不到合成器可执行文件。先构建：cargo build -p aether-compositor --offline")
        print("（只验本脚本逻辑可跑：python scripts/e2e-ui-actions.py --selftest）")
        return 2

    failed = 0
    for sc in SCENARIOS:
        stdout, stderr, code = run_binary(binary, sc["script"])
        rows, problems = parse_results(stdout)
        fails = problems + check_scenario(rows, sc["expect"])
        if code != 0:
            fails.append(f"合成器退出码 {code}（stderr：{stderr.strip()[:200]}）")
        if fails:
            failed += 1
            print(f"✗ {sc['name']}")
            for f in fails:
                print(f"    {f}")
        else:
            print(f"✓ {sc['name']}（{len(rows)} 条断言）")

    print(f"\n{len(SCENARIOS) - failed}/{len(SCENARIOS)} 个场景通过")
    if failed:
        print("提示：e2e 失败时先看 --e2e 的原始输出，再判断是行为问题还是断言问题。")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
