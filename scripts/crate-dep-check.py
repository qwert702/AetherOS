#!/usr/bin/env python3
"""依赖声明检查：源码里用到的外部 crate **必须**在该 crate 的 Cargo.toml 里声明。

## 为什么需要这条判据（2026-10-11 的真实事故，台账 #53）

`aetherd` 的 SNTP 对时（`aetherd/src/timesync.rs`）直接调
`libc::clock_gettime` / `clock_settime` / `adjtime`，而 `aetherd/Cargo.toml` 里
**根本没有 `libc`**。这条路径同时躲过了当时所有判据：

    · Windows 构建 —— 那段代码在 `#[cfg(unix)]` 里，本地**根本不参与编译**；
    · musl 检查（`scripts/musl-warn-check.py`）—— **特意跳过 aetherd**
      （ring 需要 musl-gcc 交叉编译），于是这条路径也没人走；
    · 单测 —— 都在 Windows 上跑。

结果：**ISO 从 2026-10-03（加 SNTP 那天）起就构建不出来**，直到 10-11
第一次真正重建镜像才炸出来（台账 #33「ISO 未重构建」的真正后果）。

本判据是**纯静态**的：不看平台、不管能不能编译 —— 用到就必须声明。

## 判据

对每个 crate，扫描 `src/**/*.rs` 里出现的 `<名字>::` 路径；若该名字在
`WATCH` 名单里，则它的 `Cargo.toml` 必须有同名依赖。以下写法都算"声明了"：

    [dependencies]
    libc = "0.2"
    [target.'cfg(unix)'.dependencies]
    libc = "0.2"
    [dependencies.libc]
    version = "0.2"
    [dev-dependencies]
    libc = "0.2"

为什么只盯一个**名单**，而不是"所有 `x::` 路径"：把 `std::`、`core::`、
`crate::`、同名本地模块都算成外部 crate 会产生大量假红 ——
**判据宁可窄而准**，名单里每加一个名字都要有"它真的漏过一次"的理由。

## 用法

    python scripts/crate-dep-check.py             # 报告（人看的）
    python scripts/crate-dep-check.py --check     # 判红即退出码 1（门禁用）
    python scripts/crate-dep-check.py --selftest  # 自检：含正向对照
"""

from __future__ import annotations

import os
import re
import sys
import tempfile
from pathlib import Path

#: 需要盯的外部 crate 名单。加名字的门槛：**它真的漏过一次**（见文件头）。
WATCH: tuple[str, ...] = ("libc",)

#: 覆盖率下限：少于这个数说明扫描器坏了，那"0 个问题"不是证据。
MIN_CRATES = 6

SECTION = re.compile(r"^\[(.+?)\]\s*$")
KV = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")


