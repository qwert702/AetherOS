#!/usr/bin/env python3
"""上传 aether-install 源码到构建机并触发重建（自包含，纯 SFTP）。"""
import io
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import vm  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LOCAL = os.path.join(ROOT, "aether-install", "src", "main.rs")
REMOTE = "/home/aether/aether-install/src/main.rs"


def upload(c, local_abs, remote):
    data = open(local_abs, "rb").read().replace(b"\r\n", b"\n")
    s = c.open_sftp()
    s.putfo(io.BytesIO(data), remote)
    got = s.stat(remote).st_size
    if got != len(data):  # 不用 assert：-O 下会被剥离（审计 L-6）
        raise RuntimeError(f"上传长度不符：远端 {got} / 本地 {len(data)}（{remote}）")


c = vm.client()
data = open(LOCAL, "rb").read().replace(b"\r\n", b"\n")
for attempt in range(4):
    try:
        upload(c, LOCAL, REMOTE)
        print(f"ok -> {REMOTE} ({len(data)} bytes)")
        break
    except Exception as e:
        print(f"attempt {attempt + 1} failed: {e!r}")
        time.sleep(3)
else:
    sys.exit("upload failed")
c.close()
