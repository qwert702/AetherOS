#!/usr/bin/env python3
"""上传 M6 安装器相关文件到构建机（自包含，含 base64 兜底通道）。"""
import base64
import io
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import vm  # noqa: E402

PAIRS = [
    ("aether-compositor/src/main.rs", "/home/aether/aether-compositor/src/main.rs"),
    ("aether-compositor/src/draw.rs", "/home/aether/aether-compositor/src/draw.rs"),
    ("aether-compositor/src/text.rs", "/home/aether/aether-compositor/src/text.rs"),
    ("aetherd/src/perm.rs", "/home/aether/aetherd/src/perm.rs"),
    ("aetherd/src/server.rs", "/home/aether/aetherd/src/server.rs"),
    ("aetherd/src/main.rs", "/home/aether/aetherd/src/main.rs"),
    ("aether-install/src/main.rs", "/home/aether/aether-install/src/main.rs"),
    ("scripts/rebuild-m4.sh", "/home/aether/rebuild-m4.sh"),
]


def upload(c, local_abs, remote):
    data = open(local_abs, "rb").read().replace(b"\r\n", b"\n")
    s = c.open_sftp()
    try:
        s.putfo(io.BytesIO(data), remote)
    except Exception as e:
        print(f"    sftp 通道失败（{e!r}），降级 base64-over-ssh")
        import shlex

        cmd = f"base64 -d > {shlex.quote(remote)}"
        stdin, stdout, stderr = c.exec_command(cmd, timeout=60)
        stdin.write(base64.b64encode(data))
        stdin.channel.shutdown_write()
        rc = stdout.channel.recv_exit_status()
        if rc != 0:
            raise RuntimeError(f"base64 通道失败 rc={rc}")
    got = s.stat(remote).st_size
    if got != len(data):  # 不用 assert：-O 下会被剥离（审计 L-6）
        raise RuntimeError(f"上传长度不符：远端 {got} / 本地 {len(data)}（{remote}）")


c = vm.client()
# 新组件目录可能不存在：先建好
rc, out, err = vm.run(c, "mkdir -p /home/aether/aether-install/src")
assert rc == 0, err
print("dir-ok")
for local_rel, remote in PAIRS:
    local_abs = os.path.join(vm.LOCAL_ROOT, local_rel.replace("/", os.sep))
    safe = vm.vm_path_of(remote)
    for attempt in range(4):
        try:
            upload(c, local_abs, safe)
            print(f"ok {local_rel} -> {safe}")
            break
        except Exception as e:
            print(f"attempt {attempt + 1} failed: {e!r}")
            time.sleep(3)
    else:
        sys.exit(f"gave up: {local_rel}")
c.close()
