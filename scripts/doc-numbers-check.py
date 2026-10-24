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
import json
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
REPORT = ROOT / "docs" / "报告" / "收尾报告（FINAL）-2026-10-04.md"
README = ROOT / "README.md"
README_EN = ROOT / "README.en.md"
SITE = ROOT / "site"
# declared() 是模块级函数，不能碰 main() 的局部 problems（附录 GL）。
PROBLEMS: list = []
AUDIT = ROOT / "docs" / "报告" / "安全审计（SECURITY-AUDIT）-2026-10-02.md"
#: 代码索引（头部那句与逐 crate 表都是规模事实的另一种写法，2026-10-11 纳入 ③f）
INDEX = ROOT / "docs" / "代码索引（INDEX）-2026-09-11.md"
GATE_COUNT = ROOT / "docs" / "证据" / "gate-count.txt"


def declared(text: str, pattern: str) -> int | None:
    """读报告里声明的数字。

    **要求恰好一处匹配**（附录 GK）：曾经有自动替换把表格行插到了文件开头，
    于是"第一个匹配"永远是那条孤立的、真正那一行再也不会被更新 ——
    而这里只读第一个匹配，**看不见**。多/少一处都判红。
    """
    _hits = re.findall(pattern, text)
    if len(_hits) != 1:
        PROBLEMS.append(f"「{pattern}」出现 {len(_hits)} 次（应为 1 次）")
        return None

    m = re.search(pattern, text)
    return int(m.group(1)) if m else None


