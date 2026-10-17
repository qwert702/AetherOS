#!/usr/bin/env python3
"""更新链路**契约测试**：发布布局 ↔ guest 拉取 URL 必须严丝合缝（2026-10-17）。

## 为什么需要它

guest 侧（`aetherd/src/updater.rs`）拉的是：

    GET  <base>/update.json
    GET  <base>/<path 去掉开头的 />

发布侧（`scripts/publish-updates.py`）把包**摊平**到 `<base>/`。这个"摊平"约定是
我从代码里读出来、又自己实现的 —— **最可能搞错、且错了就"更新永远不发"** 的一环。
本脚本不猜：真起一个 HTTP 服务，按发布布局摆文件，然后**用 guest 的 URL 规则去拉**，
逐字节核对 sha256。不需要虚拟机、不需要 VPS。

## 诚实边界

- 只验 **URL 布局与清单内容**；不验 Rust 侧的下载/暂存逻辑（那由 `updater.rs` 的单测覆盖）；
- 用明文 http 起本地服务：guest 侧配置**强制 https**（安全选择），本地循环回环只用于验契约，
  不代表配置政策变了。

## 用法

    python scripts/update-contract-check.py            # 全套契约测试（自带临时 HTTP 服务）
    python scripts/update-contract-check.py --selftest # 只验纯逻辑（不起服务）
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import socketserver
import sys
import tempfile
import threading
import urllib.request
from pathlib import Path

# 与 aetherd::update::TARGET_PREFIXES 同一条判据
TARGET_PREFIXES = ("/var/apps/", "/etc/aether/", "/var/lib/aether/")
MANIFEST_NAME = "update.json"


def host_file(rel: str) -> bool:
    return ("/" + rel).startswith(TARGET_PREFIXES) and ".." not in rel.split("/")


def published_layout(package: Path, site: Path) -> list[tuple[str, Path]]:
    """把 `<package>/<version>/` 摊平成发布布局，返回 [(远端相对路径, 本地文件)]。

    映射规则（与 publish-updates.py 一致，两处必须同时改）：
        <package>/update.json            → <site>/update.json
        <package>/var/apps/hello/hello   → <site>/var/apps/hello/hello
    顺带校验清单里的每个 path 都在白名单内、且包内文件存在。
    """
    manifest = package / MANIFEST_NAME
    m = json.loads(manifest.read_text(encoding="utf-8"))
    plan: list[tuple[str, Path]] = [(MANIFEST_NAME, manifest)]
    for f in m["files"]:
        rel = f["path"].lstrip("/")
        if not host_file(rel):
            raise SystemExit(f"清单里 {f['path']!r} 不在白名单内")
        src = package / rel
        if not src.is_file():
            raise SystemExit(f"包内缺文件：{src}")
        plan.append((rel, src))
    for rel, src in plan:
        dst = site / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes(src.read_bytes())
    return plan


def make_package(root: Path) -> Path:
    """造一个最小合法包（与 mkupdate.py 的产物同构）。"""
    pkg = root / "0.1.2"
    (pkg / "var/apps/hello").mkdir(parents=True)
    body = b"#!/bin/sh\necho hello from update\n"
    (pkg / "var/apps/hello/hello").write_bytes(body)
    (pkg / MANIFEST_NAME).write_text(json.dumps({
        "version": "0.1.2",
        "files": [{"path": "/var/apps/hello/hello",
                   "sha256": hashlib.sha256(body).hexdigest(),
                   "bytes": len(body)}],
    }, ensure_ascii=False), encoding="utf-8")
    return pkg


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):  # 测试输出要干净
        pass


def contract_check() -> int:
    fails: list[str] = []

    def check(cond: bool, what: str) -> None:
        print(("  [OK  ] " if cond else "  [FAIL] ") + what)
        if not cond:
            fails.append(what)

    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        site = root / "site"
        pkg = make_package(root / "updates")
        plan = published_layout(pkg, site)
        print(f"== 发布布局（{len(plan)} 个文件）==")
        for rel, _ in plan:
            print(f"  site/{rel}")

        handler = lambda *a, **kw: Quiet(*a, directory=str(site), **kw)  # noqa: E731
        with socketserver.TCPServer(("127.0.0.1", 0), handler) as httpd:
            port = httpd.server_address[1]
            th = threading.Thread(target=httpd.serve_forever, daemon=True)
            th.start()
            try:
                base = f"http://127.0.0.1:{port}"
                print(f"== 按 guest 的 URL 规则拉（base={base}）==")
                # ① 清单必须能取到、且是合法 JSON
                raw = urllib.request.urlopen(f"{base}/{MANIFEST_NAME}", timeout=10).read()
                m = json.loads(raw.decode("utf-8"))
                check(m["version"] == "0.1.2", "拉到的清单版本正确（0.1.2）")
                # ② 每个文件按 <base>/<rel> 拉，逐字节对哈希
                all_ok = True
                for f in m["files"]:
                    rel = f["path"].lstrip("/")
                    got = urllib.request.urlopen(f"{base}/{rel}", timeout=10).read()
                    h = hashlib.sha256(got).hexdigest()
                    ok = h == f["sha256"] and len(got) == f["bytes"]
                    all_ok = all_ok and ok
                    print(f"  {'[OK  ]' if ok else '[FAIL]'} {rel}（{len(got)} 字节，sha256 {'一致' if ok else '不符'}）")
                check(all_ok, "所有文件按 guest 的 URL 规则都能取到且哈希一致")
                # ③ 负向对照：少传一个文件 ⇒ guest 会 404（说明布局缺一个都会断）
                missing = "var/apps/hello/nope"
                try:
                    urllib.request.urlopen(f"{base}/{missing}", timeout=5)
                    check(False, "负向：不存在的文件应当 404，却没报错")
                except urllib.error.HTTPError as e:
                    check(e.code == 404, f"负向：不存在的文件返回 404（实得 {e.code}）")
            finally:
                httpd.shutdown()

    print("契约测试" + ("通过 ✓" if not fails else f"失败 ✗（{len(fails)} 项）"))
    return 0 if not fails else 1


def selftest() -> int:
    fails: list[str] = []

    def check(cond: bool, what: str) -> None:
        print(("  [OK  ] " if cond else "  [FAIL] ") + what)
        if not cond:
            fails.append(what)

    check(host_file("var/apps/hello/hello"), "白名单内路径接受")
    check(not host_file("usr/bin/aetherd"), "系统二进制拒绝")
    check(not host_file("../etc/aether/x"), "含 .. 拒绝")
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        pkg = make_package(root / "updates")
        # 负向：清单里塞系统二进制 ⇒ 摊平时必须整包拒绝
        m = json.loads((pkg / MANIFEST_NAME).read_text(encoding="utf-8"))
        m["files"][0]["path"] = "/usr/bin/aetherd"
        (pkg / MANIFEST_NAME).write_text(json.dumps(m), encoding="utf-8")
        try:
            published_layout(pkg, root / "site")
            check(False, "负向：系统二进制应当被拒绝，却没有")
        except SystemExit:
            check(True, "负向：摊平阶段拒绝系统二进制")
    print("自检" + ("全部通过" if not fails else f"失败（{len(fails)} 项）"))
    return 0 if not fails else 1


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="更新链路契约测试（发布布局 ↔ guest URL）")
    ap.add_argument("--selftest", action="store_true", help="只验纯逻辑（不起 HTTP 服务）")
    args = ap.parse_args(argv)
    return selftest() if args.selftest else contract_check()


if __name__ == "__main__":
    raise SystemExit(main())
