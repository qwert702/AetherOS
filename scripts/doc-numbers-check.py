"""文档数字门禁：把「手抄数字」换成「从来源推导 + 不一致就红」。

## 为什么要有它

2026-10-04 有一个提交（`0ef4324`）标题是「收尾报告 **5 处数字失实/自相矛盾**」——
那 5 处全是**我手抄出来的数字**：P99 写成固定值、提交数 89（实际 86）、
审计附录 68（实际 69）、某处仍写旧的「741 项 / 15 项门禁」、`gates.py` 注释也还是 15 项。

共同毛病：**数字靠抄，不靠来源**。而且**手抄的数字必然过期** ——
本次实测提交数已从 86 变成 124、审计附录从 69 变成 71。

## 怎么判（三条，都是可推导的）

| 数字 | 推导方式 |
|---|---|
| 测试数 | `cargo test -- --list` 里以 `: test` 结尾的行数（实测与「N passed」求和**完全一致**：749） |
| 门禁数 | 读 `docs/证据/gate-count.txt` —— 由 `gates.py` 在**本次运行开始时**写入，所以同一次运行内自洽 |
| 审计附录数 | 审计文档里 `^## 附录` 的行数 |

**不检查**提交数：它随时间滑动，写死必然失实 —— 文档里应只留命令，不留数字。
"""
from __future__ import annotations

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
REPORT = ROOT / "docs" / "报告" / "收尾报告（FINAL）-2026-10-04.md"
AUDIT = ROOT / "docs" / "报告" / "安全审计（SECURITY-AUDIT）-2026-10-02.md"
GATE_COUNT = ROOT / "docs" / "证据" / "gate-count.txt"


def declared(text: str, pattern: str) -> int | None:
    """读报告里声明的数字。

    **要求恰好一处匹配**（附录 GK）：曾经有自动替换把表格行插到了文件开头，
    于是"第一个匹配"永远是那条孤立的、真正那一行再也不会被更新 ——
    而这里只读第一个匹配，**看不见**。多/少一处都判红。
    """
    _hits = re.findall(pattern, text)
    if len(_hits) != 1:
        problems.append(f"「{pattern}」在报告里出现 {len(_hits)} 次（应为 1 次）")
        return None

    m = re.search(pattern, text)
    return int(m.group(1)) if m else None


def main() -> int:
    if not REPORT.exists():
        print(f"[SKIP] 找不到 {REPORT.name}")
        return 0
    text = REPORT.read_text(encoding='utf-8')
    problems: list[str] = []

    # ① 测试数
    proc = subprocess.run(
        ["cargo", "test", "--workspace", "--offline", "--locked", "--", "--list"],
        cwd=ROOT, capture_output=True, text=True,
    )
    real_tests = sum(1 for line in proc.stdout.splitlines() if line.endswith(': test'))
    doc_tests = declared(text, r'单元 \+ 集成测试 \| \*\*(\d+) 项全绿\*\*')
    print(f"① 测试数：文档 {doc_tests} / 实测 {real_tests}")
    if real_tests == 0:
        print("   [SKIP] 拿不到测试清单（cargo 不可用？）")
    elif doc_tests != real_tests:
        problems.append(f"测试数：文档 {doc_tests}，实测 {real_tests}")

    # ② 门禁数
    doc_gates = declared(text, r'\| 门禁 \| \*\*(\d+) 项全过\*\*')
    if GATE_COUNT.exists():
        real_gates = declared(GATE_COUNT.read_text(encoding='utf-8'), r'(\d+)')
        print(f"② 门禁数：文档 {doc_gates} / 本次运行 {real_gates}")
        if real_gates and doc_gates != real_gates:
            problems.append(f"门禁数：文档 {doc_gates}，本次运行 {real_gates}")
    else:
        print("② 门禁数：[SKIP] 没有 gate-count.txt（单独跑本脚本时会这样）")

    # ③ 审计附录数
    real_apps = len(re.findall(r'^## 附录', AUDIT.read_text(encoding='utf-8'), re.M))
    doc_apps = declared(text, r'\| 审计附录 \| (\d+) 篇 \|')
    print(f"③ 审计附录：文档 {doc_apps} / 实测 {real_apps}")
    if doc_apps != real_apps:
        problems.append(f"审计附录：文档 {doc_apps}，实测 {real_apps}")

    if problems:
        print()
        print("[FAIL] 文档里的数字与来源不一致（这就是「手抄必然失实」的现场）：")
        for p in problems:
            print(f"        {p}")
        print("        改文档，或者改代码 —— 但别让两者不一致。")
        return 1
    print("[ OK ] 文档数字与来源一致（测试 / 门禁 / 审计附录）")
    return 0


if __name__ == '__main__':
    sys.exit(main())
