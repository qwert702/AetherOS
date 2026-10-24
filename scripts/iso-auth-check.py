#!/usr/bin/env python3
"""认证基线自检：构建脚本必须拒绝"既没口令、也没显式 demo"的构建。

背景（2026-10-24）：`platform/build-iso.sh` 此前默认产出 root 无口令镜像；
对一个 Live 演示系统这是刻意取舍，但对任何非演示构建都是危险默认值。
现在改为：

    AETHER_ROOT_PW_HASH=...  -> locked（root 需要口令）
    AETHER_DEMO=1            -> demo（root 无口令，显式选择）
    两者都没有                -> 构建失败（安全默认）

本脚本只调用 `build-iso.sh --check-auth`，不会编译或下载任何东西。
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "platform" / "build-iso.sh"


def _bash() -> str:
    for cand in ("/bin/bash", "/usr/bin/bash", shutil.which("bash")):
        if cand and Path(cand).is_file():
            return cand
    raise SystemExit("找不到 bash：认证基线自检需要 bash（build-iso.sh 是 bash 脚本）")


def _env(**extra: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("AETHER_")}
    env.update(extra)
    env.setdefault("HOME", os.environ.get("HOME", os.environ.get("USERPROFILE", "")))
    return env


def run_case(name: str, extra: dict[str, str], expect_code: int, expect_text: str) -> bool:
    proc = subprocess.run(
        [_bash(), str(SCRIPT), "--check-auth"],
        cwd=ROOT,
        env=_env(**extra),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=30,
    )
    out = (proc.stdout or "") + (proc.stderr or "")
    ok = proc.returncode == expect_code and expect_text in out
    print(("  [OK  ] " if ok else "  [FAIL] ") + f"{name}（exit={proc.returncode}）")
    if not ok:
        print(f"        期望 exit={expect_code} 且包含 {expect_text!r}")
        print(f"        实得：{out.strip()[:300]}")
    return ok


def selftest() -> int:
    cases = [
        ("无变量 ⇒ 拒绝构建", {}, 1, "认证基线未选择"),
        ("AETHER_DEMO=1 ⇒ demo", {"AETHER_DEMO": "1"}, 0, "AUTH_MODE=demo"),
        ("AETHER_ROOT_PW_HASH ⇒ locked", {"AETHER_ROOT_PW_HASH": "$6$test$hash"}, 0, "AUTH_MODE=locked"),
    ]
    # 未知参数那条不能用 --check-auth（否则会被当成合法参数）；单独跑。
    ok = True
    for name, extra, code, text in cases:
        ok = run_case(name, extra, code, text) and ok
    proc = subprocess.run(
        [_bash(), str(SCRIPT), "--not-a-real-flag"],
        cwd=ROOT,
        env=_env(),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=30,
    )
    out = (proc.stdout or "") + (proc.stderr or "")
    case_ok = proc.returncode == 2 and "未知参数" in out
    print(("  [OK  ] " if case_ok else "  [FAIL] ") + f"未知参数 ⇒ 拒绝（exit={proc.returncode}）")
    ok = ok and case_ok
    print("\n自检结果：" + ("全部通过" if ok else "有失败项"))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="认证基线自检（build-iso.sh --check-auth）")
    ap.add_argument("--selftest", action="store_true", help="跑四种模式的自检")
    args = ap.parse_args(argv)
    if not args.selftest:
        ap.error("目前只支持 --selftest")
    return selftest()


if __name__ == "__main__":
    raise SystemExit(main())
