#!/usr/bin/env python3
"""生成 AetherOS 更新包（P1 宿主侧）—— 与 `aetherd/src/update.rs` 的清单格式严格对齐。

## 包长什么样

```
<out>/<version>/
  update.json                 { "version": "0.1.2", "files": [ {path, sha256, bytes} ] }
  update.json.sig             Ed25519 签名（64 字节；生产打包必须）
  var/apps/hello/hello        包内相对路径 = 目标路径去掉开头的 '/'
```

`path` 是**绝对目标路径**，必须命中 `aetherd` 的白名单（`/var/apps/`、`/etc/aether/`、
`/var/lib/aether/`）。系统组件（`/usr/bin/*`）**刻意不在白名单里** —— 那类更新走
整机路径（新 ISO + 无人安装 + `--keep-var`），见 T3。

## 用法

    mkupdate.py --version 0.1.2 --from <暂存树> --out updates/
    mkupdate.py --selftest

`--from` 指向一棵**镜像目标路径**的树（例如 `stage/var/apps/foo/foo`），
打包时原样映射到 `/var/apps/foo/foo` —— 这样"目标路径"不用猜，看目录结构就是答案。
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
import tempfile
from pathlib import Path

# ── 与 Rust 侧一致的常量（改一处必须改两处，所以这里写死并加断言自检）────────
TARGET_PREFIXES = ("/var/apps/", "/etc/aether/", "/var/lib/aether/")
MANIFEST_NAME = "update.json"
MANIFEST_SIG_NAME = "update.json.sig"
MAX_FILES = 4096
VERSION_RE = re.compile(r"^\d+(\.\d+)*$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")


class PackError(Exception):
    """打包失败（路径不合规、源树有问题等）—— 一律**整包拒绝**，不做部分打包。"""


def valid_version(v: str) -> bool:
    return bool(VERSION_RE.match(v))


def path_is_allowed(path: str) -> bool:
    """镜像 `aetherd::update::path_is_allowed` 的四条判据（绝对、无 `..`、无控制字符、命中白名单）。"""
    if not path.startswith("/") or ".." in path:
        return False
    if any(c.isprintable() is False and c not in " " for c in path):  # 控制字符（含 \n）
        return False
    if "//" in path:
        return False
    return path.startswith(TARGET_PREFIXES) and path not in TARGET_PREFIXES


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def collect(stage: Path) -> list[tuple[str, Path]]:
    """遍历暂存树，返回 [(绝对目标路径, 源文件)]。符号链接一律拒绝（与 Rust 侧一致）。"""
    if not stage.is_dir():
        raise PackError(f"--from 不是一个目录：{stage}")
    out: list[tuple[str, Path]] = []
    for p in sorted(stage.rglob("*")):
        rel = p.relative_to(stage).as_posix()
        if p.is_symlink():
            raise PackError(f"{rel}：暂存树里有符号链接，拒绝（更新包只放普通文件）")
        if p.is_dir():
            continue
        if not p.is_file():
            raise PackError(f"{rel}：不是普通文件")
        target = "/" + rel
        if not path_is_allowed(target):
            raise PackError(
                f"{rel}：目标路径 {target} 不在允许范围内（允许前缀：{'、'.join(TARGET_PREFIXES)}）\n"
                "       系统组件（如 /usr/bin/*）刻意不走文件级更新 —— 那类更新请走整机路径（--keep-var）"
            )
        out.append((target, p))
    if not out:
        raise PackError("暂存树里没有任何文件")
    if len(out) > MAX_FILES:
        raise PackError(f"文件数 {len(out)} 超过上限 {MAX_FILES}")
    return out


def _load_private_key(path: Path):
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    except ImportError as exc:  # pragma: no cover - 只在缺依赖时走到
        raise PackError(
            "签名需要 cryptography（python -m pip install -r scripts/requirements.txt）"
        ) from exc
    key_path = path.expanduser()
    if not key_path.is_file():
        raise PackError(f"签名私钥不存在：{key_path}")
    try:
        key = serialization.load_pem_private_key(key_path.read_bytes(), password=None)
    except (ValueError, TypeError) as exc:
        raise PackError(f"签名私钥读不出来（需要 Ed25519 PKCS#8 PEM）：{exc}") from exc
    if not isinstance(key, Ed25519PrivateKey):
        raise PackError("签名私钥必须是 Ed25519 密钥")
    return key


def public_key_of(private_key) -> bytes:
    from cryptography.hazmat.primitives import serialization

    return private_key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )


def sign_manifest(pkg: Path, key_path: Path) -> bytes:
    """对 `update.json` 的**原始字节**签名，写 `update.json.sig`（64 字节）。"""
    key = _load_private_key(key_path)
    data = (pkg / MANIFEST_NAME).read_bytes()
    signature = key.sign(data)
    (pkg / MANIFEST_SIG_NAME).write_bytes(signature)
    return signature


def verify_manifest_signature(pkg: Path, public_key: bytes) -> bool:
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    try:
        signature = (pkg / MANIFEST_SIG_NAME).read_bytes()
        data = (pkg / MANIFEST_NAME).read_bytes()
        Ed25519PublicKey.from_public_bytes(public_key).verify(signature, data)
        return True
    except (InvalidSignature, OSError, ValueError):
        return False


def pack(version: str, stage: Path, out: Path, signing_key: Path | None = None) -> Path:
    if not valid_version(version):
        raise PackError(f"版本号不合法：{version!r}（只允许数字与点，如 1.2.3）")
    files = collect(stage)
    pkg = out / version
    if pkg.exists():
        raise PackError(f"目标包目录已存在：{pkg}（先删掉或换个版本号，避免半新半旧）")
    entries = []
    for target, src in files:
        rel = target.lstrip("/")
        dst = pkg / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)
        entries.append({"path": target, "sha256": sha256_file(dst), "bytes": dst.stat().st_size})
    manifest = {"version": version, "files": entries}
    (pkg / MANIFEST_NAME).write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=False) + "\n", encoding="utf-8"
    )
    if signing_key is not None:
        sign_manifest(pkg, signing_key)
    return pkg


def verify_package(
    pkg: Path,
    require_signature: bool = False,
    public_key: bytes | None = None,
) -> list[str]:
    """打包后的**独立复检**：读回清单、逐条核对哈希与大小（不信任打包过程）。"""
    problems: list[str] = []
    m = json.loads((pkg / MANIFEST_NAME).read_text(encoding="utf-8"))
    if not valid_version(m.get("version", "")):
        problems.append(f"版本号不合法：{m.get('version')!r}")
    seen = set()
    for f in m.get("files", []):
        path, sha, size = f.get("path"), f.get("sha256"), f.get("bytes")
        if not path_is_allowed(path or ""):
            problems.append(f"{path}：目标路径不在允许范围")
            continue
        if not HEX64_RE.match(sha or ""):
            problems.append(f"{path}：sha256 不是 64 位十六进制小写")
        if path in seen:
            problems.append(f"{path}：清单里出现两次")
        seen.add(path)
        src = pkg / (path or "").lstrip("/")
        if not src.is_file():
            problems.append(f"{path}：包内找不到文件")
            continue
        if sha256_file(src) != sha:
            problems.append(f"{path}：哈希不符（清单 {sha} / 实际 {sha256_file(src)}）")
        if src.stat().st_size != size:
            problems.append(f"{path}：大小不符（清单 {size} / 实际 {src.stat().st_size}）")
    if require_signature:
        sig = pkg / MANIFEST_SIG_NAME
        if not sig.is_file():
            problems.append(f"缺少签名文件：{MANIFEST_SIG_NAME}")
        elif sig.stat().st_size != 64:
            problems.append(f"签名长度不对：{sig.stat().st_size} 字节（期望 64）")
        elif public_key is None:
            problems.append("缺少验签公钥，无法确认签名")
        elif not verify_manifest_signature(pkg, public_key):
            problems.append("清单签名校验失败（签名与公钥不匹配）")
    return problems


def selftest() -> int:
    """纯逻辑自检：判据、路径白名单、集合打包、以及**三条负向对照**。"""
    fails: list[str] = []

    def check(cond: bool, what: str) -> None:
        print(("  [OK  ] " if cond else "  [FAIL] ") + what)
        if not cond:
            fails.append(what)

    # 版本号
    check(valid_version("0.1.2") and valid_version("1") and valid_version("1.2.3.4"), "合法版本号接受")
    check(not valid_version("v1.2") and not valid_version("1.2-beta") and not valid_version(""), "非法版本号拒绝")

    # 白名单（与 Rust 侧逐条对齐）
    check(path_is_allowed("/var/apps/hello/hello"), "白名单内路径接受（/var/apps/）")
    check(path_is_allowed("/etc/aether/services/foo.toml"), "白名单内路径接受（/etc/aether/）")
    check(not path_is_allowed("/usr/bin/aetherd"), "系统二进制被拒绝（/usr/bin/ 不在白名单）")
    check(not path_is_allowed("/var/apps/../shadow"), "含 .. 被拒绝（前缀能骗过，规范化骗不过）")
    check(not path_is_allowed("/var/apps/a\nb"), "控制字符（换行注入）被拒绝")
    check(not path_is_allowed("/var/apps/"), "前缀本身不能当目标（不能覆盖目录）")
    check(not path_is_allowed("var/apps/x"), "相对路径被拒绝")

    # 端到端：真打一个包，再独立复检
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        stage = root / "stage" / "var" / "apps" / "hello"
        stage.mkdir(parents=True)
        (stage / "hello").write_text("#!/bin/sh\necho hi\n", encoding="utf-8")
        (root / "stage" / "etc" / "aether").mkdir(parents=True)
        (root / "stage" / "etc" / "aether" / "conf.toml").write_text("k = 1\n", encoding="utf-8")
        pkg = pack("0.1.2", root / "stage", root / "updates")
        m = json.loads((pkg / MANIFEST_NAME).read_text(encoding="utf-8"))
        check(m["version"] == "0.1.2", "清单版本正确")
        check(len(m["files"]) == 2, "清单文件数正确（两个）")
        check(all({"path", "sha256", "bytes"} <= set(f) for f in m["files"]), "每条含 path/sha256/bytes")
        check((pkg / "var/apps/hello/hello").is_file(), "包内布局 = 目标路径去掉开头的 /")
        check(verify_package(pkg) == [], "独立复检通过（哈希与大小逐条核对）")

        # 负向 ①：把一个字节改坏 ⇒ 复检必须报出来
        target = pkg / "var/apps/hello/hello"
        target.write_text("#!/bin/sh\necho HI\n", encoding="utf-8")
        probs = verify_package(pkg)
        check(any("哈希不符" in p for p in probs), "负向①：内容被改坏 ⇒ 复检报哈希不符")

        # 负向 ②：暂存树里放系统二进制 ⇒ 打包必须整包拒绝
        bad = root / "bad" / "usr" / "bin"
        bad.mkdir(parents=True)
        (bad / "aetherd").write_text("x", encoding="utf-8")
        try:
            pack("0.1.3", root / "bad", root / "updates")
            check(False, "负向②：系统二进制应当被拒绝，却没有")
        except PackError as e:
            check("/usr/bin/" in str(e) or "不在允许范围" in str(e), "负向②：系统二进制被拒绝（整包）")

        # 负向 ③：同一个版本号打两次 ⇒ 拒绝（避免半新半旧的包）
        try:
            pack("0.1.2", root / "stage", root / "updates")
            check(False, "负向③：重复版本号应当被拒绝，却没有")
        except PackError:
            check(True, "负向③：目标包目录已存在 ⇒ 拒绝覆盖")

        # 签名闭环（有 cryptography 才跑；CI 的快速门禁不要求装它）
        try:
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        except ImportError:
            print("  [SKIP] 未安装 cryptography，跳过签名自检")
        else:
            key = Ed25519PrivateKey.generate()
            key_path = root / "test-signing-key.pem"
            key_path.write_bytes(key.private_bytes(
                encoding=serialization.Encoding.PEM,
                format=serialization.PrivateFormat.PKCS8,
                encryption_algorithm=serialization.NoEncryption(),
            ))
            pub = public_key_of(key)
            signed = pack("0.1.4", root / "stage", root / "updates", signing_key=key_path)
            check((signed / MANIFEST_SIG_NAME).stat().st_size == 64, "签名文件为 64 字节")
            check(verify_package(signed, require_signature=True, public_key=pub) == [],
                  "签名包独立复检通过")
            # 负向：改清单一个字节 ⇒ 验签必须失败
            manifest = signed / MANIFEST_NAME
            manifest.write_bytes(manifest.read_bytes().replace(b"0.1.4", b"0.1.5", 1))
            probs = verify_package(signed, require_signature=True, public_key=pub)
            check(any("签名校验失败" in p for p in probs), "负向④：清单被篡改 ⇒ 验签失败")

    print(f"\n自检结果：{'全部通过' if not fails else f'{len(fails)} 项失败'}")
    return 1 if fails else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="生成 AetherOS 更新包（P1 宿主侧）")
    ap.add_argument("--version", help="版本号，形如 0.1.2")
    ap.add_argument("--from", dest="stage", help="暂存树（目录结构 = 目标路径）")
    ap.add_argument("--out", default="updates", help="输出目录（默认 updates/）")
    ap.add_argument("--signing-key", help="Ed25519 私钥 PEM（生产打包必须提供）")
    ap.add_argument(
        "--allow-unsigned-dev",
        action="store_true",
        help="仅本地开发：允许生成无签名包（生产发布禁止；客户端默认会拒绝）",
    )
    ap.add_argument("--selftest", action="store_true", help="跑纯逻辑自检，不打包")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()
    if not args.version or not args.stage:
        ap.error("需要 --version 与 --from（或 --selftest）")
    if not args.signing_key and not args.allow_unsigned_dev:
        ap.error("生产打包必须提供 --signing-key（本地开发可用 --allow-unsigned-dev）")
    try:
        pkg = pack(
            args.version,
            Path(args.stage),
            Path(args.out),
            signing_key=Path(args.signing_key) if args.signing_key else None,
        )
    except PackError as e:
        print(f"mkupdate: 打包失败（整包拒绝）：{e}", file=sys.stderr)
        return 2
    if args.signing_key:
        pub = public_key_of(_load_private_key(Path(args.signing_key)))
        problems = verify_package(pkg, require_signature=True, public_key=pub)
    else:
        problems = verify_package(pkg)
    if problems:
        print("mkupdate: 复检不通过：", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 3
    count = len(json.loads((pkg / MANIFEST_NAME).read_text(encoding="utf-8"))["files"])
    signed = "已签名" if args.signing_key else "未签名（开发模式）"
    print(f"mkupdate: 已生成 {pkg}（{count} 个文件，{signed}，复检通过）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
