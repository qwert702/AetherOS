"""musl 目标的**告警**门禁（2026-10-06 加，附录 GR）。

## 为什么单独做

`gates.py` 原来那一步只跑 `cargo check --target musl`，**只看退出码** ——
而 cargo **有告警也返回 0**。于是它验证的是"能编译"，而 README 声称的是
"musl 侧 6/7 crate **0 警告**"。

本会话就是栽在这上面：新加的报表方法在 musl 下成了 dead_code（3 条告警），
门禁**一直是绿的** —— 典型的"从不触发的判据等于没有判据"。

## 判据

逐个 crate 跑 `cargo check --target x86_64-unknown-linux-musl`，
**统计 `warning:` 行**；有任何一条就非零退出。
排除 `ring@` 构建脚本噪声（缺交叉编译器是**已知**限制，且 `aetherd` 不在清单里）。

用法：`python scripts/musl-warn-check.py`（退出码 0 = 无告警）
"""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 与 gates.py 的 musl 步骤保持同一份清单（aetherd 因 ring 需要 musl-gcc，查不了）
CRATES = [
    "aether-compositor",
    "aether-init",
    "aether-ops",
    "aether-install",
    "aether-ipc",
    "aether-shell",
]


def main() -> int:
    bad = 0
    for crate in CRATES:
        proc = subprocess.run(
            ["cargo", "check", "--offline", "--locked",
             "--target", "x86_64-unknown-linux-musl", "--all-targets", "-p", crate],
            cwd=ROOT, capture_output=True, text=True,
        )
        out = proc.stdout + proc.stderr
        warns = [
            line for line in out.splitlines()
            if line.startswith("warning")
            and "ring@" not in line
            and "generated" not in line
        ]
        if proc.returncode != 0:
            print(f"[FAIL] {crate}: cargo check 失败（退出码 {proc.returncode}）")
            bad += 1
        elif warns:
            print(f"[FAIL] {crate}: {len(warns)} 条告警")
            for line in warns[:4]:
                print(f"        {line[:100]}")
            bad += 1
        else:
            print(f"[ OK ] {crate}: 0 告警")
    if bad:
        print(f"\nmusl 告警门禁未通过：{bad}/{len(CRATES)} 个 crate 有问题")
        return 1
    print(f"\nmusl 告警门禁通过：{len(CRATES)} 个 crate 全部 0 告警")
    return 0


if __name__ == "__main__":
    sys.exit(main())
