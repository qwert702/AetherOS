#!/usr/bin/env python3
"""vm-sync.py —— 把本地工作树同步到构建机（VirtualBox 里的 AetherOS-Build）。

## 为什么不能用 git

构建机**访问不了 GitHub**（`git fetch` 报 `Connection reset by peer`）。所以镜像同步只能走 SFTP。
2026-09-30 我在这里踩过一个很贵的坑，写下来免得重演：

    cd /home/aether && git fetch origin && git reset --hard origin/main

`git fetch` 失败后，`&&` 链因为管道里 `tail` 的退出码是 0 而**继续执行**了
`git reset --hard origin/main` —— 用的是**陈旧的本地 ref**（初始发布提交），
把之前 SFTP 传上去的全部 P0–P3 源码冲掉了，并且下一步就基于远古代码构建了 ISO。
**结论：构建机上永远不要跑 `git reset --hard`。** 同步只用本脚本。

## 为什么必须归一化行尾与权限

Windows 工作树是 CRLF（`core.autocrlf=true`），而 SFTP 是**字节级**的：

- 带 CRLF 的 shell 脚本在 guest 里报 `$'\r': command not found`；
- `platform/overlay/init` 变成 `#!/bin/sh\r` → 内核 exec 时报
  `Failed to execute /init (error -2)`（ENOENT：找不到 `/bin/sh\r` 这个解释器），
  ISO 于是回退到 Buildroot 默认 init，**开机进了 login 提示而不是桌面**；
- SFTP 也不保留可执行位，`/init` 会丢掉 `+x`。

所以本脚本对**已知的文本扩展名**做 CRLF→LF，并给 overlay/脚本恢复可执行位。

## 用法

    python scripts/vm-sync.py              # 同步全部（排除 .git/target/platform build）
    python scripts/vm-sync.py --only scripts platform  # 只同步部分目录

凭据（`AETHER_VM_PASSWORD` 或 `AETHER_VM_KEY`）见 `scripts/vm.py` / `scripts/setup-vm.ps1`。
"""

from __future__ import annotations

import argparse
import os
import posixpath
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vm  # noqa: E402

REMOTE_ROOT = "/home/aether"

#: 只对这些扩展名（以及无扩展名的 init/defconfig）做行尾归一化 —— 绝不碰 PNG/GIF 等二进制。
TEXT_EXT = {".rs", ".toml", ".sh", ".py", ".md", ".cfg", ".conf", ".txt",
            ".html", ".in", ".mk", ".json", ".c", ".h", ".ps1", ".yml", ".yaml"}
TEXT_NAMES = {"init", "Cargo.lock", "Cargo.toml"}

#: 需要恢复可执行位的路径（overlay 里的启动脚本）。
EXECUTABLE = ["platform/overlay/init"]

#: 永不覆盖：Buildroot 构建树与产物（几十分钟的编译成果）。
SKIP_PREFIX = ("platform/build", "platform/output")


def normalize(rel: str) -> str:
    return rel.replace("\\", "/")


def skipped(rel: str) -> bool:
    r = normalize(rel)
    first = r.split("/")[0]
    if first in (".git", "target", ".temp", "__pycache__", "node_modules"):
        return True
    return r.startswith(SKIP_PREFIX)


def is_text(rel: str) -> bool:
    base = posixpath.basename(normalize(rel))
    return os.path.splitext(base)[1].lower() in TEXT_EXT or base in TEXT_NAMES


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="同步本地工作树到构建机（SFTP + LF 归一化）。")
    ap.add_argument("--only", nargs="*", help="只同步这些顶层目录/文件")
    args = ap.parse_args(argv)
    root = os.getcwd()

    c = vm.client()
    sftp = c.open_sftp()
    made: set[str] = set()

    def ensure(remote_dir: str) -> None:
        if remote_dir in made or remote_dir in ("", "/"):
            return
        try:
            sftp.stat(remote_dir)
        except IOError:
            ensure(posixpath.dirname(remote_dir))
            try:
                sftp.mkdir(remote_dir)
            except IOError:
                pass
        made.add(remote_dir)

    count = 0
    converted = 0
    total = 0
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [
            d for d in dirnames
            if not skipped(normalize(os.path.relpath(os.path.join(dirpath, d), root)))
        ]
        for fn in filenames:
            local = os.path.join(dirpath, fn)
            rel = normalize(os.path.relpath(local, root))
            if skipped(rel):
                continue
            if args.only and rel.split("/")[0] not in args.only and rel not in args.only:
                continue
            remote = posixpath.join(REMOTE_ROOT, rel)
            ensure(posixpath.dirname(remote))
            data = open(local, "rb").read()
            if is_text(rel) and b"\r\n" in data:
                data = data.replace(b"\r\n", b"\n")
                converted += 1
            # putfo 直接写字节，避免 Windows 文本模式带来的二次转换
            sftp.putfo(__import__("io").BytesIO(data), remote)
            count += 1
            total += len(data)
    for rel in EXECUTABLE:
        try:
            sftp.chmod(posixpath.join(REMOTE_ROOT, rel), 0o755)
        except IOError as e:
            print("  警告：无法设置可执行位 %s: %s" % (rel, e))
    sftp.close()
    c.close()
    print("已同步 %d 个文件（%.1f MB），其中 %d 个做了 CRLF→LF" % (count, total / 1048576, converted))
    print("提醒：**不要在构建机上跑 git reset --hard**（见本文件头注释）")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
