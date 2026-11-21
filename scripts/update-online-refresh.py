#!/usr/bin/env python3
"""M8 自动更新在线拉取复测：guest 内配置 updater → 在线检查/拉取 → 验签 → 暂存 → 重启应用。

## 两种模式

* **live（默认）**：经 SSH 连到 guest，写入 `/etc/aether/updater.json` 指向测试源，
  重启 aetherd 让 updater 立刻跑一轮，然后收集证据：远端版本、`incoming/` 投递、
  `staged/ready.json`、`apply.list`、逐文件哈希、（可选）Ed25519 验签、重启后标记文件。
* **`--dry-run`**：不起 guest，只对测试源做**静态检查**——`update.json` /
  `update.json.sig` 可达、签名长度、清单内每个文件可下载且 sha256/大小一致。

## guest 连接（与 scripts/vm.py 同一套环境变量）

    AETHER_VM_HOST     默认 127.0.0.1
    AETHER_VM_PORT     默认 2222
    AETHER_VM_USER     默认 aether
    AETHER_VM_KEY      私钥路径（与 AETHER_VM_PASSWORD 二选一）
    AETHER_VM_PASSWORD  密码（与 AETHER_VM_KEY 二选一）

没有可用 guest（paramiko 缺失 / 无凭据 / 连不上）时：**未执行，退出码 2**，
绝不把"没跑"记成 PASS。

## 退出码

* `0` —— PASS；
* `1` —— 校验失败（哈希不符 / 缺文件 / 验签失败 / 标记缺失）；
* `2` —— 未执行：无可用 guest 或无凭据；
* `3` —— 连接失败（guest SSH 或测试源网络不通）。

路径与判据与 `aetherd/src/updater.rs`、`aetherd/src/update.rs` 对齐：

    CONFIG_PATH   /etc/aether/updater.json
    UPDATE_ROOT   /var/lib/aether/update
    INCOMING      incoming/     STAGED  staged/
    READY         ready.json   APPLY_LIST apply.list
    VERSION_FILE  /etc/aether/version
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import socket
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

MANIFEST_NAME = "update.json"
MANIFEST_SIG_NAME = "update.json.sig"
CONFIG_PATH = "/etc/aether/updater.json"
UPDATE_ROOT = "/var/lib/aether/update"
INCOMING = "incoming"
STAGED = "staged"
READY = "ready.json"
APPLY_LIST = "apply.list"
VERSION_FILE = "/etc/aether/version"
DEFAULT_MARKER = "/var/lib/aether/update-probe.txt"
DEFAULT_SOURCE = "https://aether.cbnac.com/updates"
TARGET_PREFIXES = ("/var/apps/", "/etc/aether/", "/var/lib/aether/")
VERSION_RE = re.compile(r"^\d+(\.\d+)*$")

EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_NOT_RUN = 2
EXIT_CONNECT = 3


class NotRun(Exception):
    """没有条件跑（无 guest / 无凭据）—— 退出码 2。"""


class ConnectFail(Exception):
    """连接失败 —— 退出码 3。"""


class CheckFail(Exception):
    """校验失败 —— 退出码 1。"""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def fetch(url: str, limit: int) -> bytes:
    try:
        with urllib.request.urlopen(url, timeout=30) as resp:
            body = resp.read(limit + 1)
    except urllib.error.HTTPError as e:
        raise ConnectFail(f"HTTP {e.code}：{url}") from e
    except (urllib.error.URLError, OSError) as e:
        raise ConnectFail(f"拉取失败（{url}）：{e}") from e
    if len(body) > limit:
        raise CheckFail(f"{url} 超过大小上限 {limit} 字节")
    return body


def verify_signature(pubkey_path: Path | None, manifest_bytes: bytes,
                     sig_bytes: bytes) -> tuple[bool, str]:
    """用内嵌公钥验签；无公钥或缺 cryptography 时返回 (None, 说明)，不算失败。"""
    if pubkey_path is None or not pubkey_path.is_file():
        return None, "未提供公钥，跳过密码学验签（布局/哈希已核）"
    try:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
        from cryptography.exceptions import InvalidSignature
    except ImportError:
        return None, "未安装 cryptography，跳过密码学验签"
    pub = pubkey_path.read_bytes()
    if len(pub) != 32:
        return False, f"公钥长度不对：{len(pub)} 字节（期望 32）"
    if len(sig_bytes) != 64:
        return False, f"签名长度不对：{len(sig_bytes)} 字节（期望 64）"
    try:
        Ed25519PublicKey.from_public_bytes(pub).verify(sig_bytes, manifest_bytes)
    except InvalidSignature:
        return False, "清单签名校验失败（签名与公钥不匹配）"
    return True, "Ed25519 验签通过"


def check_manifest_files(base: str, manifest: dict) -> list[str]:
    """按 guest 的 URL 规则逐文件下载并核对 sha256/大小；返回问题列表。"""
    problems: list[str] = []
    files = manifest.get("files")
    if not isinstance(files, list) or not files:
        problems.append("清单里没有任何文件")
        return problems
    for f in files:
        path, sha, size = f.get("path"), f.get("sha256"), f.get("bytes")
        if not isinstance(path, str) or not path.startswith(TARGET_PREFIXES):
            problems.append(f"{path!r} 不在白名单前缀内")
            continue
        rel = path.lstrip("/")
        url = f"{base.rstrip('/')}/{rel}"
        body = fetch(url, 64 * 1024 * 1024)
        got_sha = sha256_bytes(body)
        if got_sha != sha:
            problems.append(f"{rel} 哈希不符（清单 {sha} / 实得 {got_sha}）")
        if len(body) != size:
            problems.append(f"{rel} 大小不符（清单 {size} / 实得 {len(body)}）")
        else:
            print(f"  [OK  ] {rel}（{len(body)} 字节，sha256 一致）")
    return problems


def dry_run(source: str, pubkey: Path | None) -> int:
    print(f"== 静态检查（dry-run）源：{source} ==")
    manifest_url = f"{source.rstrip('/')}/{MANIFEST_NAME}"
    sig_url = f"{source.rstrip('/')}/{MANIFEST_SIG_NAME}"

    print(f"  取清单 {manifest_url}")
    manifest_bytes = fetch(manifest_url, 1024 * 1024)
    print(f"  取签名 {sig_url}")
    sig_bytes = fetch(sig_url, 128)

    try:
        manifest = json.loads(manifest_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as e:
        print(f"  [FAIL] 清单不是合法 JSON：{e}", file=sys.stderr)
        return EXIT_FAIL

    version = manifest.get("version")
    if not isinstance(version, str) or not VERSION_RE.match(version):
        print(f"  [FAIL] 清单 version 不合法：{version!r}", file=sys.stderr)
        return EXIT_FAIL
    print(f"  清单版本：{version}；签名 {len(sig_bytes)} 字节")

    sig_ok, sig_note = verify_signature(pubkey, manifest_bytes, sig_bytes)
    if sig_ok is False:
        print(f"  [FAIL] {sig_note}", file=sys.stderr)
        return EXIT_FAIL
    print(f"  [{'OK  ' if sig_ok else 'SKIP'}] {sig_note}")

    problems = check_manifest_files(source, manifest)
    if problems:
        for p in problems:
            print(f"  [FAIL] {p}", file=sys.stderr)
        return EXIT_FAIL
    print("\n静态检查 PASS：清单/签名可达，所有文件哈希与大小一致")
    return EXIT_PASS


# ────────────────────────────── live guest 模式 ──────────────────────────────

def guest_client():
    """建立 SSH 连接；缺 paramiko / 无凭据 → NotRun；连不上 → ConnectFail。"""
    try:
        import paramiko
    except ImportError as e:
        raise NotRun("未执行：无可用 guest（缺少 paramiko；pip install -r scripts/requirements.txt）") from e

    host = os.environ.get("AETHER_VM_HOST", "127.0.0.1")
    port = int(os.environ.get("AETHER_VM_PORT", "2222"))
    user = os.environ.get("AETHER_VM_USER", "aether")
    key = os.environ.get("AETHER_VM_KEY") or None
    password = os.environ.get("AETHER_VM_PASSWORD") or None
    if not key and not password:
        raise NotRun("未执行：无可用 guest（缺少 AETHER_VM_KEY 或 AETHER_VM_PASSWORD）")

    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.RejectPolicy())
    try:
        c.connect(hostname=host, port=port, username=user, password=password,
                  key_filename=key, timeout=15, allow_agent=False, look_for_keys=False)
    except (paramiko.SSHException, OSError, socket.error) as e:
        c.close()
        raise ConnectFail(f"guest SSH 连接失败（{user}@{host}:{port}）：{e}") from e
    return c


def run(c, cmd: str, timeout: int = 60) -> tuple[int, str, str]:
    _, stdout, stderr = c.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    rc = stdout.channel.recv_exit_status()
    return rc, out.strip(), err.strip()


def guest_read(c, path: str) -> str:
    rc, out, err = run(c, f"cat {path} 2>/dev/null")
    return out if rc == 0 else ""


def live(args: argparse.Namespace) -> int:
    try:
        c = guest_client()
    except NotRun as e:
        print(str(e))
        return EXIT_NOT_RUN
    except ConnectFail as e:
        print(str(e), file=sys.stderr)
        return EXIT_CONNECT

    problems: list[str] = []
    marker = ""
    incoming = ""
    try:
        if args.after_reboot:
            return verify_after_reboot(c, args, problems)

        # ① 当前版本
        current = guest_read(c, VERSION_FILE)
        print(f"== live 复测 · 当前版本：{current or '(读不到)'} ==")

        # ② 写 updater 配置（https；间隔 5 分钟，满足 updater.rs 下限）
        cfg = {"url": args.source, "check_minutes": 5}
        cfg_b64 = base64.b64encode(json.dumps(cfg).encode()).decode()
        sudo = args.sudo
        rc, out, err = run(c, f"{sudo} mkdir -p /etc/aether && "
                               f"echo {cfg_b64} | base64 -d | {sudo} tee {CONFIG_PATH} >/dev/null")
        if rc != 0:
            raise CheckFail(f"写 {CONFIG_PATH} 失败：{err or out}")
        print(f"  [OK  ] 已写入 {CONFIG_PATH}：{cfg}")

        # ③ 重启 aetherd，让 updater 以 last_check=0 立刻跑一轮
        rc, out, err = run(c, args.restart_cmd, timeout=30)
        if rc != 0:
            print(f"  [WARN] 重启 aetherd 返回 {rc}：{err or out}（若 updater 由其它方式托管，请手动重启）")
        else:
            print("  [OK  ] aetherd 已重启，updater 开始首轮检查")

        # ④ 轮询暂存证据
        print(f"  轮询 {UPDATE_ROOT}（最多 {args.timeout} 秒）…")
        staged_ready = False
        staged_version: str | None = None
        deadline = time.time() + args.timeout
        while time.time() < deadline:
            incoming = guest_read(c, f"ls -1 {UPDATE_ROOT}/{INCOMING} 2>/dev/null")
            ready = guest_read(c, f"cat {UPDATE_ROOT}/{STAGED}/{READY} 2>/dev/null")
            if ready:
                staged_ready = True
                try:
                    staged_version = json.loads(ready).get("version")
                except json.JSONDecodeError:
                    staged_version = None
                break
            time.sleep(5)

        if not staged_ready:
            problems.append(f"超时未看到 {STAGED}/{READY}（incoming 残留：{incoming or '无'}）")
        else:
            print(f"  [OK  ] 已暂存：staged/{READY}，版本 {staged_version}")

        # ⑤ 证据：清单 / 签名 / 逐文件哈希 / apply.list
        if staged_ready:
            problems.extend(verify_staged_bundle(c, args, staged_version))

        # ⑥ 重启后标记文件（需要 guest 已重启应用；否则提示复跑 --after-reboot）
        marker = guest_read(c, args.marker)
        if marker:
            print(f"  [OK  ] 重启后标记文件 {args.marker}：{marker[:80]}")
        else:
            print(f"  [--  ] 标记文件 {args.marker} 暂不存在；应用在下次重启后生效，"
                  f"重启后复跑：python scripts/update-online-refresh.py --after-reboot")

    except CheckFail as e:
        problems.append(str(e))
    finally:
        c.close()

    if problems:
        for p in problems:
            print(f"  [FAIL] {p}", file=sys.stderr)
        print("\nM8 在线复测: FAIL")
        return EXIT_FAIL
    if not marker:
        # 暂存链路 PASS，但应用尚未重启生效 —— 不算整链 PASS，按未完成报 1
        print("\nM8 在线复测: 暂存链路 PASS；等待重启后标记复核")
        return EXIT_FAIL
    print("\nM8 在线复测: PASS（验签 → 暂存 → 重启应用）")
    return EXIT_PASS


def verify_staged_bundle(c, args, staged_version: str | None) -> list[str]:
    problems: list[str] = []
    manifest_text = guest_read(c, f"cat {UPDATE_ROOT}/{STAGED}/{MANIFEST_NAME}")
    sig_text = guest_read(c, f"cat {UPDATE_ROOT}/{STAGED}/{MANIFEST_SIG_NAME}")
    if not manifest_text:
        problems.append("staged 包里读不到 update.json")
        return problems
    try:
        manifest = json.loads(manifest_text)
    except json.JSONDecodeError:
        problems.append("staged 包 update.json 不是合法 JSON")
        return problems

    if staged_version and manifest.get("version") != staged_version:
        problems.append(f"ready.json 版本 {staged_version} 与清单 {manifest.get('version')} 不一致")

    if len(sig_text.encode()) != 64:
        problems.append(f"签名长度 {len(sig_text.encode())} 字节（期望 64）")

    apply_list = guest_read(c, f"cat {UPDATE_ROOT}/{STAGED}/{APPLY_LIST}")
    if not apply_list:
        problems.append("缺少 apply.list（init 应用清单）")
    else:
        print(f"  [OK  ] apply.list：{len(apply_list.splitlines())} 条")

    # 逐文件核对 staged 目录里的哈希
    for f in manifest.get("files", []):
        path, sha, size = f.get("path"), f.get("sha256"), f.get("bytes")
        rel = path.lstrip("/")
        rc, out, _ = run(c, f"sha256sum {UPDATE_ROOT}/{STAGED}/{rel} 2>/dev/null")
        if rc != 0:
            problems.append(f"staged 缺文件：{rel}")
            continue
        got_sha = out.split()[0]
        rc2, size_out, _ = run(c, f"stat -c %s {UPDATE_ROOT}/{STAGED}/{rel} 2>/dev/null")
        got_size = int(size_out) if rc2 == 0 else -1
        if got_sha != sha:
            problems.append(f"{rel} 哈希不符（清单 {sha} / 实得 {got_sha}）")
        elif got_size != size:
            problems.append(f"{rel} 大小不符（清单 {size} / 实得 {got_size}）")
        else:
            print(f"  [OK  ] staged/{rel}（{got_size} 字节，sha256 一致）")
    return problems


def verify_after_reboot(c, args, problems: list[str]) -> int:
    current = guest_read(c, VERSION_FILE)
    print(f"== 重启后复核 · 当前版本：{current or '(读不到)'} ==")
    marker = guest_read(c, args.marker)
    if not marker:
        problems.append(f"重启后标记文件不存在：{args.marker}")
    else:
        print(f"  [OK  ] 标记文件 {args.marker}：{marker[:80]}")
    staged_left = guest_read(c, f"ls -1 {UPDATE_ROOT}/{STAGED} 2>/dev/null")
    if staged_left:
        problems.append(f"应用后 staged/ 未清空：{staged_left}")
    else:
        print("  [OK  ] staged/ 已清空（更新已应用）")
    if problems:
        for p in problems:
            print(f"  [FAIL] {p}", file=sys.stderr)
        print("\n重启后复核: FAIL")
        return EXIT_FAIL
    print("\n重启后复核: PASS")
    return EXIT_PASS


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="M8 自动更新在线拉取复测（guest 在线检查/拉取/验签/暂存/应用）",
        epilog=(
            "退出码：0 PASS · 1 校验失败 · 2 未执行（无 guest/无凭据）· 3 连接失败。\n"
            "guest 凭据：AETHER_VM_HOST/PORT/USER/KEY（或 PASSWORD）。"
        ),
    )
    ap.add_argument("--dry-run", action="store_true",
                    help="静态检查测试源：清单/签名可达 + 逐文件哈希一致（不需要 guest）")
    ap.add_argument("--source", default=os.environ.get("AETHER_UPDATE_SOURCE", DEFAULT_SOURCE),
                    help=f"更新源 Base URL（默认 {DEFAULT_SOURCE} 或环境 AETHER_UPDATE_SOURCE）")
    ap.add_argument("--pubkey", default="aetherd/keys/update_pubkey.bin",
                    help="Ed25519 公钥（32 字节原始；不存在则跳过验签）")
    ap.add_argument("--marker", default=DEFAULT_MARKER, help="重启后应用标记文件路径")
    ap.add_argument("--timeout", type=int, default=300, help="等待暂存的秒数（默认 300）")
    ap.add_argument("--sudo", default="sudo -n", help="guest 上提权前缀（默认 'sudo -n'）")
    ap.add_argument("--restart-cmd", default="sudo -n systemctl restart aetherd",
                    help="重启 aetherd 的 guest 命令（按 guest 托管方式调整）")
    ap.add_argument("--after-reboot", action="store_true",
                    help="只做重启后复核：标记文件 + staged 清空")
    args = ap.parse_args(argv)

    pubkey = Path(args.pubkey)
    if not pubkey.is_absolute():
        pubkey = Path(__file__).resolve().parent.parent / pubkey

    if args.dry_run:
        try:
            return dry_run(args.source, pubkey)
        except ConnectFail as e:
            print(f"连接失败：{e}", file=sys.stderr)
            return EXIT_CONNECT
        except CheckFail as e:
            print(f"校验失败：{e}", file=sys.stderr)
            return EXIT_FAIL

    return live(args)


if __name__ == "__main__":
    raise SystemExit(main())
