#!/usr/bin/env python3
"""AetherOS 代码规模口径 —— 单一事实来源。

用法:
    python scripts/repo-stats.py                # 逐 crate 行数/文件数 + 合计
    python scripts/repo-stats.py --per-file     # 额外打印逐文件行数（更新 INDEX 用）
    python scripts/repo-stats.py --check        # 门禁：与 README/INDEX/roadmap 声明的合计比对

为什么要这个脚本：规模数字原本手写在 README.md / INDEX.md 里，每次提交都会漂
—— 2026-09-29 复核时 README 的 `aetherd` 行数与实测差了 1,130 行，合计差了 1,393 行。
口径统一在这里，文档只引用不手抄。

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


def gate(stats: Sequence[CrateStat]) -> int:
    """门禁：文档声明必须与实测一致。"""
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
