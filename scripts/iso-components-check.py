"""ISO 组件门禁：**仓库里有的二进制 crate，必须进镜像**（2026-10-06 加，附录 GV）。

## 为什么

`aether-install` 曾经在仓库里、却**不在** `build-iso.sh` 的 `COMPONENTS` 里 ——
于是 README 说"重新构建后装机可用"，而**重建也不会带上安装器**。
**没有任何判据在看这件事**（`sh -n` 查不出来，它不是语法错误）。

## 判据

凡 `Cargo.toml` 的 `workspace.members` 里带 `src/main.rs` 的 crate，
都必须出现在 `build-iso.sh` 的 `COMPONENTS=(...)` 里。
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


# 例外：仓库里有 main.rs、但**刻意**不进镜像的 crate。
# 必须写明理由，而且会被**打印出来** —— 本会话第 4 条：
# "过滤器/白名单会隐藏 bug"，所以不写理由的例外等于隐藏 bug。
STUBS = {
    "aether-shell": "M0 骨架占位（src/main.rs 文档注释自述「当前为 M0 骨架占位，防止结构空转」）",
}


def main() -> int:
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"members\s*=\s*\[(.*?)\]", cargo, re.S)
    if not m:
        print("[FAIL] Cargo.toml 里找不到 workspace.members")
        return 1
    members = re.findall(r'"([^"]+)"', m.group(1))

    iso = (ROOT / "platform" / "build-iso.sh").read_text(encoding="utf-8")
    mi = re.search(r"COMPONENTS=\(([^)]*)\)", iso)
    if not mi:
        print("[FAIL] build-iso.sh 里找不到 COMPONENTS=(...)")
        return 1
    in_iso = set(mi.group(1).split())

    bins = []
    for name in members:
        if (ROOT / name / "src" / "main.rs").is_file():
            bins.append(name)

    for name, why in sorted(STUBS.items()):
        print(f"  例外：{name} —— {why}")
    bad = [b for b in bins if b not in in_iso and b not in STUBS]
    print(f"仓库里的二进制 crate（{len(bins)} 个）：{', '.join(sorted(bins))}")
    print(f"镜像 COMPONENTS（{len(in_iso)} 个）：{', '.join(sorted(in_iso))}")
    if bad:
        print(f"[FAIL] 这些二进制 crate 没进镜像：{', '.join(bad)}")
        print("       后果：仓库里有、ISO 里没有 —— 功能「看着做完了」却不可用。")
        return 1
    print("[ OK ] 所有二进制 crate 都在镜像里")
    return 0


if __name__ == "__main__":
    sys.exit(main())
