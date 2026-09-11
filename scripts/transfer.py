#!/usr/bin/env python3
"""把本地文件经 SFTP 传到构建机（CRLF 归一 + 重试）。
单连接传全部文件；写入走 putfo（内容经内存缓冲，远端路径经 vm.vm_path_of 重定根）。
用法: python scripts/transfer.py <本地相对路径> <VM绝对路径> [<本地> <VM> ...]
源码内容一律先经 Write/Edit 工具提交审查，本脚本只做传输。
注意：Git Bash 下命令行里的 /home/... 会被 MSYS 改写成本地 Windows 路径，
调用时须加 MSYS2_ARG_CONV_EXCL="*"。"""
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

for round_no in range(4):
    try:
        c = vm.client()
        s = c.open_sftp()
        for local_rel, remote in pairs:
            local_abs = os.path.join(vm.LOCAL_ROOT, local_rel.replace("/", os.sep))
            data = open(local_abs, "rb").read().replace(b"\r\n", b"\n")
            safe = vm.vm_path_of(remote)
            s.putfo(io.BytesIO(data), safe)
            got = s.stat(safe).st_size
            assert got == len(data), (got, len(data))
            print(f"ok {local_rel} -> {safe} ({len(data)} bytes)")
        c.close()
        sys.exit(0)
    except SystemExit:
        raise
    except Exception:
        traceback.print_exc()
        print(f"--- round {round_no + 1} failed, retrying...")
        time.sleep(4)
sys.exit("transfer failed after retries")
