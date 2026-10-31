#!/usr/bin/env python3
"""发布前检查：ISO 字节/哈希/isohybrid、release.json 与 site/data.json 一致性。

它回答的是发布流程里最容易出错的三件事：
  1. 手里这个 ISO 是不是 `site/release.json` 公布的那个文件；
  2. 它是否能 dd 到磁盘引导（offset 510 必须是 0x55AA）；
  3. 站点数据与 release.json 是否还是同一份事实。

用法：
  python scripts/release-readiness.py --iso path/to/aetheros.iso
  python scripts/release-readiness.py            # 自动找根目录/ dist/ 下的 ISO
  python scripts/release-readiness.py --json

退出码：0 = 全部通过；1 = 有失败项；2 = 参数/文件错误。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RELEASE = ROOT / "site" / "release.json"
DATA = ROOT / "site" / "data.json"
ISOHYBRID_OFFSET = 510
ISOHYBRID_MAGIC = b"\x55\xaa"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def find_iso() -> Path | None:
    candidates = [ROOT / "aetheros-0.1-amd64.iso"]
    for d in (ROOT / "dist", ROOT / "dist" / "archive"):
        if d.is_dir():
            candidates.extend(sorted(d.glob("*.iso")))
    existing = [p for p in candidates if p.is_file()]
    if not existing:
        return None
    # 优先根目录的“当前产物”，否则选最新的文件
    return max(existing, key=lambda p: (p.parent == ROOT, p.stat().st_mtime))


def main() -> int:
    ap = argparse.ArgumentParser(description="AetherOS 发布就绪检查")
    ap.add_argument("--iso", type=Path, help="待发布 ISO；省略则自动查找")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    if not RELEASE.is_file():
        print(f"[FAIL] 找不到 {RELEASE}")
        return 2
    release = json.loads(RELEASE.read_text(encoding="utf-8-sig"))
    data = json.loads(DATA.read_text(encoding="utf-8-sig")) if DATA.is_file() else {}

    results: list[dict] = []

    def add(name: str, ok: bool, detail: str = "") -> None:
        results.append({"check": name, "ok": ok, "detail": detail})

    # release.json 与 site/data.json 的同一事实
    for key in ("iso_bytes", "iso_size", "iso_sha256", "iso_date", "tag"):
        rv = release.get(key)
        dv = data.get("version" if key == "tag" else key)
        add(f"release/site {key}", str(rv) == str(dv),
            f"release={rv!r} site={dv!r}")

    iso = args.iso or find_iso()
    if iso is None or not iso.is_file():
        add("ISO 文件", False, "找不到 ISO；用 --iso 指定或先构建")
        iso = None
    else:
        size = iso.stat().st_size
        digest = sha256(iso)
        add("ISO 存在", True, str(iso))
        add("ISO 大小", size == int(release.get("iso_bytes", -1)),
            f"{size} vs release {release.get('iso_bytes')}")
        add("ISO SHA256", digest == str(release.get("iso_sha256", "")),
            f"{digest} vs release {release.get('iso_sha256')}")
        with iso.open("rb") as f:
            f.seek(ISOHYBRID_OFFSET)
            magic = f.read(2)
        add("ISO isohybrid", magic == ISOHYBRID_MAGIC,
            f"offset {ISOHYBRID_OFFSET} = {magic.hex()}（需要 55aa）")

    failed = [r for r in results if not r["ok"]]
    if args.json:
        print(json.dumps({"iso": str(iso) if iso else None, "results": results,
                          "failed": len(failed)}, ensure_ascii=False, indent=2))
    else:
        for r in results:
            print(f"[{'PASS' if r['ok'] else 'FAIL'}] {r['check']}"
                  + (f"  ({r['detail']})" if r["detail"] else ""))
        print(f"\n{len(results) - len(failed)}/{len(results)} 项通过；失败 {len(failed)} 项")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
