#!/usr/bin/env python3
"""AetherOS 代码规模口径 —— 单一事实来源。

用法:
    python scripts/repo-stats.py                # 逐 crate 行数/文件数 + 合计
    python scripts/repo-stats.py --per-file     # 额外打印逐文件行数（更新 INDEX 用）
    python scripts/repo-stats.py --check        # 门禁：与 README/INDEX/roadmap 声明的
                                                # 合计、逐 crate、逐文件三层比对

为什么要这个脚本：规模数字原本手写在 README.md / INDEX.md 里，每次提交都会漂
—— 2026-09-29 复核时 README 的 `aetherd` 行数与实测差了 1,130 行，合计差了 1,393 行。
口径统一在这里，文档只引用不手抄。

**为什么门禁要分三层**（2026-10-02 安全审计）：此前 `--check` 只比对**合计**行，
于是"README 写 compositor 15,890 / INDEX 写 15,926 / 实测 16,021"这种三方不一致
照样全绿 —— 门禁给了**虚假保证**。现在合计、逐 crate、逐文件任一不一致都非零退出。

口径:
  * 范围 = Cargo.toml 的 `workspace.members`（不在脚本里另列一遍，避免两处维护）
  * 对象 = 成员目录下的 `*.rs`，递归、排除 `target/`
  * 行数 = 换行符个数（等价 `wc -l`）

退出码: 0 = 一致；1 = 门禁不通过；2 = 用法或环境错误
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Pattern, Sequence

REPO_ROOT = Path(__file__).resolve().parent.parent
CARGO_TOML = REPO_ROOT / "Cargo.toml"
INDEX_MD = REPO_ROOT / "INDEX.md"
README_MD = REPO_ROOT / "README.md"
ROADMAP_MD = REPO_ROOT / "docs" / "roadmap.md"

EXIT_OK = 0
EXIT_GATE_FAILED = 1
EXIT_USAGE = 2

# INDEX.md 的合计行： | **合计** | **20,949** | **41** | |
DECL_INDEX: Pattern[str] = re.compile(
    r"\|\s*\*\*合计\*\*\s*\|\s*\*\*([\d,]+)\*\*\s*\|\s*\*\*([\d,]+)\*\*"
)
# README.md 的合计行： 合计 20,949 行 Rust / 41 个源文件 / 7 个 crate（…）
DECL_README: Pattern[str] = re.compile(
    r"合计\s*([\d,]+)\s*行 Rust\s*/\s*([\d,]+)\s*个源文件"
)
# docs/roadmap.md 的规模行： 代码规模（2026-09-29 实测）：**20,949 行 / 41 个 .rs**（7 crate）
DECL_ROADMAP: Pattern[str] = re.compile(
    r"代码规模（[^）]*）：\*\*([\d,]+) 行 / ([\d,]+) 个 \.rs\*\*"
)
# INDEX.md 的 crate 行： | `aether-compositor` | 16,149 | 22 | 职责… |
DECL_CRATE_INDEX: Pattern[str] = re.compile(
    r"^\|\s*`(aether[\w-]+)`\s*\|\s*([\d,]+)\s*\|\s*([\d,]+)\s*\|", re.M
)
# README.md 的组件表行： | `aether-compositor/` | 16,149 | 说明 | M1–M2 |
DECL_CRATE_README: Pattern[str] = re.compile(
    r"^\|\s*`(aether[\w-]+)/`\s*\|\s*([\d,]+)\s*\|", re.M
)
# INDEX.md 的 crate 小节标题： ### `aetherd`（AI 中枢，5,912 行 / 11 文件）
SECTION_INDEX: Pattern[str] = re.compile(r"^###\s+`?(aether[\w-]*)`?")
# INDEX.md 的逐文件行： | `src/main.rs` (3,962) | 说明 |
# 末尾的分组用来兼容 `(170, Linux)` 这种"行数 + 平台注解"的写法。
DECL_FILE_INDEX: Pattern[str] = re.compile(r"^\|\s*`([^`]+\.rs)`\s*\((\d[\d,]*)([^)]*)\)\s*\|")
# 逐文件门禁最多打印多少条不一致（避免刷屏；总数仍会报出来）
MAX_REPORTED_FILE_MISMATCHES = 20


@dataclass(frozen=True)
class FileStat:
    """单个源文件的行数。path 为相对仓库根目录的路径。"""

    path: Path
    lines: int


@dataclass(frozen=True)
class CrateStat:
    """单个 workspace 成员（crate）的规模。"""

    name: str
    files: tuple[FileStat, ...]

    @property
    def lines(self) -> int:
        return sum(f.lines for f in self.files)


def _fail(message: str) -> SystemExit:
    """统一的错误出口：打到 stderr 并以 EXIT_USAGE(2) 结束（不吞异常、不静默回退）。"""
    print(f"[repo-stats] {message}", file=sys.stderr)
    return SystemExit(EXIT_USAGE)


def workspace_members() -> list[str]:
    """从 Cargo.toml 读 workspace.members。Python < 3.11 时退回正则解析。"""
    if not CARGO_TOML.is_file():
        raise _fail(f"找不到 {CARGO_TOML}")
    text = CARGO_TOML.read_text(encoding="utf-8")
    try:
        import tomllib  # Python 3.11+
    except ImportError:
        tomllib = None  # type: ignore[assignment]
    if tomllib is not None:
        try:
            data = tomllib.loads(text)
        except tomllib.TOMLDecodeError as exc:
            raise _fail(f"Cargo.toml 解析失败: {exc}")
        members = data.get("workspace", {}).get("members")
        if isinstance(members, list) and members:
            return [str(m) for m in members]
    block = re.search(r"members\s*=\s*\[(.*?)\]", text, re.S)
    if block is None:
        raise _fail("Cargo.toml 里没解析到 workspace.members")
    found = re.findall(r'"([^"]+)"', block.group(1))
    if not found:
        raise _fail("workspace.members 是空的")
    return found


def count_lines(path: Path) -> int:
    """按换行符个数计数（等价 wc -l）。读失败直接报错，不静默跳过。"""
    try:
        data = path.read_bytes()
    except OSError as exc:
        raise _fail(f"读取失败 {path}: {exc}")
    return data.count(b"\n")


def collect() -> list[CrateStat]:
    """遍历所有成员，返回逐 crate 的规模。"""
    stats: list[CrateStat] = []
    for name in workspace_members():
        crate_dir = REPO_ROOT / name
        if not crate_dir.is_dir():
            raise _fail(f"workspace 成员目录不存在: {crate_dir}")
        files = sorted(p for p in crate_dir.rglob("*.rs") if "target" not in p.parts)
        stats.append(
            CrateStat(
                name=name,
                files=tuple(
                    FileStat(path=f.relative_to(REPO_ROOT), lines=count_lines(f))
                    for f in sorted(files)
                ),
            )
        )
    return stats


def totals(stats: Sequence[CrateStat]) -> tuple[int, int]:
    """返回 (总行数, 总文件数)。"""
    return sum(s.lines for s in stats), sum(len(s.files) for s in stats)


def print_table(stats: Sequence[CrateStat], per_file: bool) -> None:
    """打印 Markdown 表（可直接粘进 INDEX.md）。"""
    total_lines, total_files = totals(stats)
    print("| crate | 行数 | 文件 |")
    print("|---|---|---|")
    for s in sorted(stats, key=lambda s: -s.lines):
        print(f"| `{s.name}` | {s.lines:,} | {len(s.files)} |")
    print(f"| **合计** | **{total_lines:,}** | **{total_files}** |")
    if per_file:
        for s in sorted(stats, key=lambda s: -s.lines):
            print(f"\n### {s.name}（{s.lines:,} 行 / {len(s.files)} 文件）")
            for f in sorted(s.files, key=lambda f: -f.lines):
                print(f"| `{f.path.as_posix()}` | {f.lines:,} |")


def declared(path: Path, pattern: Pattern[str]) -> tuple[int, int] | None:
    """从文档里读出声明的 (行数, 文件数)；读不到返回 None。"""
    if not path.is_file():
        raise _fail(f"找不到 {path}")
    text = path.read_text(encoding="utf-8")
    match = pattern.search(text)
    if match is None:
        return None
    return int(match.group(1).replace(",", "")), int(match.group(2).replace(",", ""))


def gate_crates(stats: Sequence[CrateStat]) -> bool:
    """门禁第二层：README / INDEX 的**逐 crate** 行数（INDEX 还含文件数）必须与实测一致。

    两处范围限制，都是踩过的坑：
    * README 的组件表只列行数（没有文件数列），所以只比行数；
    * INDEX 顶部的汇总表在第一个 `## ` 小节之前 —— 后面的「测试分布」表同样是
      "三列 + 第一列是 crate 名"，靠列数区分不开，只能靠范围排除。
    """
    actual = {s.name: (s.lines, len(s.files)) for s in stats}
    ok = True

    # ---- INDEX：行数 + 文件数 ----
    head = INDEX_MD.read_text(encoding="utf-8").split("\n## ", 1)[0]
    index_rows = DECL_CRATE_INDEX.findall(head)
    if not index_rows:
        print("[FAIL] INDEX.md: 没找到逐 crate 汇总表（表格式变了？）")
        ok = False
    else:
        bad = 0
        for name, lines_s, files_s in index_rows:
            want = actual.get(name)
            if want is None:
                print(f"[FAIL] INDEX.md: 汇总表里有未知 crate `{name}`")
                bad += 1
                continue
            got = (int(lines_s.replace(",", "")), int(files_s.replace(",", "")))
            if got != want:
                print(
                    f"[FAIL] INDEX.md: `{name}` 声明 {got[0]:,} 行 / {got[1]} 文件，"
                    f"实测 {want[0]:,} 行 / {want[1]} 文件"
                )
                bad += 1
        # **完整性断言**（代码审查发现）：只逐行比对的话，把某个 crate 整行删掉，
        # 剩下的行依然全部一致 → 门禁报 OK。这是"门禁给虚假保证"的典型形态，
        # 所以这里要求两侧的 crate 集合完全相等。
        listed = {r[0] for r in index_rows}
        missing = sorted(set(actual) - listed)
        if missing:
            print(f"[FAIL] INDEX.md 汇总表漏了 {len(missing)} 个 crate：{missing}"
                  "（删掉一行不算通过）")
            bad += len(missing)
        if bad == 0:
            print(f"[ OK ] INDEX.md 逐 crate：{len(index_rows)} 个 crate 全部一致（集合相等）")
        else:
            ok = False

    # ---- README：只有行数 ----
    readme_rows = DECL_CRATE_README.findall(README_MD.read_text(encoding="utf-8"))
    if not readme_rows:
        print("[FAIL] README.md: 没找到组件表（表格式变了？）")
        ok = False
    else:
        bad = 0
        for name, lines_s in readme_rows:
            want = actual.get(name)
            if want is None:
                print(f"[FAIL] README.md: 组件表里有未知 crate `{name}`")
                bad += 1
                continue
            got = int(lines_s.replace(",", ""))
            if got != want[0]:
                print(f"[FAIL] README.md: `{name}` 声明 {got:,} 行，实测 {want[0]:,} 行")
                bad += 1
        listed = {r[0] for r in readme_rows}
        missing = sorted(set(actual) - listed)
        if missing:
            print(f"[FAIL] README.md 组件表漏了 {len(missing)} 个 crate：{missing}")
            bad += len(missing)
        if bad == 0:
            print(f"[ OK ] README.md 逐 crate：{len(readme_rows)} 个 crate 行数一致（集合相等）")
        else:
            ok = False
    return ok


def gate_files(stats: Sequence[CrateStat]) -> bool:
    """门禁第三层：INDEX 的**逐文件**行数必须与实测一致。

    这一层是 2026-10-02 审计补的：此前 42 条逐文件声明里有 11 条是错的
    （4 处 `src/main.rs` 被同一个数字覆盖），而门禁只盖合计，全绿。
    """
    actual = {f.path.as_posix(): f.lines for s in stats for f in s.files}
    section: str | None = None
    checked = 0
    mismatches: list[str] = []
    seen: set[str] = set()
    for line in INDEX_MD.read_text(encoding="utf-8").splitlines():
        head = SECTION_INDEX.match(line)
        if head:
            section = head.group(1)
            continue
        row = DECL_FILE_INDEX.match(line)
        if not row or section is None:
            continue
        full = f"{section}/{row.group(1)}"
        seen.add(full)
        want = actual.get(full)
        if want is None:
            mismatches.append(f"{full}: 表里有，但仓库里没有这个文件")
            continue
        checked += 1
        declared = int(row.group(2).replace(",", ""))
        if declared != want:
            mismatches.append(f"{full}: 声明 {declared:,} / 实测 {want:,}")
    # **双向断言**（代码审查发现）：原来的实现只做"表 → 仓库"单向核对，
    # 于是 (a) 删掉表里某一行、(b) 仓库新增文件但表里没登记、(c) 格式漂移让
    # 正则匹配不上（该行静默脱管）这三种情况全都报 OK。这里要求两边集合相等。
    unchecked = sorted(set(actual) - seen)
    if unchecked:
        print(f"[FAIL] INDEX.md 逐文件表漏了 {len(unchecked)} 个源文件（新增文件必须登记，"
              "格式也必须让正则认得出）：")
        for m in unchecked[:MAX_REPORTED_FILE_MISMATCHES]:
            print(f"        {m}")
        return False
    if mismatches:
        print(f"[FAIL] INDEX.md 逐文件行数：{len(mismatches)} 条不一致（已核对 {checked} 条）")
        for m in mismatches[:MAX_REPORTED_FILE_MISMATCHES]:
            print(f"        {m}")
        if len(mismatches) > MAX_REPORTED_FILE_MISMATCHES:
            print(f"        …另有 {len(mismatches) - MAX_REPORTED_FILE_MISMATCHES} 条未列出")
        return False
    print(f"[ OK ] INDEX.md 逐文件行数：{checked} 条全部一致")
    return True


def count_test_attrs(crate: str) -> int:
    """数一个 crate 源码里 `#[test]` 属性的个数（含 Linux 专属用例）。"""
    root = REPO_ROOT / crate
    n = 0
    for path in root.rglob("*.rs"):
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        n += sum(1 for line in text.splitlines() if line.strip() == "#[test]")
    return n


def gate_test_table(stats: Sequence[CrateStat]) -> bool:
    """门禁：INDEX.md 的测试分布表必须与源码实测一致。

    为什么值得一条门禁（代码审查发现）：那张表曾经**自相矛盾** ——
    各行属性相加 415、合计行却写 403、`aether-ops` 写 15 而实测 14，
    而它没有任何自动检查（`gen-site.py` 重测行数但不重测测试数）。

    这里核对两件可算的事：① 每行属性数 == 源码里 `#[test]` 的个数；
    ② 合计行的两列 == 各自求和。Windows 列是实跑结果，无法从源码推出，
    因此**只核对合计**，并在表下方注明这一点。
    """
    text = INDEX_MD.read_text(encoding="utf-8")
    rows: dict[str, tuple[int, int]] = {}
    total: tuple[int, int] | None = None
    for line in text.splitlines():
        m = re.match(
            r"^\|\s*`?([a-z\-]+)`?\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|", line
        )
        if not m:
            continue
        name, win, attrs = m.group(1), int(m.group(2)), int(m.group(3))
        if name == "合计" or name.endswith("合计"):
            continue
        rows[name] = (win, attrs)
        if name == "**合计**":
            total = (win, attrs)
    # 合计行是 `| **合计** | 399 | 418 |`：上面的正则抓不到 `**`，单独找
    m = re.search(r"^\|\s*\*\*合计\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|", text, re.M)
    if m:
        total = (int(m.group(1)), int(m.group(2)))

    if not rows:
        print("[FAIL] INDEX.md 测试分布表：没解析到任何行（格式变了？）")
        return False

    ok = True
    for crate, (_, attrs) in sorted(rows.items()):
        actual = count_test_attrs(crate)
        if attrs != actual:
            print(f"[FAIL] INDEX.md 测试表：`{crate}` 声明 {attrs} 项 `#[test]`，实测 {actual}")
            ok = False
    if total is None:
        print("[FAIL] INDEX.md 测试分布表：没找到合计行")
        ok = False
    else:
        sum_win = sum(v[0] for v in rows.values())
        sum_attrs = sum(v[1] for v in rows.values())
        if total != (sum_win, sum_attrs):
            print(
                f"[FAIL] INDEX.md 测试表合计：声明 {total}，各行相加为 "
                f"({sum_win}, {sum_attrs})"
            )
            ok = False
    if ok:
        print(f"[ OK ] INDEX.md 测试分布表：{len(rows)} 个 crate 的属性数与合计一致")
    return ok


def gate(stats: Sequence[CrateStat]) -> int:
    """门禁：文档声明必须与实测一致（合计 + 逐 crate + 逐文件 + 测试表）。"""
    total_lines, total_files = totals(stats)
    ok = True
    for label, path, pattern in (
        ("INDEX.md", INDEX_MD, DECL_INDEX),
        ("README.md", README_MD, DECL_README),
        ("docs/roadmap.md", ROADMAP_MD, DECL_ROADMAP),
    ):
        claimed = declared(path, pattern)
        if claimed is None:
            print(f"[FAIL] {label}: 没找到合计行（格式变了？）")
            ok = False
        elif claimed == (total_lines, total_files):
            print(f"[ OK ] {label}: {total_lines:,} 行 / {total_files} 文件")
        else:
            print(
                f"[FAIL] {label}: 声明 {claimed[0]:,} 行 / {claimed[1]} 文件，"
                f"实测 {total_lines:,} 行 / {total_files} 文件"
            )
            ok = False
    # 逐 crate 与逐文件：合计对了不代表下面也对（这就是"虚假保证"的来源）
    ok = gate_crates(stats) and ok
    ok = gate_files(stats) and ok
    ok = gate_test_table(stats) and ok
    return EXIT_OK if ok else EXIT_GATE_FAILED


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description="AetherOS 代码规模口径（单一事实来源）")
    parser.add_argument(
        "--check",
        action="store_true",
        help="与 README.md / INDEX.md 声明的合计比对，不一致则非零退出",
    )
    parser.add_argument("--per-file", action="store_true", help="额外打印逐文件行数")
    args = parser.parse_args(list(argv))

    stats = collect()
    if args.check:
        return gate(stats)
    print_table(stats, per_file=args.per_file)
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
