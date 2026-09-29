#!/usr/bin/env python3
"""serve-apps.py —— 在宿主上起一个只读 HTTP 服务，把打包好的应用喂给 guest。

## 为什么需要它

guest 里没有包管理器，也没有共享目录；但 **QEMU 用户态网络**已经给了 guest→宿主的通道：
guest 通过 DHCP 拿到 `10.0.2.15`，网关正是宿主（`10.0.2.2`）。2026-09-29 用长跑日志确认过
这条链路是通的（`udhcpc: lease of 10.0.2.15 obtained from 10.0.2.2`），而 guest 里
BusyBox 自带 `wget`。所以"装软件"的最后一段就是：**宿主起个只读服务，guest 拉一个包**。

## 用法

    # 宿主侧（把 dist/apps 里的 .aep 服务出去）
    python3 scripts/serve-apps.py --dir dist/apps

    # guest 侧（在 AetherOS 的终端里）
    wget http://10.0.2.2:8765/htop.aep -O /var/tmp/htop.aep
    mkdir -p /var/tmp/pkg && tar xf /var/tmp/htop.aep -C /var/tmp/pkg
    aetherd app install /var/tmp/pkg/htop

## 安全边界（刻意做窄）

- **只读**：只实现 GET/HEAD，没有上传、没有删除；
- **只在指定目录里找文件**，路径做规范化，`..` 与绝对路径一律拒；
- **默认只绑 127.0.0.1**（QEMU 的 SLIRP 把宿主呈现为 `10.0.2.2`，映射到宿主回环），
  想从别的机器访问得显式 `--host 0.0.0.0`，那时自己承担后果；
- 不用 `SimpleHTTPRequestHandler` 的目录列举（少一个信息泄露面），改为显式列出可用包。
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import socket
import socketserver
import sys
from pathlib import Path

#: 默认端口。选这个没有特别含义，只要和 guest 里敲的一致即可。
DEFAULT_PORT = 8765


def sha256_of(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def build_handler(root: Path, verbose: bool):
    """把 server 限制在 `root` 里的一个极简处理器。"""

    class Handler(http.server.BaseHTTPRequestHandler):
        server_version = "AetherAppServer/1.0"

        def _resolve(self) -> Path | None:
            """把请求路径解析成 root 下的真实文件；越界一律 None。"""
            raw = self.path.split("?", 1)[0].lstrip("/")
            if not raw or raw.endswith("/"):
                return None
            if ".." in Path(raw).parts or raw.startswith("/"):
                return None
            cand = (root / raw).resolve()
            try:
                cand.relative_to(root.resolve())  # 必须仍在 root 里（防符号链接跑出去）
            except ValueError:
                return None
            return cand if cand.is_file() else None

        def _send_file(self, p: Path, body: bool) -> None:
            data = p.read_bytes()
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("X-SHA256", sha256_of(p))
            self.end_headers()
            if body:
                self.wfile.write(data)

        def do_GET(self):  # noqa: N802
            p = self._resolve()
            if p is None:
                self.send_error(404, "no such package")
                return
            if verbose:
                print(f"  -> {self.client_address[0]} 拉走了 {p.name}（{p.stat().st_size} 字节）")
            self._send_file(p, body=True)

        def do_HEAD(self):  # noqa: N802
            p = self._resolve()
            if p is None:
                self.send_error(404, "no such package")
                return
            self._send_file(p, body=False)

        def log_message(self, fmt, *args):  # 默认会往 stderr 刷一行行噪音
            if verbose:
                super().log_message(fmt, *args)

    return Handler


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="把打包好的 AetherOS 应用用 HTTP 喂给 guest（只读）。")
    ap.add_argument("--dir", default="dist/apps", help="要服务的目录（默认 dist/apps）")
    ap.add_argument("--host", default="127.0.0.1",
                    help="监听地址（默认只绑回环；QEMU 的 SLIRP 会用 10.0.2.2 映射过来）")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"端口（默认 {DEFAULT_PORT}）")
    ap.add_argument("--quiet", action="store_true", help="不打印每个请求")
    args = ap.parse_args(argv)

    root = Path(args.dir)
    if not root.is_dir():
        print(f"[serve-apps] 目录不存在：{root}", file=sys.stderr)
        return 2

    pkgs = sorted(list(root.glob("*.aep")) + list(root.glob("*.tar")))
    if not pkgs:
        print(f"[serve-apps] {root} 里没有 .aep 包 —— 先用 scripts/mkapp.py 打一个：", file=sys.stderr)
        print("  python3 scripts/mkapp.py <程序> --id <名字> --image-lib-dir <镜像rootfs> "
              "--out dist/apps --tar", file=sys.stderr)
        return 2

    print(f"[serve-apps] 目录 {root.resolve()}")
    print(f"[serve-apps] 可用包（guest 里用 http://10.0.2.2:{args.port}/<文件名> 取）：")
    for p in pkgs:
        print(f"  {p.name:<24} {p.stat().st_size:>9} 字节  sha256={sha256_of(p)[:16]}…")
    print(f"[serve-apps] 监听 {args.host}:{args.port}（Ctrl-C 停止）")
    print("[serve-apps] guest 里的典型流程：")
    print(f"  wget http://10.0.2.2:{args.port}/{pkgs[0].name} -O /var/tmp/{pkgs[0].name}")
    print(f"  mkdir -p /var/tmp/pkg && tar xf /var/tmp/{pkgs[0].name} -C /var/tmp/pkg")
    print(f"  aetherd app install /var/tmp/pkg/<应用id>")

    handler = build_handler(root, verbose=not args.quiet)
    try:
        with Server((args.host, args.port), handler) as httpd:
            httpd.serve_forever()
    except KeyboardInterrupt:
        print("\n[serve-apps] 已停止")
    except OSError as e:
        if e.errno in (48, 98, 10048):  # EADDRINUSE（Linux/macOS/Windows 三套码）
            print(f"[serve-apps] 端口 {args.port} 已被占用（换个 --port）", file=sys.stderr)
            return 1
        print(f"[serve-apps] 监听失败：{e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
