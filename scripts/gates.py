#!/usr/bin/env python3
"""gates.py —— 一条命令跑完所有门禁（本地与 CI 共用同一份清单）。

## 为什么需要它（2026-10-02 审计"D.2 第 18 条：仓库无 CI"）

这些检查覆盖的东西**互不重叠**，此前全靠人记得跑：

| 检查 | 覆盖什么 | 不覆盖什么 |
|---|---|---|
| `cargo test` | 逻辑与回归 | Windows 上会跳过所有 `#[cfg(unix)]` / `cfg(linux)` 用例 |
| `scripts/musl-warn-check.py` | Linux 目标**能否编译 + 6 个 crate 是否 0 告警**（原来只查能否编译 —— 而 cargo 有告警也返回 0，附录 GR） | 跑不了（交叉编译产物在本机执行不了） |
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

# ⚠️ **不要**把子进程的 TEMP/TMPDIR 改到仓库内（曾经试过，已回退）。
#
# 动机是合理的：本机 C 盘一度可用空间为 0，`mkapp.py --selftest` 之类会**间歇性**
# 失败，`perm::tests::audit_rotates_when_oversized` 更是直接 `StorageFull`。
# 但把临时目录指到 `<repo>/target/gates-tmp` 会**破坏测试语义**：
# `aetherd` 的写工具白名单只允许"用户数据区（家目录、/tmp）"，而测试用
# `std::env::temp_dir()` 造路径 —— 一旦它落到仓库里，删除/重命名类测试会正确地
# 拒绝写入而失败（`tools.rs::delete_then_restore_*`、`rename_refuses_to_overwrite_existing`）。
# 也就是说：临时目录的位置**是测试前提的一部分**，不能随手改。
#
# 正确做法：磁盘空间问题按环境问题处理（清理磁盘 / 换机器），
# 测试自身的空间占用问题在测试里解决（例：审计轮转测试用 1KB 上限，见 perm.rs）。


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


def _posix_shell() -> str | None:
    """找一个能用的 POSIX shell 做语法检查。

    Windows 上没有 bash，但 w64devkit 自带一个 POSIX shell；Linux/macOS 直接用 /bin/sh。
    找不到就返回 None（调用方打印"跳过"，**不假装检查过**）。
    注意：它不支持 bash 数组等 bashism，所以只用来检查纯 POSIX 的脚本。
    """
    import shutil
    for cand in ("/bin/sh", "/usr/bin/sh",
                 r"C:\Tools\w64devkit\w64devkit\bin\bash.exe",
                 r"C:\Program Files\Git\bin\bash.exe"):
        if Path(cand).exists():
            return cand
    return shutil.which("sh")


def main() -> int:
    ap = argparse.ArgumentParser(description="跑完所有门禁")
    ap.add_argument("--online", action="store_true", help="允许 cargo 联网（CI 用）")
    ap.add_argument("--fast", action="store_true", help="只跑秒级检查")
    args = ap.parse_args()

    offline = [] if args.online else ["--offline"]

    steps: list[tuple[str, list[str]]] = []
    if not args.fast:
        # --locked：与 platform/build-iso.sh 保持一致（对抗审查发现 CI 没带）——
        # Cargo.lock 一旦漂移，不加 --locked 的 CI 会静默接受并构建出不同的依赖树，
        # 而 M-9 的核心主张正是"同一提交必然得到同一产物"。
        steps.append(("cargo build（含测试目标，--locked）",
                      ["cargo", "build", "--workspace", "--all-targets", "--locked", *offline]))
        steps.append(("cargo test（Windows 上会跳过 unix 专属用例）",
                      ["cargo", "test", "--workspace", "--locked", *offline]))
        # 自动 e2e（FEATURE-GAPS §4.1）：`--e2e` 是**无界面**动作运行器，
        # 因此这一条在 Windows 上也能真跑（不像 GUI 走查图那样依赖图形环境）。
        # 放在 cargo build 之后：它需要刚构建出来的合成器二进制。
        steps.append(("自动 e2e（无界面动作运行器）",
                      [PY, "scripts/e2e-ui-actions.py"]))
        # 像素回归：`--shot` 逐字节与基线比对（确定模式，见审计附录 CF / CH）。
        # 渲染不出来时脚本自己 [SKIP] 并返回 0（例如目标平台没有字体）——
        # 会打印原因，不是静默跳过。
        steps.append(("像素回归（--shot 逐字节 vs 基线）",
                      [PY, "scripts/shot-diff.py", "verify"]))
        # 几何断言：19 页 × 3 档，查**横向越界**与控件重叠（后者只报告）。
        # 纵向越界不判红 —— 设置正文可滚动，内容比可视区高是正常的。
        steps.append(("布局几何（越界 / 重叠）",
                      [PY, "scripts/layout-check.py", "--strict"]))
        # i18n 完整性：`pick`/`tr` 的**英文位**不得含中文（漏译信号）。
        # 脚本**断言扫描覆盖面**（实测 270 个调用，阈值 200）—— 解析器一坏就报错，
        # 而不是静默报 0（附录 FD 记了这条：那个 0 不是证据）。
        steps.append(("i18n 完整性（英文位不得含中文）",
                      [PY, "scripts/i18n-check.py"]))
        # 分析函数自检：证明判据**真的会响**（合成 BMP 喂给 ⑧ / overlap / content_colors）。
        # 一个从不触发的判据等于没有判据 —— 同时**如实断言** ⑧ 的已知盲点。
        steps.append(("布局分析函数自检（判据真的会响）",
                      [PY, "scripts/layout-check-selftest.py"]))
        # 缩放扫描器的正向对照：往临时文件注入违规 → 必须判红并点名 → 清理后放行。
        # （顺带记录：①②③④ 默认只扫 draw.rs —— 用 --file 才覆盖别的文件，见附录 FF。）
        steps.append(("缩放扫描器正向对照（判据真的会响）",
                      [PY, "scripts/scale-audit-selftest.py"]))
        # 静态扫描「尺寸缩放而步长写死」—— 本会话修掉的 6 处都是这一类，
        # 而它们只在 125%/150% 下才暴露（100% 恰好不重叠）。这一步是瞬时的。
        steps.append(("缩放硬编码扫描（步长 vs 缩放高度）",
                      [PY, "scripts/scale-audit.py", "--strict"]))
        # 性能门禁（FEATURE-GAPS §4.2）：跑 `--bench` 看 P99 是否在一帧预算内。
        # 需要刚构建出来的二进制，所以排在 cargo build 之后、与 e2e 同批。
        steps.append(("性能门禁（P99 帧时间 ≤ 一帧预算）",
                      [PY, "scripts/bench-gate.py"]))
        # musl 目标不一定装了：装了才查（aetherd 因 ring 需要 musl-gcc，永远查不了）
        installed = subprocess.run(["rustup", "target", "list", "--installed"],
                                   capture_output=True, text=True).stdout
        if "x86_64-unknown-linux-musl" in installed:
            # 这一步**同时查"能编译"与"0 告警"**（附录 GR）：
            # 原来只跑 cargo check，而 cargo **有告警也返回 0** ——
            # 于是 README 声称的"musl 侧 6/7 crate 0 警告"没有任何东西在看。
            steps.append((
                "musl 目标（6/7 crate：能编译 + 0 告警）",
                [PY, "scripts/musl-warn-check.py"],
            ))
        else:
            print("⚠ 跳过 musl 检查：未安装 x86_64-unknown-linux-musl（rustup target add 后可查）"
                  " —— 这一步**没有**被执行，别当成已通过")

    steps.append(("文档规模门禁（三层 + 集合相等）",
                  [PY, "scripts/repo-stats.py", "--check"]))
    steps.append(("站点门禁（页面/发布事实/脚本哈希）",
                  [PY, "scripts/gen-site.py", "--check"]))
    # 链接门禁（2026-10-03 文档整理时加）：搬目录会让相对链接**静默失效**，
    # 而原先没有任何检查在看这件事 —— 整理那次就是靠它抓出 5 条断链。
    steps.append(("文档链接门禁（本地链接必须能解析）",
                  [PY, "scripts/check-links.py"]))
    steps.append(("mkapp 自检（打包器边界）", [PY, "scripts/mkapp.py", "--selftest"]))
    steps.append(("站点生成器自检（链接白名单）", [PY, "scripts/gen-site.py", "--selftest"]))
    # 构建链脚本也要有判据（对抗审查发现：E–H 批次对交付链的修复没有任何自动检查）。
    # 本机没有真 bash，只有 POSIX shell —— 用它能做的：语法检查（对不含 bashism 的脚本有效）。
    sh = _posix_shell()
    if sh:
        for script in ("platform/br2-external/board/aether/post-build.sh",
                       "platform/overlay/init"):
            steps.append((f"shell 语法：{script}", [sh, "-n", script]))
    else:
        print("⚠ 本机没有可用的 POSIX shell，跳过构建脚本语法检查（Linux CI 会跑）")
    # e2e 的**判定逻辑**自检：不需要合成器、不需要真机，所以能进日常门禁。
    # 真实 e2e 在完整模式跑（见上）；这一步保证"脚本本身没写错"。
    steps.append(("自动 e2e 判定逻辑自检", [PY, "scripts/e2e-ui-actions.py", "--selftest"]))
    # 性能门禁的判定逻辑同样自检：不需要合成器、不需要真机（阈值与解析都要能验）
    steps.append(("性能门禁判定逻辑自检", [PY, "scripts/bench-gate.py", "--selftest"]))
    # 文档里的数字必须与来源一致（手抄必然失实：2026-10-04 那个提交就是现场）。
    # 它读 `docs/证据/gate-count.txt` —— 那份文件在本函数**跑之前**写入（见下），
    # 所以同一次运行内自洽：改了门禁步骤数 → 这一步当场红，逼着改文档。
    steps.append(("文档数字一致性（测试 / 门禁 / 附录）",
                  [PY, "scripts/doc-numbers-check.py"]))
    steps.append(("Python 语法检查", [PY, "-m", "py_compile", *[
        str(p.relative_to(ROOT)) for p in sorted((ROOT / "scripts").glob("*.py"))
    ]]))

    # 把本次运行的步骤数落盘（**跑之前**写，见上一步的说明）
    (ROOT / "docs" / "证据" / "gate-count.txt").write_text(f"{len(steps)}\n", encoding="utf-8")

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
