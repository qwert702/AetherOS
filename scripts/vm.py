#!/usr/bin/env python3
"""AetherOS 构建机 SSH 助手：远程命令（连接建立 + 复用接口）。

用法:
    python scripts/vm.py sh "<命令>" [超时秒]

凭据一律走环境变量 —— **脚本内不再硬编码密码**：
    AETHER_VM_PASSWORD     构建机登录密码（与 AETHER_VM_KEY 二选一）
    AETHER_VM_KEY          私钥路径（公钥认证；与密码同时给时由 paramiko 按"先钥后密"尝试）
    AETHER_VM_HOST         默认 127.0.0.1
    AETHER_VM_PORT         默认 2222
    AETHER_VM_USER         默认 aether
    AETHER_VM_KNOWN_HOSTS  主机密钥记录路径，默认 scripts/.vm_known_hosts（已 gitignore）

主机密钥策略（TOFU，trust on first use）：
    首次连接把构建机密钥写入 AETHER_VM_KNOWN_HOSTS 并打印指纹；
    之后一律**严格校验**，密钥变了直接拒连。旧版用的 AutoAddPolicy 会把
    "密钥换了"也静默接受，等于没有校验。

文件上传统一走 scripts/transfer.py（路径重定根 + putfo + 重试）。
"""

from __future__ import annotations

import base64
import hashlib
import os
import posixpath
import sys

import paramiko

# 仓库根目录（scripts/ 的上一级）：不再写死某台机器的绝对路径
LOCAL_ROOT = os.environ.get("AETHER_LOCAL_ROOT") or os.path.dirname(
    os.path.dirname(os.path.abspath(__file__))
)
VM_ROOT = "/home/aether"

CONNECT_TIMEOUT = 15
_DEFAULT_KNOWN_HOSTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".vm_known_hosts")


def _env(name: str, default: str) -> str:
    """读环境变量并做基本校验：去掉首尾空白，空值回退默认。"""
    value = (os.environ.get(name) or "").strip()
    return value or default


HOST = _env("AETHER_VM_HOST", "127.0.0.1")
USER = _env("AETHER_VM_USER", "aether")


def _port() -> int:
    """端口必须显式可用：非法值直接报错，不要静默回退。"""
    raw = _env("AETHER_VM_PORT", "2222")
    try:
        port = int(raw)
    except ValueError:
        raise SystemExit(f"[vm.py] AETHER_VM_PORT 不是整数: {raw!r}")
    if not 1 <= port <= 65535:
        raise SystemExit(f"[vm.py] AETHER_VM_PORT 越界（应在 1-65535）: {port}")
    return port


PORT = _port()


def _known_hosts_path() -> str:
    return os.environ.get("AETHER_VM_KNOWN_HOSTS") or _DEFAULT_KNOWN_HOSTS


def _load_known_hosts(client: paramiko.SSHClient) -> bool:
    """加载主机密钥记录。返回是否已有记录（决定用严格策略还是首次信任）。"""
    path = _known_hosts_path()
    if not os.path.isfile(path) or os.path.getsize(path) == 0:
        return False
    try:
        client.load_host_keys(path)
    except (OSError, paramiko.SSHException) as exc:
        raise SystemExit(f"[vm.py] 主机密钥记录读取失败（{path}）：{exc}")
    return True


def _remember_host_key(client: paramiko.SSHClient) -> None:
    """首次连接：记录指纹并落盘，供后续严格校验。"""
    transport = client.get_transport()
    if transport is None:
        raise SystemExit("[vm.py] 连接已断开，无法记录主机密钥")
    key = transport.get_remote_server_key()
    # 与 OpenSSH 的 SHA256 指纹同格式，可用
    #   ssh-keyscan -p <port> <host> | ssh-keygen -lf -
    # 交叉核对
    fingerprint = base64.b64encode(hashlib.sha256(key.asbytes()).digest()).decode().rstrip("=")
    path = _known_hosts_path()
    parent = os.path.dirname(path)
    try:
        if parent:
            os.makedirs(parent, exist_ok=True)
        client.save_host_keys(path)
    except OSError as exc:
        raise SystemExit(f"[vm.py] 写入主机密钥记录失败（{path}）：{exc}")
    print(
        f"[vm.py] 首次连接：已记录主机密钥 {key.get_name()} SHA256:{fingerprint} → {path}",
        file=sys.stderr,
    )


