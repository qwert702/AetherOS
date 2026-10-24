#!/usr/bin/env python3
"""把更新包发布到 VPS（P1 发布侧，2026-10-16）。

## 与 guest 侧的契约（必须严格对齐，否则"更新不发"）

guest 侧 `aetherd/src/updater.rs` 做的是：

    GET  <base>/update.json            ← 清单
    GET  <base>/<path 去掉开头的 />    ← 每个文件（例如 <base>/var/apps/hello/hello）

**所以包被"摊平"放在 <base>/ 下**，不是按版本号分子目录。本地 `mkupdate.py` 产出的
`updates/<version>/`（清单 + 文件树）在发布时要映射成：

    updates/<version>/update.json          →  <base>/update.json
    updates/<version>/var/apps/hello/hello →  <base>/var/apps/hello/hello

## 可逆优先

默认**干跑**（只打印要传什么）。真的上传要显式 `--apply`。上传前逐条校验：
路径必须是白名单前缀内的相对路径、不许有 `..`、不许有控制字符（与 Rust 侧同一条判据）。

## 用法

    python scripts/publish-updates.py updates/0.1.2 --dry-run     # 看要传什么（默认）
    python scripts/publish-updates.py updates/0.1.2 --apply       # 真上传
    python scripts/publish-updates.py --selftest                  # 纯逻辑自检
"""
from __future__ import annotations

import argparse
import json
import os
import posixpath
import re
import sys
from pathlib import Path

# 与 aetherd::update::TARGET_PREFIXES / updater 同一条判据（改一处必须改三处，故在此断言自检）
TARGET_PREFIXES = ("/var/apps/", "/etc/aether/", "/var/lib/aether/")
MANIFEST_NAME = "update.json"
MANIFEST_SIG_NAME = "update.json.sig"
DEFAULT_ROOT = "/var/www/aether.cbnac.com"
REMOTE_SUBDIR = "updates"


def rel_is_allowed(rel: str) -> bool:
    """相对路径是否允许（镜像 Rust 侧 path_is_allowed 的四条判据）。"""
    if not rel or rel.startswith("/") or ".." in rel.split("/"):
        return False
    if any(ord(c) < 32 or ord(c) == 127 for c in rel):
        return False
    if "//" in rel:
        return False
    return ("/" + rel).startswith(TARGET_PREFIXES)


def plan_upload(package: Path) -> list[tuple[Path, str]]:
    """列出 (本地文件, 远端相对路径)。清单本身映射成 `update.json`，其余摊平。

    返回的第一个元素永远是清单（先传它、最后再"生效"由调用方决定顺序 ——
    这里只做映射，顺序由 `publish()` 按"先文件后清单"排）。
    """
    manifest = package / MANIFEST_NAME
    if not manifest.is_file():
        raise SystemExit(f"包里没有 {MANIFEST_NAME}：{manifest}")
    signature = package / MANIFEST_SIG_NAME
    if not signature.is_file():
        raise SystemExit(
            f"包里没有 {MANIFEST_SIG_NAME}：{signature}（生产发布必须签名；"
            "用 mkupdate.py --signing-key 生成）"
        )
    if signature.stat().st_size != 64:
        raise SystemExit(
            f"签名文件长度不对：{signature.stat().st_size} 字节（期望 64）"
        )
    m = json.loads(manifest.read_text(encoding="utf-8"))
    version = m.get("version")
    if not isinstance(version, str) or not re.fullmatch(r"\d+(\.\d+)*", version):
        raise SystemExit(f"清单里的 version 不合法：{version!r}")
    files = m.get("files")
    if not isinstance(files, list) or not files:
        raise SystemExit("清单里没有任何文件")

    plan: list[tuple[Path, str]] = [
        (manifest, MANIFEST_NAME),
        (signature, MANIFEST_SIG_NAME),
    ]
    for f in files:
        p = f.get("path")
        if not isinstance(p, str):
            raise SystemExit(f"清单条目缺 path：{f!r}")
        rel = p.lstrip("/")
        if not rel_is_allowed(rel):
            raise SystemExit(f"目标路径不在允许范围内：{p!r}（允许前缀：{'、'.join(TARGET_PREFIXES)}）")
        local = package / rel
        if not local.is_file():
            raise SystemExit(f"包内缺少文件：{local}")
        plan.append((local, rel))
    # 同一远端路径出现两次 = 自相矛盾
    seen: set[str] = set()
    for _, rel in plan:
        if rel in seen:
            raise SystemExit(f"远端口径重复：{rel}")
        seen.add(rel)
    return plan


