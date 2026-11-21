#!/usr/bin/env python3
"""生成 AetherOS 应用包签名密钥（Ed25519）。

用法（在仓库根目录）：

    python scripts/app-keygen.py --private-key ~/.aether/app-signing-key.pem

产出：
  * 私钥：默认写到仓库**外**的 `~/.aether/app-signing-key.pem`（PKCS#8 PEM，0600）；
  * 公钥：`aetherd/keys/app_pubkey.bin`（32 字节原始公钥，可入库）；
  * 公钥 SHA-256 指纹：打印出来，供发布时核对。

⚠️ **测试密钥，非生产**：仓库内随附的 `aetherd/keys/app_pubkey.bin` 对应的私钥
只用于开发与自测，**绝不能**用于签名发布给用户的应用包。生产发布私钥由 owner
离线保管（脱机存储、不进任何仓库/镜像/脚本目录）；公钥轮换要走发布流程，
旧客户端在轮换后将不再接受新签名包。

安全要求：
  * 私钥**绝不入库**；脚本会拒绝把私钥写进仓库目录；
  * 私钥丢失 = 无法再签新应用包；请离线备份；
  * 公钥可以公开，但一旦替换，旧客户端将不再接受新包。

本脚本依赖 `cryptography`（paramiko 的间接依赖，见 scripts/requirements.txt）。
"""
from __future__ import annotations

import argparse
import hashlib
import os
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_PUBLIC = REPO / "aetherd" / "keys" / "app_pubkey.bin"
DEFAULT_PRIVATE = Path.home() / ".aether" / "app-signing-key.pem"


def _crypto():
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    except ImportError as exc:  # pragma: no cover - 只在缺依赖时走到
        raise SystemExit(
            "需要 cryptography（python -m pip install -r scripts/requirements.txt）"
        ) from exc
    return serialization, Ed25519PrivateKey


def public_bytes(private_key) -> bytes:
    serialization, _ = _crypto()
    return private_key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )


def write_keypair(private_path: Path, public_path: Path, force: bool = False) -> tuple[Path, bytes]:
    serialization, Ed25519PrivateKey = _crypto()
    private_resolved = private_path.expanduser().resolve()
    if REPO in private_resolved.parents or private_resolved == REPO:
        raise SystemExit(f"拒绝把私钥写进仓库：{private_resolved}")
    if private_resolved.exists() and not force:
        raise SystemExit(f"私钥已存在：{private_resolved}（要覆盖请加 --force）")

    key = Ed25519PrivateKey.generate()
    private_resolved.parent.mkdir(parents=True, exist_ok=True)
    private_resolved.write_bytes(
        key.private_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PrivateFormat.PKCS8,
            encryption_algorithm=serialization.NoEncryption(),
        )
    )
    try:
        os.chmod(private_resolved, 0o600)
    except OSError:
        # Windows 上 chmod 语义有限；真正的保护靠 ACL/文件位置。
        pass

    pub = public_bytes(key)
    public_path.parent.mkdir(parents=True, exist_ok=True)
    public_path.write_bytes(pub)
    return private_resolved, pub


def selftest() -> int:
    fails = 0
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        priv, pub = write_keypair(root / "key.pem", root / "pub.bin", force=True)
        print(f"  [OK  ] 生成私钥 {priv.name}（{priv.stat().st_size} 字节）")
        print(f"  [OK  ] 生成公钥 {len(pub)} 字节")
        msg = b'{"id":"hello","entry":"bin/hello"}'
        # 用生成的私钥签、用公钥验
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

        key = serialization.load_pem_private_key(priv.read_bytes(), password=None)
        signature = key.sign(msg)
        Ed25519PublicKey.from_public_bytes(pub).verify(signature, msg)
        print("  [OK  ] 签/验闭环")
        try:
            Ed25519PublicKey.from_public_bytes(pub).verify(signature, msg + b"x")
            print("  [FAIL] 篡改消息应当验签失败")
            fails += 1
        except Exception:
            print("  [OK  ] 篡改消息被拒绝")

        # 负向：私钥写进仓库必须被拒绝
        try:
            write_keypair(REPO / "should-not-exist.pem", root / "pub2.bin", force=True)
            print("  [FAIL] 仓库内私钥应当被拒绝")
            fails += 1
        except SystemExit:
            print("  [OK  ] 仓库内私钥被拒绝")
    print(f"\n自检结果：{'全部通过' if fails == 0 else f'{fails} 项失败'}")
    return 1 if fails else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="生成 AetherOS 应用包签名密钥（Ed25519；测试密钥，非生产）"
    )
    ap.add_argument("--private-key", default=str(DEFAULT_PRIVATE), help="私钥输出路径（必须在仓库外）")
    ap.add_argument("--public-key", default=str(DEFAULT_PUBLIC), help="公钥输出路径（默认入库）")
    ap.add_argument("--force", action="store_true", help="允许覆盖已有私钥（危险）")
    ap.add_argument("--selftest", action="store_true", help="只跑自检，不写仓库")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()

    private_path, pub = write_keypair(
        Path(args.private_key), Path(args.public_key), force=args.force
    )
    print(f"私钥：{private_path}（0600，务必离线备份；不要提交）")
    print(f"公钥：{Path(args.public_key)}（{len(pub)} 字节）")
    print(f"公钥 SHA-256 指纹：{hashlib.sha256(pub).hexdigest()}")
    print("注意：这是开发/自测密钥；生产发布请使用 owner 离线保管的私钥。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
