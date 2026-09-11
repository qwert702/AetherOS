#!/usr/bin/env python3
"""AetherOS build VM helper: sync files & run commands over SSH (password auth)."""
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


def put(local_rel, vm_path):
    c = client()
    try:
        sftp = c.open_sftp()
        local = LOCAL_ROOT + "\\" + local_rel.replace("/", "\\")
        data = open(local, "rb").read()
        with sftp.open(vm_path, "wb") as f:
            f.write(data)
        print(f"put {local_rel} -> {vm_path} ({sftp.stat(vm_path).st_size} bytes)")
        sftp.close()
    finally:
        c.close()


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "sh":
        sh(sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 60)
    elif mode == "put":
        put(sys.argv[2], sys.argv[3])
    else:
        print("usage: vm.py sh <cmd> [timeout] | put <local-rel> <vm-path>", file=sys.stderr)
        sys.exit(2)