def publish(package: Path, root: str, apply: bool) -> int:
    plan = plan_upload(package)
    base = posixpath.join(root.rstrip("/"), REMOTE_SUBDIR)
    print(f"源包：{package}")
    print(f"远端：{base}/")
    # 顺序很重要：**先传文件，最后传清单** —— 清单先到会让 guest 立刻来拉还没传完的文件
    ordered = sorted(plan, key=lambda x: (x[1] == MANIFEST_NAME, x[1]))
    for local, rel in ordered:
        size = local.stat().st_size
        print(f"  {'[传]' if apply else '[干跑]'} {rel}（{size} 字节）")
    if not apply:
        print("\n默认干跑，未做任何上传。确认无误后加 --apply。")
        return 0

    host = os.environ.get("AETHER_VPS_HOST")
    user = os.environ.get("AETHER_VPS_USER", "aether-deploy")
    key = os.environ.get("AETHER_VPS_KEY")
    if not host or not key:
        raise SystemExit("需要 AETHER_VPS_HOST 与 AETHER_VPS_KEY（见 scripts/deploy-site.py 的口径）")
    try:
        import paramiko  # 延迟导入：干跑与自检都不需要它
    except ImportError:
        raise SystemExit("需要 paramiko（用 D:\\py\\python.exe 跑；本机 Python 运行时没有它）")

    c = paramiko.SSHClient()
    c.load_system_host_keys()
    c.set_missing_host_key_policy(paramiko.RejectPolicy())
    c.connect(hostname=host, username=user, key_filename=key, timeout=20)
    try:
        def sh(cmd: str) -> str:
            _, out, err = c.exec_command(cmd)
            return (out.read() + err.read()).decode("utf-8", "replace").strip()

        print("  远端准备目录…", sh(f"mkdir -p {base} && echo ok"))
        sftp = c.open_sftp()
        try:
            for local, rel in ordered:
                remote = posixpath.join(base, rel)
                cur = base
                for part in rel.split("/")[:-1]:
                    cur = posixpath.join(cur, part)
                    try:
                        sftp.stat(cur)
                    except IOError:
                        sftp.mkdir(cur)
                sftp.put(str(local), remote)
                print(f"  [完成] {rel}")
        finally:
            sftp.close()
        print("  远端权限…", sh(f"chown -R nginx:nginx {base} 2>/dev/null || chown -R www-data:www-data {base} 2>/dev/null; echo done"))
        print("  验证清单可取：", sh(f"head -c 120 {base}/{MANIFEST_NAME}"))
    finally:
        c.close()
    print("\n发布完成。guest 侧 aetherd 会在下个检查周期拉到它（下次启动生效）。")
    return 0


