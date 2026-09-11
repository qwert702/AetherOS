#!/usr/bin/env python3
"""AetherOS build VM helper: run commands over SSH (password auth).
文件上传统一走 scripts/transfer.py（路径重定根 + putfo + 重试）。"""
import os
import posixpath
import sys
import paramiko

HOST = "127.0.0.1"
PORT = 2222
USER = "aether"
PASSWORD = "aetheros"

LOCAL_ROOT = r"d:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether"
VM_ROOT = "/home/aether"


def client():
    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    c.connect(HOST, port=PORT, username=USER, password=PASSWORD, timeout=15)
    return c


def run(c, cmd, timeout=60):
    stdin, stdout, stderr = c.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    rc = stdout.channel.recv_exit_status()
    return rc, out, err


def sh(cmd, timeout=60):
    c = client()
    try:
        rc, out, err = run(c, cmd, timeout)
        print(out)
        if err.strip():
            print("[stderr]", err, file=sys.stderr)
        sys.exit(rc)
    finally:
        c.close()


def vm_path_of(vm_rel):
    """把目标路径规范到 /home/aether 之下（防穿越）：拒绝 ..，越界段一律重定根。"""
    if ".." in vm_rel.replace("\\", "/").split("/"):
        raise SystemExit(f"VM 路径越界: {vm_rel}")
    clean = posixpath.normpath("/" + vm_rel.replace("\\", "/").strip("/"))
    return "/home/aether" + ("" if clean == "/" else clean)


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "sh":
        sh(sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 60)
    elif mode == "put":
        # 上传统一到 scripts/transfer.py（带路径重定根与重试；Git Bash 下
        # 需 MSYS2_ARG_CONV_EXCL="*"，否则 /home/... 参数被 MSYS 改写）。
        print("请改用: MSYS2_ARG_CONV_EXCL=* python scripts/transfer.py <本地相对路径> <VM绝对路径>",
              file=sys.stderr)
        sys.exit(2)
    else:
        print("usage: vm.py sh <cmd> [timeout] | put(→transfer.py)", file=sys.stderr)
        sys.exit(2)
