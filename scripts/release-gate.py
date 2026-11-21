#!/usr/bin/env python3
"""release-gate.py —— 发布门禁聚合（E4 文档/站点自动检查进发布流程）。

把既有发布前检查串成一条入口，串行执行并聚合结果。**只调用、不修改**被检查
脚本的语义：每个子脚本自己判红/黄，本脚本不吞掉它的失败。

## 聚合的检查（与既有脚本一一对应）

| key | 脚本 | 覆盖 |
|---|---|---|
| site | `gen-site.py --check` | 站点页面 / 发布事实 / 脚本哈希自洽 |
| doc-numbers | `doc-numbers-check.py` | 文档数字与来源一致 |
| links | `check-links.py` | 本地文档链接可解析 |
| repo-stats | `repo-stats.py --check` | 文档规模三层 + 集合相等 |
| site-health | `site-health-check.py` | 线上站点可用性 / 安全头 / 事实一致（仅联网模式） |
| release-readiness | `release-readiness.py` | ISO 字节 / SHA256 / isohybrid / release↔data 一致 |

## 退出码

- 0：全部 PASS 或 SKIP；
- 1：任一 FAIL（含子脚本非零退出、超时、命令不存在）。

## 用法

    python scripts/release-gate.py --offline          # 离线：跳过线上站点巡检
    python scripts/release-gate.py --only site,links  # 只跑指定子项
    python scripts/release-gate.py --json out.json    # 额外写机器可读结果
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PY = sys.executable

PASS, FAIL, SKIP = "PASS", "FAIL", "SKIP"

# 单个子项的兜底超时（秒）；站点巡检默认 20s/页，这里给足余量。
STEP_TIMEOUT = 600


def _steps() -> list[dict]:
    return [
        {
            "key": "site",
            "name": "站点自洽（gen-site --check）",
            "cmd": [PY, "scripts/gen-site.py", "--check"],
            "online_only": False,
        },
        {
            "key": "doc-numbers",
            "name": "文档数字一致性",
            "cmd": [PY, "scripts/doc-numbers-check.py"],
            "online_only": False,
        },
        {
            "key": "links",
            "name": "文档链接门禁",
            "cmd": [PY, "scripts/check-links.py"],
            "online_only": False,
        },
        {
            "key": "repo-stats",
            "name": "文档规模三层门禁",
            "cmd": [PY, "scripts/repo-stats.py", "--check"],
            "online_only": False,
        },
        {
            "key": "site-health",
            "name": "线上站点巡检",
            "cmd": [PY, "scripts/site-health-check.py"],
            "online_only": True,
        },
        {
            "key": "release-readiness",
            "name": "发布就绪（ISO / 哈希 / isohybrid）",
            "cmd": [PY, "scripts/release-readiness.py"],
            "online_only": False,
        },
    ]


def classify(code: int) -> str:
    if code == 0:
        return PASS
    if code == 77:
        return SKIP
    return FAIL


def tail(text: str, limit: int = 300) -> str:
    lines = [ln for ln in text.splitlines() if ln.strip()]
    snippet = "\n".join(lines[-2:])
    return snippet[-limit:].replace("\n", " ⏎ ")


def run_step(step: dict) -> dict:
    record: dict = {
        "key": step["key"],
        "name": step["name"],
        "cmd": " ".join(step["cmd"]),
        "exit_code": None,
        "status": "",
        "duration_s": 0.0,
        "output_tail": "",
        "skipped_reason": "",
    }
    t0 = time.time()
    try:
        r = subprocess.run(
            step["cmd"], cwd=ROOT, capture_output=True, text=True,
            encoding="utf-8", errors="replace", timeout=STEP_TIMEOUT,
        )
        record["exit_code"] = r.returncode
        record["status"] = classify(r.returncode)
        record["output_tail"] = tail((r.stdout or "") + (r.stderr or ""))
    except subprocess.TimeoutExpired:
        record["exit_code"] = 124
        record["status"] = FAIL
        record["output_tail"] = f"超时（>{STEP_TIMEOUT}s）"
    except FileNotFoundError as exc:
        record["exit_code"] = 127
        record["status"] = FAIL
        record["output_tail"] = f"命令不存在：{exc}"
    record["duration_s"] = round(time.time() - t0, 1)
    return record


def main() -> int:
    ap = argparse.ArgumentParser(description="发布门禁聚合")
    ap.add_argument("--offline", action="store_true",
                    help="离线模式：跳过需要访问线上站点的子项")
    ap.add_argument("--only", default="",
                    help="逗号分隔的子项 key，只跑指定项")
    ap.add_argument("--json", metavar="OUT.json", default="",
                    help="把机器可读结果写到该文件")
    args = ap.parse_args()

    only = {k.strip() for k in args.only.split(",") if k.strip()}
    steps = _steps()
    if only:
        unknown = only - {s["key"] for s in steps}
        if unknown:
            print(f"[FAIL] 未知子项 key：{sorted(unknown)}")
            return 1

    records: list[dict] = []
    for step in steps:
        if only and step["key"] not in only:
            continue
        if args.offline and step["online_only"]:
            records.append({
                "key": step["key"],
                "name": step["name"],
                "cmd": " ".join(step["cmd"]),
                "exit_code": None,
                "status": SKIP,
                "duration_s": 0.0,
                "output_tail": "",
                "skipped_reason": "--offline 跳过线上站点巡检",
            })
            continue
        records.append(run_step(step))

    # —— 结果表 ——
    headers = ["步骤", "命令", "退出码", "状态", "耗时", "关键输出"]
    rows = []
    for rec in records:
        rows.append([
            rec["name"],
            rec["cmd"],
            str(rec["exit_code"]) if rec["exit_code"] is not None else "—",
            rec["status"],
            f"{rec['duration_s']:.1f}s",
            rec["output_tail"] or rec["skipped_reason"],
        ])

    widths = [max(len(h), *(len(r[i]) for r in rows)) if rows else len(h)
              for i, h in enumerate(headers)]

    def fmt(row: list[str]) -> str:
        return " | ".join(c.ljust(widths[i]) for i, c in enumerate(row))

    print("发布门禁结果" + ("（离线）" if args.offline else "（联网）"))
    print(fmt(headers))
    print("-+-".join("-" * w for w in widths))
    for row in rows:
        print(fmt(row))

    counts = {PASS: 0, FAIL: 0, SKIP: 0}
    for rec in records:
        counts[rec["status"]] += 1
    print()
    print(f"合计：PASS {counts[PASS]} · FAIL {counts[FAIL]} · SKIP {counts[SKIP]}")

    if args.json:
        payload = {
            "offline": args.offline,
            "summary": {**counts, "exit_code": 1 if counts[FAIL] else 0},
            "steps": records,
        }
        Path(args.json).write_text(
            json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"[INFO] JSON 结果已写入：{args.json}")

    return 1 if counts[FAIL] else 0


if __name__ == "__main__":
    raise SystemExit(main())