def selftest() -> int:
    import tempfile

    fails: list[str] = []

    def check(cond: bool, what: str) -> None:
        print(("  [OK  ] " if cond else "  [FAIL] ") + what)
        if not cond:
            fails.append(what)

    check(rel_is_allowed("var/apps/hello/hello"), "白名单内相对路径接受")
    check(not rel_is_allowed("../etc/aether/x"), "含 .. 拒绝")
    check(not rel_is_allowed("/var/apps/x"), "绝对路径拒绝（应为相对）")
    check(not rel_is_allowed("var/apps/a\nb"), "控制字符拒绝")
    check(not rel_is_allowed("usr/bin/aetherd"), "系统二进制拒绝（不在白名单）")
    check(not rel_is_allowed(""), "空路径拒绝")

    with tempfile.TemporaryDirectory() as td:
        pkg = Path(td) / "0.1.2"
        (pkg / "var/apps/hello").mkdir(parents=True)
        (pkg / "var/apps/hello/hello").write_text("#!/bin/sh\necho hi\n", encoding="utf-8")
        import hashlib
        h = hashlib.sha256((pkg / "var/apps/hello/hello").read_bytes()).hexdigest()
        (pkg / MANIFEST_NAME).write_text(json.dumps({
            "version": "0.1.2",
            "files": [{"path": "/var/apps/hello/hello", "sha256": h, "bytes": 18}],
        }), encoding="utf-8")
        # 发布契约要求签名文件存在；自检只验布局，不验密码学（Rust 单测验）
        (pkg / MANIFEST_SIG_NAME).write_bytes(b"\x00" * 64)
        plan = dict((rel, local.name) for local, rel in plan_upload(pkg))
        check(plan.get(MANIFEST_NAME) == MANIFEST_NAME, "清单映射到 update.json（摊平，不带版本目录）")
        check(plan.get(MANIFEST_SIG_NAME) == MANIFEST_SIG_NAME,
              "签名映射到 update.json.sig（与清单同目录）")
        check(plan.get("var/apps/hello/hello") == "hello",
              "包内文件映射到 var/apps/hello/hello（**摊平**：guest 就是按这个 URL 拉的）")
        # 负向：清单里塞系统二进制 ⇒ 整包拒绝
        (pkg / MANIFEST_NAME).write_text(json.dumps({
            "version": "0.1.3",
            "files": [{"path": "/usr/bin/aetherd", "sha256": h, "bytes": 18}],
        }), encoding="utf-8")
        try:
            plan_upload(pkg)
            check(False, "负向：系统二进制应当被拒绝，却没有")
        except SystemExit:
            check(True, "负向：清单里的系统二进制被拒绝")
        # 负向：版本号非法
        (pkg / MANIFEST_NAME).write_text(json.dumps({"version": "v1", "files": []}), encoding="utf-8")
        try:
            plan_upload(pkg)
            check(False, "负向：非法版本号应当被拒绝，却没有")
        except SystemExit:
            check(True, "负向：非法版本号被拒绝")
        # 负向：缺签名 ⇒ 发布必须拒绝
        (pkg / MANIFEST_NAME).write_text(json.dumps({
            "version": "0.1.3",
            "files": [{"path": "/var/apps/hello/hello", "sha256": h, "bytes": 18}],
        }), encoding="utf-8")
        (pkg / MANIFEST_SIG_NAME).unlink()
        try:
            plan_upload(pkg)
            check(False, "负向：缺签名应当被拒绝，却没有")
        except SystemExit:
            check(True, "负向：缺签名被拒绝")

    print("自检" + ("全部通过" if not fails else f"失败（{len(fails)} 项）"))
    return 0 if not fails else 1


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="把更新包发布到 VPS")
    ap.add_argument("package", nargs="?", help="本地包目录（如 updates/0.1.2）")
    ap.add_argument("--root", default=DEFAULT_ROOT, help=f"站点根目录（默认 {DEFAULT_ROOT}）")
    # 默认就是干跑（可逆优先）；`--dry-run` 接受为**显式别名**，免得文档里写了却报
    # "unrecognized arguments"（2026-10-16 实测踩到：我自己写的用法示例跑不过）。
    ap.add_argument("--dry-run", action="store_true", help="只打印要传什么（默认行为，可显式写）")
    ap.add_argument("--apply", action="store_true", help="真的上传（与 --dry-run 互斥）")
    ap.add_argument("--selftest", action="store_true", help="纯逻辑自检")
    args = ap.parse_args(argv)
    if args.dry_run and args.apply:
        ap.error("--dry-run 与 --apply 互斥（默认就是干跑，要上传才加 --apply）")
    if args.selftest:
        return selftest()
    if not args.package:
        ap.error("需要包目录（或用 --selftest）")
    return publish(Path(args.package), args.root, args.apply)


if __name__ == "__main__":
    raise SystemExit(main())
