#!/usr/bin/env python3
"""部署前检查：确认 VPS 访问方式满足 deploy-site.py 的安全前提。

只做本地检查，不连接服务器、不读取/打印私钥内容。检查项：
  * AETHER_VPS_HOST 是否设置
  * AETHER_VPS_USER 是否设置，且不是 root
  * AETHER_VPS_KEY 是否指向存在的私钥文件
  * POSIX 下私钥权限是否过宽（group/other 可读）
  * known_hosts 文件是否存在（若显式配置）

用法：
  python scripts/deploy-preflight.py
  python scripts/deploy-preflight.py --json
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser(description="AetherOS 部署访问前置检查")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    checks: list[dict] = []

    def add(name: str, ok: bool, detail: str = "") -> None:
        checks.append({"check": name, "ok": ok, "detail": detail})

    host = os.environ.get("AETHER_VPS_HOST", "").strip()
    user = os.environ.get("AETHER_VPS_USER", "").strip()
    key = os.environ.get("AETHER_VPS_KEY", "").strip()
    known = os.environ.get("AETHER_VPS_KNOWN_HOSTS", "").strip()

    add("AETHER_VPS_HOST", bool(host), host or "未设置")
    add("AETHER_VPS_USER", bool(user), user or "未设置")
    add("非 root 账号", bool(user) and user != "root",
        "当前是 root；deploy-site.py 默认拒绝 root 直连" if user == "root" else "ok")
    if key:
        p = Path(key).expanduser()
        add("私钥文件存在", p.is_file(), str(p))
        if p.is_file() and os.name != "nt":
            mode = stat.S_IMODE(p.stat().st_mode)
            add("私钥权限", (mode & 0o077) == 0, f"mode={oct(mode)}（建议 600）")
    else:
        add("AETHER_VPS_KEY", False, "未设置；推荐密钥认证，不建议用口令")
    if known:
        add("known_hosts 文件存在", Path(known).expanduser().is_file(), known)

    failed = [c for c in checks if not c["ok"]]
    if args.json:
        print(json.dumps({"checks": checks, "failed": len(failed)}, ensure_ascii=False, indent=2))
    else:
        for c in checks:
            print(f"[{'PASS' if c['ok'] else 'FAIL'}] {c['check']}"
                  + (f"  ({c['detail']})" if c["detail"] else ""))
        print(f"\n{len(checks) - len(failed)}/{len(checks)} 项通过；失败 {len(failed)} 项")
        if failed:
            print("\n下一步：按 docs/报告/发布部署清单（DEPLOY-ACCESS）-2026-10-31.md 配置非 root 账号与密钥。")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
