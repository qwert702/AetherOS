#!/usr/bin/env python3
"""gates.py —— 一条命令跑完所有门禁（本地与 CI 共用同一份清单）。

## 为什么需要它（2026-10-02 审计"D.2 第 18 条：仓库无 CI"）

这些检查覆盖的东西**互不重叠**，此前全靠人记得跑：

| 检查 | 覆盖什么 | 不覆盖什么 |
|---|---|---|
| `cargo test` | 逻辑与回归 | Windows 上会跳过所有 `#[cfg(unix)]` / `cfg(linux)` 用例 |
| `cargo check --target ...-musl` | Linux 目标**能否编译** | 跑不了（交叉编译产物在本机执行不了） |
| `repo-stats.py --check` | 文档数字与代码是否同步（三层，含集合相等） | 站点与发布事实 |
| `gen-site.py --check` | 站点页面 / 发布事实 / 下载脚本哈希是否需要 `--refresh` | 代码本身 |
| `mkapp.py --selftest` | 打包器的纯函数边界（路径穿越等） | 真实打包流程 |
| `gen-site.py --selftest` | 链接白名单等纯函数边界 | 真实渲染（由 --check 覆盖） |

**Linux 专属断言只在 Linux 上执行** —— 这正是要把它放进 CI 的原因：
开发者机器是 Windows，那些用例从不运行，却要为此背书。

## 用法

    python scripts/gates.py              # 全部（默认 cargo 离线，适配本机缓存）
    python scripts/gates.py --online     # CI 用：允许 cargo 联网取依赖
    python scripts/gates.py --fast       # 只跑秒级的文档/站点/纯函数门禁

退出码：全部通过 0，任一失败 1（并打印失败清单）。
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PY = sys.executable


def run(cmd: list[str], timeout: int = 1800) -> tuple[int, str]:
    t0 = time.time()
    try:
        r = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True,
                           encoding="utf-8", errors="replace", timeout=timeout)
        out = (r.stdout or "") + (r.stderr or "")
        return r.returncode, f"{time.time() - t0:.1f}s · {out.strip()[-400:]}"
    except subprocess.TimeoutExpired:
        return 124, f"超时（{timeout}s）"
    except FileNotFoundError as exc:
        return 127, f"命令不存在：{exc}"


def main() -> int:
    ap = argparse.ArgumentParser(description="跑完所有门禁")
    ap.add_argument("--online", action="store_true", help="允许 cargo 联网（CI 用）")
    ap.add_argument("--fast", action="store_true", help="只跑秒级检查")
    args = ap.parse_args()

    offline = [] if args.online else ["--offline"]

    steps: list[tuple[str, list[str]]] = []
    if not args.fast:
        steps.append(("cargo build（含测试目标）",
                      ["cargo", "build", "--workspace", "--all-targets", *offline]))
        steps.append(("cargo test（Windows 上会跳过 unix 专属用例）",
                      ["cargo", "test", "--workspace", *offline]))
        # musl 目标不一定装了：装了才查（aetherd 因 ring 需要 musl-gcc，永远查不了）
        installed = subprocess.run(["rustup", "target", "list", "--installed"],
                                   capture_output=True, text=True).stdout
        if "x86_64-unknown-linux-musl" in installed:
            steps.append((
                "cargo check --target musl（6/7 crate）",
                ["cargo", "check", *offline, "--target", "x86_64-unknown-linux-musl",
                 "--all-targets", "-p", "aether-compositor", "-p", "aether-init",
                 "-p", "aether-ops", "-p", "aether-install", "-p", "aether-ipc",
                 "-p", "aether-shell"],
            ))
        else:
            print("跳过 musl 检查：未安装 x86_64-unknown-linux-musl（rustup target add 后可查）")

    steps.append(("文档规模门禁（三层 + 集合相等）",
                  [PY, "scripts/repo-stats.py", "--check"]))
    steps.append(("站点门禁（页面/发布事实/脚本哈希）",
                  [PY, "scripts/gen-site.py", "--check"]))
    steps.append(("mkapp 自检（打包器边界）", [PY, "scripts/mkapp.py", "--selftest"]))
    steps.append(("站点生成器自检（链接白名单）", [PY, "scripts/gen-site.py", "--selftest"]))
    steps.append(("Python 语法检查", [PY, "-m", "py_compile", *[
        str(p.relative_to(ROOT)) for p in sorted((ROOT / "scripts").glob("*.py"))
    ]]))

    failed: list[str] = []
    for name, cmd in steps:
        code, detail = run(cmd)
        mark = "OK  " if code == 0 else "FAIL"
        print(f"[{mark}] {name}  ({detail.splitlines()[0] if detail else ''})")
        if code != 0:
            failed.append(name)
            for line in detail.splitlines()[-12:]:
                print(f"        {line}")

    print()
    if failed:
        print(f"门禁未通过：{len(failed)}/{len(steps)} 项失败 → {failed}")
        return 1
    print(f"全部 {len(steps)} 项门禁通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