def main() -> int:
    if not REPORT.exists():
        print(f"[SKIP] 找不到 {REPORT.name}")
        return 0
    text = REPORT.read_text(encoding='utf-8')
    problems: list[str] = list(PROBLEMS)

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

    # 规模数：repo-stats 是**权威口径**，只跑一次，③b 与 ③f 共用
    _stats_out = subprocess.run([sys.executable, str(ROOT / "scripts/repo-stats.py")],
                                cwd=ROOT, capture_output=True, text=True,
                                encoding='utf-8').stdout
    _m_stats = re.search(r"\*\*合计\*\* \| \*\*([\d,]+)\*\* \| \*\*(\d+)\*\*", _stats_out)
    real_lines = int(_m_stats.group(1).replace(",", "")) if _m_stats else None
    real_files = int(_m_stats.group(2)) if _m_stats else None

    # ② 门禁数
    doc_gates = declared(text, r'\| 门禁 \| \*\*(\d+) 项全过\*\*')
    subset = False
    real_gates = None
    if GATE_COUNT.exists():
        # `utf-8-sig`：这份文件由 `gates.py` 写（无 BOM），但**人手工编辑时很容易带上 BOM** ——
        # 带上之后 `"30".isdigit()` 会因为开头那个 \ufeff 变成 False，判据退化成"期望 None"，
        # 报出来的错误看不懂。读的时候顺手把 BOM 吃掉（2026-10-11 实测踩到过）。
        raw = GATE_COUNT.read_text(encoding='utf-8-sig').split()
        real_gates = int(raw[0]) if raw and raw[0].isdigit() else None
        # 第二行是模式（`full` / `fast` / `ci`）。`--fast` 与 `--ci` 都只跑子集，
        # 拿它们的步数去比文档里的**全量**步数必然不相等 —— 那不是缺陷，是窗口取错了
        # （2026-10-11：`gates.py --fast` 一直是红的，红的就是这一条）。
        subset_mode = raw[1] if len(raw) > 1 else ""
        subset = subset_mode in ("fast", "ci")
        if subset:
            print(f"② 门禁数：[SKIP] 子集模式（--{subset_mode} 本次 {real_gates} 步，"
                  f"与文档声明的全量步数不可比）")
        elif real_gates and doc_gates != real_gates:
            problems.append(f"门禁数：文档 {doc_gates}，本次运行 {real_gates}")
        else:
            print(f"② 门禁数：文档 {doc_gates} / 本次运行 {real_gates}")
    else:
        print("② 门禁数：[SKIP] 没有 gate-count.txt（单独跑本脚本时会这样）")

    # ③b README 的数字（最常被读的文件，此前没人管 —— 附录 GL）
    if README.exists():
        rtext = README.read_text(encoding='utf-8')
        doc_readme_tests = declared(rtext, r"\| (\d+) 项单元测试全绿")
        print(f"③b README 测试数：{doc_readme_tests} / 实测 {real_tests}")
        if doc_readme_tests != real_tests:
            problems.append(f"README 测试数：文档 {doc_readme_tests}，实测 {real_tests}")
        doc_readme_files = declared(rtext, r"(\d+) 个源文件")
        print(f"③b README 源文件数：{doc_readme_files} / 实测 {real_files}")
        if doc_readme_files != real_files:
            problems.append(f"README 源文件数：文档 {doc_readme_files}，实测 {real_files}")

    # ③c 英文 README（中英成对，附录 GM）
    if README_EN.exists():
        etext = README_EN.read_text(encoding='utf-8')
        _m = re.search(r"(\d+)\s+unit tests?\s+(?:all\s+)?(?:green|pass)", etext, re.I)
        doc_en_tests = int(_m.group(1)) if _m else None
        print(f"③c README.en 测试数：{doc_en_tests} / 实测 {real_tests}")
        if doc_en_tests != real_tests:
            problems.append(f"README.en 测试数：文档 {doc_en_tests}，实测 {real_tests}")

    # ③d **同一事实的全部写法**（附录 GP）：门禁只认一种写法的话，
    # 第二种写法就会静默漂移（INDEX 头部行就是这么漏的，附录 GO）。
    for _label, _path, _pat, _expect in (
        ("README.md Windows 行", README, r"Windows (\d+) 项全绿", real_tests),
        ("README.en Beyond 行", README_EN, r"Beyond the (\d+) unit tests", real_tests),
        ("收尾报告 自引用行", REPORT, r"「(\d+) 项测试 / (\d+) 项门禁」", (real_tests, real_gates)),
        ("收尾报告 验收命令注释", REPORT, r"# (\d+) 项门禁（含", real_gates),
    ):
        if subset and _label.startswith("收尾报告"):
            # 子集模式下"门禁数"这个事实压根没法得出（本次没跑全量），
            # 硬比只会得到一个假红 —— 明确跳过并说明，而不是让它去误报。
            print(f"③d {_label}：[SKIP] 子集模式（本次运行不含全量门禁数）")
            continue
        if not _path.exists():
            continue
        _txt = _path.read_text(encoding='utf-8')
        _hits = re.findall(_pat, _txt)
        _vals = []
        for _h in _hits:
            _vals.append(tuple(int(x.replace(",", "")) for x in _h) if isinstance(_h, tuple)
                         else int(_h.replace(",", "")))
        print(f"③d {_label}：{_vals} / 期望 {_expect}")
        if len(_vals) != 1 or _vals[0] != _expect:
            problems.append(f"{_label}：文档 {_vals}，期望 {_expect}")

    # ③e **发布事实**（附录 GQ）：ISO 体积与下载链接形态。
    # 权威来源 = site/release.json（gen-site.py 称"已发布产物的权威事实"）。
    _rel_path = ROOT / "site" / "release.json"
    if _rel_path.exists():
        _rel = json.loads(_rel_path.read_text(encoding="utf-8"))
        _size, _tag = _rel.get("iso_size"), _rel.get("tag")
        for _label, _path in (("README.md", README), ("README.en.md", README_EN),
                              ("site/index.html", SITE / "index.html"),
                              ("site/en/index.html", SITE / "en" / "index.html"),
                              ("site/download.html", SITE / "download.html"),
                              ("site/en/download.html", SITE / "en" / "download.html")):
            if not _path.exists():
                continue
            _txt = _path.read_text(encoding="utf-8")
            if _label.startswith("README"):
                _found = re.findall(r"ISO[^\n]{0,12}?(\d+\.\d+ MB)", _txt)
                # 打印的"期望"要与**实际判据**一致（README 里本来就有两处体积，
                # 早先这句打印写成一个，读日志的人会以为它错了 —— 实测就被误导过一次）
                print(f"③e {_label} ISO 体积：{_found} / 期望 ['{_size}', '{_size}']")
                if _found != [_size, _size]:
                    problems.append(f"{_label} ISO 体积：文档 {_found}，期望 ['{_size}', '{_size}']")
            # 链接形态：prerelease 下 /releases/latest 会 404
            if re.search(r"releases/latest", _txt):
                problems.append(f"{_label} 用了 /releases/latest（prerelease 会 404）")
            if f"releases/tag/{_tag}" not in _txt and f"releases/download/{_tag}/" not in _txt:
                problems.append(f"{_label} 没有指向 releases/{_tag} 的链接")

    # ③ 审计附录数
    real_apps = len(re.findall(r'^## 附录', AUDIT.read_text(encoding='utf-8'), re.M))
    doc_apps = declared(text, r'\| 审计附录 \| (\d+) 篇 \|')
    print(f"③ 审计附录：文档 {doc_apps} / 实测 {real_apps}")
    if doc_apps != real_apps:
        problems.append(f"审计附录：文档 {doc_apps}，实测 {real_apps}")

    # ③f **同一事实的其余写法**（2026-10-11 补，台账 #51）。
    # 加它的原因：③b/③c/③d 只认几种固定写法，于是同一份文件里的**第二种写法**
    # 可以一直漂 —— 实测就漂着 4 处（README 两处「750 项」、INDEX「45 个 .rs」、
    # 收尾报告的代码行数与测试数注释），而门禁一直是绿的。
    if README.exists():
        _rtext = README.read_text(encoding='utf-8')
        for _pat, _what in ((r"除 (\d+) 项单测外", "README「除 N 项单测外」"),
                            (r"Windows (\d+) 项", "README「Windows N 项」")):
            for _m2 in re.finditer(_pat, _rtext):
                _v = int(_m2.group(1))
                print(f"③f {_what}：{_v} / 实测 {real_tests}")
                if _v != real_tests:
                    problems.append(f"{_what}：文档 {_v}，实测 {real_tests}")
    if INDEX.exists():
        _itext = INDEX.read_text(encoding='utf-8')
        _hits = [int(h) for h in re.findall(r"(\d+) 个 \.rs", _itext)]
        print(f"③f INDEX「N 个 .rs」：{_hits} / 实测 {real_files}")
        for _v in _hits:
            if real_files is not None and _v != real_files:
                problems.append(f"INDEX「N 个 .rs」：文档 {_v}，实测 {real_files}")
    if REPORT.exists():
        _ft = REPORT.read_text(encoding='utf-8')
        for _pat, _what, _real in (
            (r"\| Rust 代码行数 \| \*\*([\d,]+)\*\*", "收尾报告 代码行数", real_lines),
            (r"#\s*(\d+) 项\s*$", "收尾报告 测试数注释", real_tests),
        ):
            _hits = [int(h.replace(",", "")) for h in re.findall(_pat, _ft, re.M)]
            print(f"③f {_what}：{_hits} / 实测 {_real}")
            for _v in _hits:
                if _real is not None and _v != _real:
                    problems.append(f"{_what}：文档 {_v}，实测 {_real}")

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
