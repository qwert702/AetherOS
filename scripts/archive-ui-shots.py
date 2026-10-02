#!/usr/bin/env python3
"""重建 / 校验 `docs/host-ui-*.png` 归档走查图。

**为什么需要这个脚本**：归档图靠手敲命令重建，必然漏。这个坑已经踩过两次 ——
视觉层改动提交后归档图没跟上，于是"像素级零回归"的结论建立在错误的基线上
（见 `docs/ui-design-handover.md` §13.2）。

用法（先构建，脚本直接调二进制）：

    cargo build -p aether-compositor --offline
    python scripts/archive-ui-shots.py            # 重建全部归档图
    python scripts/archive-ui-shots.py --check    # 只对比，不写入（视觉回归门禁）

`--check` 的退出码：0 = 与归档一致；1 = 有差异（打印差异百分比与包围盒）。

约定：
- 顶栏时钟区（x > 1200 且 y < 32）与 AI 光标区视为非确定区，比对时屏蔽。
- 不经过 cargo 调用 —— 见 ui-design-handover §11：嵌套 shell 调 cargo 时
  PATH 会被改写成 Windows 分号形式，cargo 静默失败。
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import struct
import subprocess
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"

# (走查参数, 归档文件名)
SHOTS: list[tuple[list[str], str]] = [
    (["--shot", "2", "--theme", "dark"], "host-ui-desktop.png"),
    (["--shot", "3", "--theme", "dark"], "host-ui-threecol.png"),
    (["--shot", "2", "--confirm", "3", "--theme", "dark"], "host-ui-confirm.png"),
    (["--shot", "2", "--menu", "--theme", "dark"], "host-ui-menu.png"),
    (["--shot", "2", "--theme", "light"], "host-ui-light-desktop.png"),
    (["--shot", "2", "--confirm", "3", "--theme", "light"], "host-ui-light-confirm.png"),
    (["--shot", "2", "--installer", "--theme", "light"], "host-ui-light-installer.png"),
    (["--shot", "2", "--menu", "--theme", "light"], "host-ui-light-menu.png"),
    # 中文输入法候选框（2.3）：两个主题各一张 —— 候选框是新增的浮层，
    # 深浅两套配色都要有基线，否则将来改色板时它会是唯一没被比对到的地方
    (["--shot", "2", "--ime", "--theme", "dark"], "host-ui-ime.png"),
    (["--shot", "2", "--ime", "--theme", "light"], "host-ui-light-ime.png"),
    # 设置中心（P3）：这是设置窗口**唯一的走查覆盖** —— 否则它只有单测、没有视觉验收
    (["--shot", "2", "--settings", "--theme", "dark"], "host-ui-settings.png"),
    (["--shot", "2", "--settings", "--theme", "light"], "host-ui-light-settings.png"),
    # 控制中心（P4）：顶栏状态簇弹出的面板
    (["--shot", "2", "--control-center", "--theme", "dark"], "host-ui-control-center.png"),
    (["--shot", "2", "--control-center", "--theme", "light"], "host-ui-light-control-center.png"),
    # 默认桌面（P4.1）：无窗口 + 桌面图标（文件管理/终端/系统设置）
    (["--shot", "2", "--desktop-only", "--theme", "dark"], "host-ui-desktop-clean.png"),
    (["--shot", "2", "--desktop-only", "--theme", "light"], "host-ui-light-desktop-clean.png"),
    # 设置中心各页（P4.2/P4.3）：此前只有单测、没有视觉基线。
    # 校验方式见提交信息：**必须裁剪到设置窗口的页面区** (815,350)-(1264,644) 比较，
    # 用整图哈希或猜的窗口位置都会得出错误结论。
    (["--shot", "2", "--settings", "--settings-page", "5", "--theme", "light"], "host-ui-light-settings-net.png"),
    (["--shot", "2", "--settings", "--settings-page", "6", "--theme", "light"], "host-ui-light-settings-apps.png"),
    (["--shot", "2", "--settings", "--settings-page", "7", "--theme", "light"], "host-ui-light-settings-privacy.png"),
]

#: 固定"当前时间"（对应 `text::now_utc_secs` 的 `AETHER_FAKE_UTC` 钩子）。
#: 顶栏与设置页都有实时时钟，不固定则逐像素回归每分钟都会失败。1780000000 ≈ 2026-05-28 18:26 (+08)。
FAKE_UTC = "1780000000"

# 非确定区（相对左上角，右边界用图像宽度推）
CLOCK_TOP = 32
CLOCK_LEFT_MARGIN = 80  # 距右边缘的宽度
# AI 指令条的输入光标：按亚秒时钟闪烁，与截图时刻相关（handover §5.3 同一处）
CARET = (515, 595, 530, 630)  # x0, y0, x1, y1


def masked(x: int, y: int, w: int) -> bool:
    """该像素是否属于非确定区（时钟 / AI 光标），比对时屏蔽。"""
    if y < CLOCK_TOP and x >= w - CLOCK_LEFT_MARGIN:
        return True
    x0, y0, x1, y1 = CARET
    return x0 <= x <= x1 and y0 <= y <= y1


def load_bmp2png():
    """按路径加载同目录的 bmp2png.py（scripts 不是包，不能直接 import）。"""
    spec = importlib.util.spec_from_file_location("bmp2png", ROOT / "scripts" / "bmp2png.py")
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


def read_png(path: Path) -> tuple[int, int, list[bytes]]:
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: 不是 PNG")
    pos, idat, w, h = 8, b"", 0, 0
    while pos < len(data):
        ln = struct.unpack(">I", data[pos : pos + 4])[0]
        tag = data[pos + 4 : pos + 8]
        payload = data[pos + 8 : pos + 8 + ln]
        if tag == b"IHDR":
            w, h = struct.unpack(">II", payload[:8])
        elif tag == b"IDAT":
            idat += payload
        pos += 12 + ln
    raw = zlib.decompress(idat)
    stride = w * 3 + 1
    rows: list[bytes] = []
    prev = bytearray(w * 3)
    for y in range(h):
        line = raw[y * stride : (y + 1) * stride]
        ft = line[0]
        cur = bytearray(line[1:])
        if ft == 1:
            for i in range(3, len(cur)):
                cur[i] = (cur[i] + cur[i - 3]) & 255
        elif ft == 2:
            for i in range(len(cur)):
                cur[i] = (cur[i] + prev[i]) & 255
        elif ft == 3:
            for i in range(len(cur)):
                cur[i] = (cur[i] + ((cur[i - 3] if i >= 3 else 0) + prev[i]) // 2) & 255
        elif ft == 4:
            def pa(a: int, b: int, c: int) -> int:
                p = a + b - c
                x, y2, z = abs(p - a), abs(p - b), abs(p - c)
                return a if x <= y2 and x <= z else (b if y2 <= z else c)

            for i in range(len(cur)):
                cur[i] = (
                    cur[i]
                    + pa(
                        cur[i - 3] if i >= 3 else 0,
                        prev[i],
                        prev[i - 3] if i >= 3 else 0,
                    )
                ) & 255
        rows.append(bytes(cur))
        prev = cur
    return w, h, rows


def compare(a_path: Path, b_path: Path) -> tuple[int, float, tuple[int, int, int, int] | None]:
    """返回 (差异像素数, 差异百分比, 包围盒)。屏蔽顶栏时钟区。"""
    wa, ha, a = read_png(a_path)
    wb, hb, b = read_png(b_path)
    if (wa, ha) != (wb, hb):
        raise SystemExit(f"尺寸不一致：{a_path} {wa}x{ha} vs {b_path} {wb}x{hb}")
    n = 0
    box = [wa, ha, -1, -1]
    masked_px = 0
    for y in range(ha):
        row_a = a[y]
        row_b = b[y]
        for x in range(wa):
            if masked(x, y, wa):
                masked_px += 1
                continue
            i = x * 3
            if row_a[i : i + 3] != row_b[i : i + 3]:
                n += 1
                box[0] = min(box[0], x)
                box[1] = min(box[1], y)
                box[2] = max(box[2], x)
                box[3] = max(box[3], y)
    total = ha * wa - masked_px
    pct = 100.0 * n / total
    return n, pct, (tuple(box) if n else None)  # type: ignore[return-value]


def run_shot(binary: Path, args: list[str], out_png: Path, bmp2png) -> None:
    bmp = ROOT / "preview.bmp"
    if bmp.exists():
        bmp.unlink()
    proc = subprocess.run(
        [str(binary), *args],
        cwd=str(ROOT),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        # 固定"当前时间"：顶栏与设置页都有实时时钟，不固定的话逐像素回归每分钟都会失败
        env={**os.environ, "AETHER_FAKE_UTC": FAKE_UTC},
    )
    if proc.returncode != 0 or not bmp.exists():
        raise SystemExit(
            f"渲染失败：{' '.join(args)}\n{proc.stderr.decode('utf-8', 'replace')}"
        )
    bmp2png.bmp2png(str(bmp), str(out_png))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="只对比不写入（视觉回归门禁）")
    ap.add_argument(
        "--tolerance",
        type=float,
        default=0.02,
        help="--check 允许的差异百分比上限（默认 0.02%%）",
    )
    opts = ap.parse_args()

    name = "aether-compositor.exe" if os.name == "nt" else "aether-compositor"
    binary = ROOT / "target" / "debug" / name
    if not binary.exists():
        raise SystemExit(
            f"找不到 {binary}\n请先构建：cargo build -p aether-compositor --offline"
        )
    bmp2png = load_bmp2png()

    tmp = ROOT / "target" / "uishot"
    tmp.mkdir(parents=True, exist_ok=True)

    print(f"{'归档图':<34} {'差异像素':>9} {'占比':>9}  结论")
    failed = False
    for args, fname in SHOTS:
        dest = DOCS / fname
        target = dest if not opts.check else tmp / f"_check_{fname}"
        run_shot(binary, args, target, bmp2png)
        if opts.check:
            if not dest.exists():
                print(f"{fname:<34} {'-':>9} {'-':>9}  归档图缺失")
                failed = True
                continue
            n, pct, box = compare(target, dest)
            ok = pct <= opts.tolerance
            failed = failed or not ok
            note = "一致" if n == 0 else (f"包围盒 {box}" if box else "一致")
            print(
                f"{fname:<34} {n:>9} {pct:>8.4f}%  {'✓ ' + note if ok else '✗ 不一致：' + note}"
            )
        else:
            print(f"{fname:<34} {'-':>9} {'-':>9}  已重建")
    if opts.check and failed:
        print("\n结论：归档图与当前 HEAD 的渲染不一致 —— 需要重建（去掉 --check）。")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
