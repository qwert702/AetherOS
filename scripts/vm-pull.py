#!/usr/bin/env python3
"""从构建机拉文件到本地（构建产物、日志、截图）。

用法: python scripts/vm-pull.py <VM路径> <本地路径>

**Git Bash 下必须加 `MSYS2_ARG_CONV_EXCL="*"`** —— 否则 `/home/aether/x.iso`
会被 MSYS 改写成 `C:/Users/.../PortableGit/versions/x/home/aether/x.iso`，
然后 sftp 报 `FileNotFoundError(2, 'No such file')`。这个错误看起来像"远端没有
这个文件"，实际是**本地 shell 改写了参数**，很容易查错方向（`transfer.py` 上传
同样需要它，见交接文档 5.5）。

为什么需要它：`transfer.py` 只做**上传**；而下载走 sftp 时构建机的 sftp-server
偶发 `SSHFX_NO_SUCH_FILE`（见交接文档），所以这里带重试。

注意：**拉二进制后要对 md5** —— `transfer.py` 曾经因为对所有文件做 CRLF 归一
而损坏二进制（已修），但"传完就核对"这个习惯值得保留。
"""
import os
import sys
import time
import traceback

sys.path.insert(0, os.path.dirname(__file__))
import vm  # noqa: E402


def pull(c, remote, local):
    s = c.open_sftp()
    print(f"    sftp cwd={s.getcwd()!r} remote={remote!r} local={local!r}", flush=True)
    s.get(remote, local)
    return os.path.getsize(local)


def main():
    if len(sys.argv) < 3:
        sys.exit("usage: vm-pull.py <vm-path> <local-path>")
    remote, local = sys.argv[1], sys.argv[2]
    parent = os.path.dirname(os.path.abspath(local))
    print(f"    本地父目录={parent!r} 存在={os.path.isdir(parent)}", flush=True)
    os.makedirs(parent or ".", exist_ok=True)

    last = None
    for attempt in range(2):
        try:
            c = vm.client()
            try:
                n = pull(c, remote, local)
            finally:
                c.close()
            print(f"ok {remote} -> {local} ({n} bytes)")
            return 0
        except Exception as e:  # noqa: BLE001
            last = e
            print(f"--- 第 {attempt + 1} 次失败: {e!r}")
            traceback.print_exc()
            time.sleep(2)
    print(f"!! 拉取失败: {last!r}")
    print("   若反复失败，可改用：vm.py sh \"base64 -w0 <路径>\" 走 exec 通道")
    return 1


if __name__ == "__main__":
    sys.exit(main())
