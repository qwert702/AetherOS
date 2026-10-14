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
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
# 注意：`import vm` 推迟到 `main()` 里 —— 它要 paramiko，而 `--selftest` 只验纯函数，
# 没装 paramiko 的 Python（比如 dsh 内置运行时）也应该能跑这条自检。

REMOTE_ROOT = "/home/aether"

#: 只对这些扩展名（以及无扩展名的 init/defconfig）做行尾归一化 —— 绝不碰 PNG/GIF 等二进制。
TEXT_EXT = {".rs", ".toml", ".sh", ".py", ".md", ".cfg", ".conf", ".txt",
            ".html", ".in", ".mk", ".json", ".c", ".h", ".ps1", ".yml", ".yaml"}
TEXT_NAMES = {"init", "Cargo.lock", "Cargo.toml"}

#: `git` 不可用时的兜底名单（至少要保证 ISO 构建能跑起来的那几个）。
EXECUTABLE = [
    "platform/overlay/init",
    "platform/br2-external/board/aether/post-build.sh",
    "platform/build-iso.sh",
]


def executable_from_git(root: str) -> list[str]:
    """从 git 索引的 mode 位取出"该有 `+x`"的文件清单。

    为什么必须这么做（2026-10-11 实测事故）：SFTP **不保留可执行位**，而 Windows
    工作树本身也不带这个信息。第一版只写了 `platform/overlay/init` 一个白名单路径，
    结果 `platform/br2-external/board/aether/post-build.sh` 上传后丢了 `+x`，
    Buildroot 一路跑到 `target-finalize` 才报：

        /bin/bash: …/post-build.sh: Permission denied
        make[1]: *** [Makefile:752: target-finalize] Error 126

    —— 失败点离原因（同步）很远，白花了一轮构建。git 索引是 Windows 上
    **唯一可靠**的 mode 来源（`core.filemode` 不跟踪本地 chmod，但仓库里
    提交过的 100755 位一直在）。
    """
    try:
        out = subprocess.run(["git", "ls-files", "--stage"], cwd=root,
                             capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as exc:
        print(f"  警告：读 git 索引失败（{exc}），退回内置名单")
        return []
    if out.returncode != 0:
        print("  警告：这不是 git 工作树，退回内置名单")
        return []
    files: list[str] = []
    for line in out.stdout.splitlines():
        meta, _, path = line.partition("\t")
        if path and meta.split(" ", 1)[0] == "100755":
            files.append(normalize(path))
    return files

#: 永不覆盖：Buildroot 构建树与产物（几十分钟的编译成果）。
#: **注意这里必须按"路径段"比较**（见 `skipped()`）：写成纯前缀会让
#: `platform/build-iso.sh` 也被当成 `platform/build` 的子路径一起跳过。
SKIP_PREFIX = ("platform/build", "platform/output")


def normalize(rel: str) -> str:
    return rel.replace("\\", "/")


def skipped(rel: str) -> bool:
    r = normalize(rel)
    first = r.split("/")[0]
    if first in (".git", "target", ".temp", "__pycache__", "node_modules"):
        return True
    # 按**路径段**判前缀，不是字符串前缀。2026-10-11 实测事故：`r.startswith(...)`
    # 让 `platform/build-iso.sh` 命中了 `platform/build` → 构建机上的 `build-iso.sh`
    # 一直是 **2026-09-30 的旧版**，里面**没有** isohybrid + MBR 自检那一段
    # （那是 2026-10-02 审计 H-6 才加的）。后果有两个，都很隐蔽：
    #   1. 日志里根本没有 "isohybrid" 这一行，而构建**照样报 PASS**；
    #   2. 产出的 ISO 前 512 字节全零、无 `0x55AA` —— 只能光盘引导，
    #      `aether-install` 却要求"M6 源镜像带 MBR 签名"，等于把装机堵死，
    #      而台账 #33 的验收里恰恰写着"MBR 签名自检"。
    return any(r == p or r.startswith(p + "/") for p in SKIP_PREFIX)


def is_text(rel: str) -> bool:
    base = posixpath.basename(normalize(rel))
    return os.path.splitext(base)[1].lower() in TEXT_EXT or base in TEXT_NAMES


def selftest() -> int:
    """自检：跳过规则只能跳**真正的构建树**（含正向对照）。

    2026-10-11 的真实事故：`r.startswith(("platform/build", "platform/output"))`
    按**字符串前缀**判断，于是 `platform/build-iso.sh` 被当成 `platform/build` 的
    子路径一起跳过 —— 构建机上的 `build-iso.sh` 一直是 2026-09-30 的旧版，
    里面**没有** isohybrid + MBR 自检（2026-10-02 审计 H-6 才加的）。
    后果是"构建报 PASS，但产出的 ISO 前 512 字节全零、无 `0x55AA`"，
    把 `aether-install` 的装机前提堵死，而没有任何判据会响。
    """
    cases = [
        # (相对路径, 是否应当被跳过)
        ("platform/build-iso.sh", False),          # ← 事故本体：脚本必须同步
        ("platform/build", True),
        ("platform/build/buildroot-2024.02.1/x", True),
        ("platform/build/output/images/rootfs.iso9660", True),
        ("platform/output/host/bin/isohybrid", True),
        ("platform/br2-external/board/aether/post-build.sh", False),
        ("platform/overlay/init", False),
        (".git/config", True),
        ("target/debug/aether-compositor.exe", True),
        ("scripts/vm-sync.py", False),
    ]
    failed = 0
    for rel, want in cases:
        got = skipped(rel)
        mark = "OK  " if got == want else "FAIL"
        if got != want:
            failed += 1
        print(f"  [{mark}] {rel}: 期望{'跳过' if want else '同步'}，"
              f"实得{'跳过' if got else '同步'}")
    if failed:
        print(f"[FAIL] 自检不通过：{failed} 个用例与期望不符")
        return 1
    print(f"[ OK ] 自检通过（{len(cases)} 个用例，含"
          f"「脚本不该被跳过」这条正向对照）")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="同步本地工作树到构建机（SFTP + LF 归一化）。")
    ap.add_argument("--only", nargs="*", help="只同步这些顶层目录/文件")
    ap.add_argument("--selftest", action="store_true", help="只验跳过规则（不需要 paramiko）")
    args = ap.parse_args(argv)
    if args.selftest:
        return selftest()

    import vm  # 延迟导入：见文件上方注释（paramiko 只在真正同步时才需要）

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
    # 可执行位：优先用 git 索引（覆盖全部 45 个脚本），读不到才退回内置名单。
    exe = executable_from_git(root) or list(EXECUTABLE)
    source = "git 索引" if exe is not EXECUTABLE else "内置名单"
    fixed = 0
    for rel in exe:
        try:
            sftp.chmod(posixpath.join(REMOTE_ROOT, rel), 0o755)
            fixed += 1
        except IOError as e:
            print("  警告：无法设置可执行位 %s: %s" % (rel, e))
    sftp.close()
    c.close()
    print("已同步 %d 个文件（%.1f MB），其中 %d 个做了 CRLF→LF" % (count, total / 1048576, converted))
    print("已按 %s 恢复 %d 个文件的可执行位" % (source, fixed))
    print("提醒：**不要在构建机上跑 git reset --hard**（见本文件头注释）")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
