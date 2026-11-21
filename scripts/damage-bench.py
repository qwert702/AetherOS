#!/usr/bin/env python3
"""G3 脏区上屏基准包装器（E1/G3 工作包）。

调用 `aether-compositor --damage-bench [N]`，在 1080p / 4K × 100% / 150% 四格上
对比**全量上屏**与**局部脏区上屏**的软件缓冲拷贝成本。

注意：本基准只测内存帧缓冲之间的拷贝（`present_full` vs `present_rects`），
**不含**真实 fbdev `/dev/fb0` 的写回带宽；真机上屏成本需另测。

用法：
    python scripts/damage-bench.py [迭代次数]   # 默认 200
"""

import subprocess
import sys
from pathlib import Path


def main() -> int:
    iterations = sys.argv[1] if len(sys.argv) > 1 else "200"
    repo_root = Path(__file__).resolve().parent.parent
    cmd = [
        "cargo",
        "run",
        "-p",
        "aether-compositor",
        "--locked",
        "--",
        "--damage-bench",
        iterations,
    ]
    return subprocess.call(cmd, cwd=repo_root)


if __name__ == "__main__":
    sys.exit(main())