def blank_comments(text: str) -> str:
    """把 `//` 与 `/* */` 注释换成等长空格（保留行号与列位置）。

    必须做这一步：注释里经常出现 `libc::clock_gettime` 这样的**说明文字**
    （本文件作者自己就会这么写）。不剔掉就会把文档当成代码用。
    """
    out: list[str] = []
    i = 0
    n = len(text)
    while i < n:
        two = text[i:i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif two == "/*":
            j = text.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out.append("".join("\n" if c == "\n" else " " for c in text[i:j]))
            i = j
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def declared_deps(toml_text: str) -> set[str]:
    """Cargo.toml 里声明了哪些依赖名（含 target / dev / 子表写法）。"""
    names: set[str] = set()
    section = ""
    for raw in toml_text.splitlines():
        line = raw.split("#", 1)[0]
        m = SECTION.match(line.strip())
        if m:
            section = m.group(1).strip()
            # [dependencies.foo] / [target.'…'.dev-dependencies.foo]
            parts = [p.strip().strip("'\"") for p in re.split(r"\.(?![^']*')", section)]
            if parts and parts[-1] not in ("dependencies", "dev-dependencies",
                                           "build-dependencies", "target"):
                if any(p.endswith("dependencies") for p in parts[:-1]) or \
                        parts[0] in ("dependencies", "dev-dependencies", "build-dependencies"):
                    names.add(parts[-1])
            continue
        if section.endswith("dependencies") or section.split(".")[-1].endswith("dependencies"):
            kv = KV.match(line)
            if kv:
                names.add(kv.group(1))
    return names


def crates(root: Path) -> list[Path]:
    """工作区里所有 crate 目录（有 Cargo.toml 且有 src/）。"""
    out = []
    for d in sorted(root.iterdir()):
        if d.is_dir() and (d / "Cargo.toml").is_file() and (d / "src").is_dir():
            out.append(d)
    return out


def scan_crate(crate: Path) -> dict[str, list[tuple[str, int]]]:
    """扫源码，返回 {被用到的外部名字: [(文件, 行号), …]}（只留 WATCH 里的）。"""
    found: dict[str, list[tuple[str, int]]] = {}
    for rs in sorted((crate / "src").rglob("*.rs")):
        text = blank_comments(rs.read_text(encoding="utf-8", errors="replace"))
        for line_no, line in enumerate(text.splitlines(), 1):
            for name in WATCH:
                if re.search(rf"(?<![\w:]){re.escape(name)}\s*::", line):
                    found.setdefault(name, []).append(
                        (str(rs.relative_to(crate)), line_no))
    return found


def inspect(root: Path) -> tuple[list[str], int, int]:
    """返回 (问题列表, 扫了几个 crate, 命中了几处用法)。"""
    problems: list[str] = []
    n_crates = 0
    n_hits = 0
    for crate in crates(root):
        n_crates += 1
        used = scan_crate(crate)
        if not used:
            continue
        have = declared_deps((crate / "Cargo.toml").read_text(encoding="utf-8",
                                                              errors="replace"))
        for name, spots in sorted(used.items()):
            n_hits += len(spots)
            if name in have:
                continue
            first = spots[0]
            problems.append(
                f"{crate.name}：代码用了 `{name}::`（{len(spots)} 处，"
                f"第一处 {first[0]}:{first[1]}），但 {crate.name}/Cargo.toml "
                f"没有声明 `{name}` 依赖"
            )
    return problems, n_crates, n_hits


# ---------------------------------------------------------------- 自检

def _mk_crate(base: Path, name: str, cargo: str, src: str) -> None:
    d = base / name
    (d / "src").mkdir(parents=True)
    (d / "Cargo.toml").write_text(cargo, encoding="utf-8")
    (d / "src" / "a.rs").write_text(src, encoding="utf-8")


def selftest() -> int:
    """自检：判据必须**真的会响**（正向对照），也必须不误报。"""
    cases = [
        ("用了但没声明 → 判红",
         '[package]\nname = "x"\n[dependencies]\nanyhow = "1"\n',
         'fn f() { unsafe { libc::clock_gettime(0, 0 as *mut _); } }\n', True),
        ("用 target 表声明了 → 通过",
         "[package]\nname = 'x'\n[target.'cfg(unix)'.dependencies]\nlibc = '0.2'\n",
         'fn f() { let _ = libc::CLOCK_REALTIME; }\n', False),
        ("子表写法声明了 → 通过",
         '[package]\nname = "x"\n[dependencies.libc]\nversion = "0.2"\n',
         'fn f() { let _ = libc::CLOCK_REALTIME; }\n', False),
        ("只有注释提到 → 不误报",
         '[package]\nname = "x"\n[dependencies]\nanyhow = "1"\n',
         '// 这里本来要调 libc::clock_settime\n/// libc::adjtime 也是\nfn f() {}\n', False),
        ("块注释里的用法也不算",
         '[package]\nname = "x"\n[dependencies]\nanyhow = "1"\n',
         '/*\n libc::timespec\n*/\nfn f() {}\n', False),
        ("别的名字不在名单里 → 不误报",
         '[package]\nname = "x"\n[dependencies]\nanyhow = "1"\n',
         'fn f() { let _ = serde_json::json!({}); }\n', False),
    ]
    failed = 0
    for label, cargo, src, want_bad in cases:
        with tempfile.TemporaryDirectory() as td:
            base = Path(td)
            _mk_crate(base, "x", cargo, src)
            for extra in range(MIN_CRATES - 1):  # 凑够覆盖率下限
                _mk_crate(base, f"pad{extra}",
                          f'[package]\nname = "pad{extra}"\n', "fn f() {}\n")
            problems, _, _ = inspect(Path(td))
            got_bad = bool(problems)
            mark = "OK  " if got_bad == want_bad else "FAIL"
            if got_bad != want_bad:
                failed += 1
            print(f"  [{mark}] {label}：期望{'判红' if want_bad else '通过'}，"
                  f"实得{'判红' if got_bad else '通过'}"
                  + (f" ← {problems[0][:70]}" if problems else ""))
    if failed:
        print(f"[FAIL] 自检不通过：{failed} 个用例与期望不符")
        return 1
    print(f"[ OK ] 自检通过（{len(cases)} 个用例，含正向对照）")
    return 0


def main() -> int:
    root = Path(os.environ.get("AETHER_LOCAL_ROOT")
                or Path(__file__).resolve().parent.parent)
    if "--selftest" in sys.argv:
        return selftest()

    problems, n_crates, n_hits = inspect(root)
    names = "、".join(f"`{n}`" for n in WATCH)
    print(f"依赖声明检查：扫了 {n_crates} 个 crate，名单（{names}）命中 {n_hits} 处用法")
    if "--check" in sys.argv and n_crates < MIN_CRATES:
        print(f"[FAIL] 扫描覆盖面不足（{n_crates} < {MIN_CRATES}）—— "
              f"扫描器可能坏了，这个结果不是证据")
        return 1
    if problems:
        print(f"[FAIL] 有 {len(problems)} 个 crate 用了未声明的依赖：")
        for p in problems:
            print(f"  {p}")
        print("  提示：`#[cfg(unix)]` / `#[cfg(target_os = …)]` 里的代码"
              "在 Windows 上不参与编译，靠本地构建**发现不了**这类问题。")
        return 1
    print(f"[ OK ] 名单里 {len(WATCH)} 个依赖：用到的都声明了")
    return 0


if __name__ == "__main__":
    sys.exit(main())