def client() -> paramiko.SSHClient:
    """建立到构建机的 SSH 连接。失败一律带可执行的提示退出，不静默降级。"""
    password = os.environ.get("AETHER_VM_PASSWORD") or None
    key_path = os.environ.get("AETHER_VM_KEY") or None
    if not password and not key_path:
        raise SystemExit(
            "[vm.py] 缺少凭据：请先设置环境变量\n"
            "        export AETHER_VM_PASSWORD='<构建机密码>'      # 密码认证\n"
            "        export AETHER_VM_KEY=~/.ssh/id_ed25519        # 或公钥认证\n"
            "        （密码不再写死在脚本里；构建机账号见 scripts/setup-vm.ps1）"
        )
    if key_path and not os.path.isfile(os.path.expanduser(key_path)):
        raise SystemExit(f"[vm.py] AETHER_VM_KEY 指向的文件不存在: {key_path}")

    c = paramiko.SSHClient()
    has_record = _load_known_hosts(c)
    c.set_missing_host_key_policy(
        paramiko.RejectPolicy() if has_record else paramiko.AutoAddPolicy()
    )
    try:
        c.connect(
            HOST,
            port=PORT,
            username=USER,
            password=password,
            key_filename=os.path.expanduser(key_path) if key_path else None,
            timeout=CONNECT_TIMEOUT,
        )
    except (paramiko.SSHException, OSError) as exc:
        c.close()
        hint = ""
        if has_record:
            hint = (
                f"\n        若构建机是重装过的：确认无误后删除 {_known_hosts_path()} 再连；"
                "否则这可能是中间人。"
            )
        raise SystemExit(f"[vm.py] SSH 连接失败（{USER}@{HOST}:{PORT}）：{exc}{hint}")
    if not has_record:
        _remember_host_key(c)
    return c


def run(c: paramiko.SSHClient, cmd: str, timeout: int = 60) -> tuple[int, str, str]:
    """在远端执行命令，返回 (退出码, stdout, stderr)。"""
    stdin, stdout, stderr = c.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    rc = stdout.channel.recv_exit_status()
    return rc, out, err


def sh(cmd: str, timeout: int = 60) -> None:
    """执行单条命令并把结果透传到本地 stdio，以远端退出码结束进程。"""
    c = client()
    try:
        rc, out, err = run(c, cmd, timeout)
        print(out)
        if err.strip():
            print("[stderr]", err, file=sys.stderr)
        sys.exit(rc)
    finally:
        c.close()


def vm_path_of(vm_rel: str) -> str:
    """把目标路径规范到 VM_ROOT 之下（防穿越）：拒绝 ..，越界段一律重定根。
    已带 VM_ROOT 前缀的绝对路径保持不变（勿重复拼接！）。"""
    p = vm_rel.replace("\\", "/")
    if ".." in p.split("/"):
        raise SystemExit(f"VM 路径越界: {vm_rel}")
    clean = posixpath.normpath("/" + p.strip("/"))
    if clean == VM_ROOT or clean.startswith(VM_ROOT + "/"):
        return clean
    return VM_ROOT + clean


if __name__ == "__main__":
    _USAGE = (
        "usage: vm.py sh <cmd> [timeout]\n"
        "       put（已废弃）→ 请用: MSYS2_ARG_CONV_EXCL=* python scripts/transfer.py "
        "<本地相对路径> <VM绝对路径>"
    )
    if len(sys.argv) < 2:
        print(_USAGE, file=sys.stderr)
        sys.exit(2)
    mode = sys.argv[1]
    if mode == "sh":
        if len(sys.argv) < 3:
            print("usage: vm.py sh <cmd> [timeout]", file=sys.stderr)
            sys.exit(2)
        if len(sys.argv) > 3:
            try:
                timeout = int(sys.argv[3])
            except ValueError:
                print(f"timeout 必须是整数: {sys.argv[3]!r}", file=sys.stderr)
                sys.exit(2)
            if timeout <= 0:
                print(f"timeout 必须为正数: {timeout}", file=sys.stderr)
                sys.exit(2)
        else:
            timeout = 60
        sh(sys.argv[2], timeout)
    elif mode == "put":
        # 上传统一到 scripts/transfer.py（带路径重定根与重试；Git Bash 下
        # 需 MSYS2_ARG_CONV_EXCL="*"，否则 /home/... 参数被 MSYS 改写）。
        print(
            "请改用: MSYS2_ARG_CONV_EXCL=* python scripts/transfer.py <本地相对路径> <VM绝对路径>",
            file=sys.stderr,
        )
        sys.exit(2)
    else:
        print(_USAGE, file=sys.stderr)
        sys.exit(2)
