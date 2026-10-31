#!/usr/bin/env python3
"""检查线上站点：可用性、安全响应头、公开事实与 site/data.json 是否一致。

为什么单独做一个脚本：`gen-site.py --check` 只能证明“仓库里的静态文件”自洽，
不能证明**生产 VPS 已经部署了这份文件**。2026-10-31 实测线上首页仍是
51,138 行 / 71 文件 / 768 测试，而仓库已是 52,544 / 77 / 793 —— 这类漂移
必须在发布流程里被自动判红，不能靠人眼。

用法：
  python scripts/site-health-check.py
  python scripts/site-health-check.py --base http://127.0.0.1:8000
  python scripts/site-health-check.py --json

默认检查 https://aether.cbnac.com；本地预览时用 --base 指向本地 HTTP 服务。
退出码：0 = 全部通过；1 = 有失败项；2 = 参数/数据文件错误。
"""

from __future__ import annotations

import argparse
import html
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "site" / "data.json"

PAGES = {
    "/": ("rust_lines", "rust_files", "tests", "iso_size", "version", "iso_date"),
    "/faq.html": ("rust_lines", "rust_files", "iso_size"),
    "/download.html": ("iso_bytes", "iso_sha256", "iso_date", "iso_size"),
    "/en/": ("rust_lines", "rust_files", "tests", "iso_size", "version", "iso_date"),
    "/en/faq.html": ("rust_lines", "rust_files", "iso_size"),
    "/en/download.html": ("iso_bytes", "iso_sha256", "iso_date", "iso_size"),
}

STATUS_PATHS = ("/", "/download.html", "/changelog.html", "/faq.html", "/en/",
                "/en/download.html", "/robots.txt", "/sitemap.xml")

SECURITY_HEADERS = (
    "Content-Security-Policy",
    "Strict-Transport-Security",
    "X-Content-Type-Options",
    "Referrer-Policy",
    "Permissions-Policy",
)


def fetch(url: str, timeout: float) -> tuple[int, dict[str, str], str]:
    req = urllib.request.Request(url, headers={"User-Agent": "AetherOS-site-health/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        body = resp.read().decode("utf-8", errors="replace")
        return resp.status, {k.lower(): v for k, v in resp.headers.items()}, body


def main() -> int:
    ap = argparse.ArgumentParser(description="AetherOS 线上站点事实/安全头检查")
    ap.add_argument("--base", default="https://aether.cbnac.com",
                    help="站点根 URL（默认线上；本地测试用 http://127.0.0.1:8000）")
    ap.add_argument("--timeout", type=float, default=20.0)
    ap.add_argument("--json", action="store_true", help="输出机器可读 JSON")
    ap.add_argument("--skip-security", action="store_true",
                    help="跳过安全响应头（本地静态服务器没有 nginx 安全头时用）")
    args = ap.parse_args()

    if not DATA.is_file():
        print(f"[FAIL] 找不到 {DATA}")
        return 2
    data = json.loads(DATA.read_text(encoding="utf-8"))
    base = args.base.rstrip("/")
    results: list[dict] = []

    def add(name: str, ok: bool, detail: str = "") -> None:
        results.append({"check": name, "ok": ok, "detail": detail})

    # 1) 所有公开入口必须 200
    for path in STATUS_PATHS:
        try:
            status, _, _ = fetch(base + path, args.timeout)
            add(f"HTTP {path}", status == 200, f"status={status}")
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            add(f"HTTP {path}", False, f"{type(exc).__name__}: {exc}")

    # 2) 给出事实的页面必须包含当前值
    for path, keys in PAGES.items():
        try:
            _, _, body = fetch(base + path, args.timeout)
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            add(f"事实 {path}", False, f"取页面失败: {exc}")
            continue
        text = html.unescape(body)
        for key in keys:
            want = str(data.get(key, ""))
            if not want:
                add(f"事实 {path} {key}", False, "site/data.json 缺字段")
            else:
                add(f"事实 {path} {key}", want in text, f"期望包含 {want!r}")

    # 3) 首页安全响应头
    if args.skip_security:
        add("安全头", True, "已按 --skip-security 跳过")
    else:
        try:
            _, headers, _ = fetch(base + "/", args.timeout)
            for name in SECURITY_HEADERS:
                add(f"安全头 {name}", bool(headers.get(name.lower())),
                    headers.get(name.lower(), "缺失"))
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            add("安全头", False, f"取响应失败: {exc}")

    failed = [r for r in results if not r["ok"]]
    if args.json:
        print(json.dumps({"base": base, "results": results, "failed": len(failed)},
                         ensure_ascii=False, indent=2))
    else:
        for r in results:
            mark = "PASS" if r["ok"] else "FAIL"
            print(f"[{mark}] {r['check']}" + (f"  ({r['detail']})" if r["detail"] else ""))
        print(f"\n{len(results) - len(failed)}/{len(results)} 项通过；失败 {len(failed)} 项")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
