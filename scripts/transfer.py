#!/usr/bin/env python3
"""把本地文件传到构建机（CRLF 归一 + 重试）。
主通道 SFTP putfo（远端路径经 vm.vm_path_of 重定根）；构建机 sftp-server 偶发
SSHFX_NO_SUCH_FILE 抽风时，自动降级为 exec+base64 通道（stdin 管道，不经命令行参数）。
用法: python scripts/transfer.py <本地相对路径> <VM绝对路径> [<本地> <VM> ...]
源码内容一律先经 Write/Edit 工具提交审查，本脚本只做传输。
注意：Git Bash 下调用需 MSYS2_ARG_CONV_EXCL="*"（见交接文档 5.5）。"""
import base64
import io
import os
import sys
import time
import traceback

sys.path.insert(0, os.path.dirname(__file__))
import vm  # noqa: E402

pairs = list(zip(sys.argv[1::2], sys.argv[2::2]))
if not pairs:
    sys.exit("usage: transfer.py <local> <vm-path> ...")


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
            raise RuntimeError(f"base64 通道失败 rc={rc}: {stderr.read().decode(errors='replace')}")
    got = s.stat(remote).st_size
    assert got == len(data), (got, len(data))


for round_no in range(4):
    try:
        c = vm.client()
        for local_rel, remote in pairs:
            local_abs = os.path.join(vm.LOCAL_ROOT, local_rel.replace("/", os.sep))
            safe = vm.vm_path_of(remote)
            upload(c, local_abs, safe)
            print(f"ok {local_rel} -> {safe}")
        c.close()
        sys.exit(0)
    except SystemExit:
        raise
    except Exception:
        traceback.print_exc()
        print(f"--- round {round_no + 1} failed, retrying...")
        time.sleep(4)
sys.exit("transfer failed after retries")
