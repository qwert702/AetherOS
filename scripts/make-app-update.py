#!/usr/bin/env python3
"""生成**应用级**更新包（R8/M8 发布链路，宿主侧打包）。

## 与既有链路的关系

清单与哈希格式**直接复用** `scripts/mkupdate.py`（同一套 `update.json` /
`update.json.sig` / `path+sha256+bytes`），不另造格式：

    <out>/<version>/
      update.json                 {"version": "...", "files": [{path, sha256, bytes}]}
      update.json.sig             Ed25519 签名（64 字节；只有 --key 时才写）
      var/apps/<id>/...           包内相对路径 = 目标路径去掉开头的 '/'

应用级更新默认**只允许 `/var/apps/` 前缀**（应用自身文件）；系统组件
（`/usr/bin/*` 等）刻意不走文件级更新，那类更新走整机 ISO + `--keep-var` 路径。

## 签名原则（不伪造签名）

* 不带 `--key`：产出 **unsigned bundle**，**不生成 `.sig`**，也不写占位/假签名；
  脚本只打印后续待执行的签名命令；
* 带 `--key`：私钥必须已存在；私钥不存在 → 明确报错退出（退出码 2），
  绝不现造一把钥匙冒充生产签名；私钥路径若落在仓库内也拒绝；
* 私钥**不进仓库**：生成钥匙用 `scripts/update-keygen.py`（私钥默认写到
  `~/.aether/`，公钥入库 `aetherd/keys/update_pubkey.bin`）。

## 用法

    python scripts/make-app-update.py --version 0.2.0 --from stage/app-hello
    python scripts/make-app-update.py --version 0.2.0 --from stage/app-hello \
        --key ~/.aether/update-signing-key.pem
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# 复用同目录既有打包逻辑（单一格式来源，不复制清单/哈希代码）
sys.path.insert(0, str(Path(__file__).resolve().parent))
import mkupdate  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
APP_PREFIX = "/var/apps/"


class AppPackError(Exception):
    """打包/用法错误 —— 整包拒绝。"""


def validate_stage(stage: Path, allow_system_paths: bool) -> None:
    """打包前预检：白名单前缀 + 应用级默认只收 /var/apps/。"""
    if not stage.is_dir():
        raise AppPackError(f"--from 不是一个目录：{stage}")
    found = 0
    for p in sorted(stage.rglob("*")):
        if p.is_symlink() or p.is_dir():
            if p.is_symlink():
                raise AppPackError(f"{p.relative_to(stage)}：暂存树里有符号链接，拒绝")
            continue
        rel = p.relative_to(stage).as_posix()
        target = "/" + rel
        if not mkupdate.path_is_allowed(target):
            raise AppPackError(
                f"{rel}：目标路径 {target} 不在允许范围内"
                f"（允许前缀：{'、'.join(mkupdate.TARGET_PREFIXES)}）"
            )
        if not allow_system_paths and not target.startswith(APP_PREFIX):
            raise AppPackError(
                f"{rel}：应用级更新只允许 {APP_PREFIX} 前缀（实得 {target}）；"
                "系统组件请走整机 ISO 路径，或显式 --allow-system-paths"
            )
        found += 1
    if found == 0:
        raise AppPackError("暂存树里没有任何文件")


def check_private_key(key_path: Path) -> Path:
    resolved = key_path.expanduser().resolve()
    if not resolved.is_file():
        raise AppPackError(
            f"签名私钥不存在：{resolved}\n"
            "  生产签名必须先离线生成钥匙：\n"
            "    python scripts/update-keygen.py --private-key ~/.aether/update-signing-key.pem\n"
            "  私钥不进仓库；当前包保持 unsigned，不生成假签名。"
        )
    if REPO in resolved.parents or resolved == REPO:
        raise AppPackError(f"拒绝使用仓库内私钥：{resolved}（私钥必须放在仓库外）")
    return resolved


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="生成应用级更新包（复用 mkupdate 清单/哈希格式；不伪造签名）",
        epilog=(
            "不带 --key 产出 unsigned bundle（无 .sig）；--key 指定的私钥不存在时退出码 2。"
        ),
    )
    ap.add_argument("--version", required=True, help="版本号，形如 0.2.0")
    ap.add_argument("--from", dest="stage", required=True, help="暂存树（目录结构 = 目标路径）")
    ap.add_argument("--out", default="updates-app", help="输出目录（默认 updates-app/）")
    ap.add_argument("--key", help="Ed25519 私钥 PEM（不给则产出 unsigned bundle）")
    ap.add_argument("--allow-system-paths", action="store_true",
                    help="允许 /etc/aether/、/var/lib/aether/ 等其它白名单前缀（默认只收 /var/apps/）")
    args = ap.parse_args(argv)

    stage = Path(args.stage)
    out = Path(args.out)

    try:
        validate_stage(stage, args.allow_system_paths)
        signing_key = check_private_key(Path(args.key)) if args.key else None

        pkg = mkupdate.pack(
            args.version, stage, out,
            signing_key=signing_key,
        )
    except mkupdate.PackError as e:
        print(f"make-app-update: 打包失败（整包拒绝）：{e}", file=sys.stderr)
        return 2
    except AppPackError as e:
        print(f"make-app-update: {e}", file=sys.stderr)
        return 2

    # 独立复检（不信任打包过程）
    if signing_key is not None:
        pub = mkupdate.public_key_of(mkupdate._load_private_key(signing_key))
        problems = mkupdate.verify_package(pkg, require_signature=True, public_key=pub)
    else:
        problems = mkupdate.verify_package(pkg)

    if problems:
        print("make-app-update: 复检不通过：", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 3

    manifest = json.loads((pkg / mkupdate.MANIFEST_NAME).read_text(encoding="utf-8"))
    count = len(manifest["files"])
    signed = signing_key is not None
    print(f"make-app-update: 已生成 {pkg}（{count} 个文件，{'已签名' if signed else '未签名（unsigned bundle）'}，复检通过）")

    if not signed:
        print("\n本包未签名，不能直接发布（客户端默认拒绝无签名清单）。待签名命令：")
        print(f"  python scripts/make-app-update.py --version {args.version} "
              f"--from {args.stage} --out {args.out} --key ~/.aether/update-signing-key.pem")
        print("\n签名后发布（先文件后清单，见 publish-updates.py）：")
        print(f"  python scripts/publish-updates.py {pkg} --dry-run")
        print(f"  python scripts/publish-updates.py {pkg} --apply")
    else:
        print("\n发布：")
        print(f"  python scripts/publish-updates.py {pkg} --dry-run")
        print(f"  python scripts/publish-updates.py {pkg} --apply")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
