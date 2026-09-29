#!/usr/bin/env python3
"""mkfont.py —— 生成 AetherOS 桌面 UI 字体（Noto Sans CJK SC 子集，含**真粗体**）。

## 为什么需要它

桌面 UI 此前用 `wqy-microhei.ttc`，两个硬伤：
1. **它没有粗体** —— `text.rs` 的 bold 槽位只能复用正文字体，标题"加粗"实际没加粗；
2. 它偏点阵屏显风格，在窗口标题/设置面板这类大字号场景显得廉价。

换成 Noto Sans CJK SC 后中英文同族、有真粗体，但完整字体每个字重约 20 MB
（TTC，含 JP/KR/SC/TC 四面），两个字重就 40 MB —— 对 33 MB 的 ISO 不可接受。

所以这里做**子集**：GB2312 全量汉字 + ASCII + 常用标点/符号，两个字重合计约 6 MB。

## 为什么要放进构建期而不是仓库

子集产物是二进制（约 6 MB），入库会让仓库变重且无法审查。所以：**构建机现成生成**
（`fontTools` 是构建机工具，不进镜像），产物写进 rootfs overlay。生成失败不阻塞构建 ——
`text.rs` 的字体候选表里 wqy 仍在，会自动回退（代价是回到伪粗体）。

## 用法

    python3 scripts/mkfont.py --out /path/to/overlay/usr/share/fonts/truetype/aether
    python3 scripts/mkfont.py --charset gb2312-l1 --out ...   # 只要一级字库（更小）

退出码：0 成功；3 缺少依赖/源字体（调用方据此回退 wqy）。
"""

from __future__ import annotations

import argparse
import os
import sys

#: Noto CJK 在 Ubuntu 的默认位置（fonts-noto-cjk / fonts-noto-cjk-extra 包）。
DEFAULT_SRC_DIR = "/usr/share/fonts/opentype/noto"

#: 需要的两个字重：文件名里的字重名 → 产物字重名。
WEIGHTS = [("Regular", "Regular"), ("Bold", "Bold")]

#: 产物前缀（text.rs 的候选表按这个名字找）。
PREFIX = "NotoSansSC"

#: 除汉字外要带上的符号：CJK 标点、箭头、勾叉、几何、常用数学/单位。
EXTRA_SYMBOLS = "、。「」『』〈〉《》【】〔〕—…·※←→↑↓✓✗●○■□▲▼★☆±×÷≈≠≤≥°§¶†‡‰′″℃℉№"


def build_charset(kind: str) -> str:
    """字符集：GB2312 + ASCII + 常用符号。

    `gb2312`：一二级汉字全量（6763 字 + 符号区）；
    `gb2312-l1`：只取一级汉字（约 3755 字，产物更小，罕见字会缺）。
    """
    chars: set[str] = set()
    for b1 in range(0xA1, 0xF8):
        for b2 in range(0xA1, 0xFF):
            try:
                ch = bytes([b1, b2]).decode("gb2312")
            except UnicodeDecodeError:
                continue
            if kind == "gb2312-l1" and not ("\u4e00" <= ch <= "\u9fff" and b1 < 0xB0):
                # 一级汉字在 GB2312 里是 0xB0A1–0xD7F9；其余（二级/符号）跳过
                if not ("\u4e00" <= ch <= "\u9fff"):
                    chars.add(ch)
                continue
            chars.add(ch)
    chars.update(chr(c) for c in range(0x20, 0x7F))
    chars.update(EXTRA_SYMBOLS)
    return "".join(sorted(chars))


def find_sc_face(ttc_path: str) -> int:
    """在 Noto CJK 的 TTC 里找简体中文那一面。

    不写死索引：不同版本里 JP/KR/SC/TC 的顺序变过（实测本仓库环境是 2），
    按 name 表里的 "SC"/"Simplified" 找更稳；找不到才退回 2。
    """
    from fontTools.ttLib import TTCollection

    collection = TTCollection(ttc_path, lazy=True)
    for i, face in enumerate(collection.fonts):
        blob = " ".join(
            rec.toUnicode() for rec in face["name"].names if rec.nameID in (1, 4, 6)
        )
        if "SC" in blob or "Simplified" in blob:
            return i
    return 2


def subset_one(ttc_path: str, face: int, text_file: str, out_path: str) -> None:
    """调 fontTools.subset 的命令行入口（走 API 而不是 PATH，避免 pyftsubset 不在 PATH）。"""
    from fontTools import subset

    argv = [
        ttc_path,
        f"--font-number={face}",
        f"--text-file={text_file}",
        f"--output-file={out_path}",
        "--layout-features=*",
        "--no-hinting",
        "--name-IDs=*",
        "--drop-tables+=DSIG",
    ]
    subset.main(argv)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="生成 AetherOS UI 字体子集（Noto Sans CJK SC）。")
    ap.add_argument("--out", required=True, help="输出目录（会被建出来）")
    ap.add_argument("--src-dir", default=DEFAULT_SRC_DIR, help=f"Noto CJK 目录（默认 {DEFAULT_SRC_DIR}）")
    ap.add_argument("--charset", default="gb2312", choices=["gb2312", "gb2312-l1"],
                    help="字符集范围（默认 gb2312 全量）")
    args = ap.parse_args(argv)

    try:
        import fontTools  # noqa: F401
    except ImportError:
        print("[mkfont] 缺 fontTools —— apt-get install python3-fonttools", file=sys.stderr)
        return 3

    srcs = []
    for weight, _ in WEIGHTS:
        p = os.path.join(args.src_dir, f"NotoSansCJK-{weight}.ttc")
        if not os.path.isfile(p):
            print(f"[mkfont] 缺源字体 {p} —— apt-get install fonts-noto-cjk fonts-noto-cjk-extra",
                  file=sys.stderr)
            return 3
        srcs.append((weight, p))

    os.makedirs(args.out, exist_ok=True)
    text = build_charset(args.charset)
    text_file = os.path.join(args.out, ".charset.txt")
    with open(text_file, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"[mkfont] 字符集 {len(text)} 字（{args.charset}）")

    total = 0
    for weight, src in srcs:
        face = find_sc_face(src)
        out = os.path.join(args.out, f"{PREFIX}-{weight}.otf")
        try:
            subset_one(src, face, text_file, out)
        except Exception as e:  # 子集化失败必须显式报出，让调用方决定是否回退
            print(f"[mkfont] 子集化失败 {weight}: {e}", file=sys.stderr)
            return 3
        size = os.path.getsize(out)
        total += size
        # 读回校验：确认产物可解析（不是半个文件），并打印覆盖情况
        from fontTools.ttLib import TTFont

        font = TTFont(out, lazy=True)
        outline = "CFF" if "CFF " in font else "glyf"
        print(f"[mkfont] {os.path.basename(out)}  {size // 1024} KB  "
              f"字形 {font['maxp'].numGlyphs}  可映射字符 {len(font.getBestCmap())}  轮廓 {outline}")

    os.remove(text_file)
    print(f"[mkfont] 完成：2 个字重合计 {total / 1048576:.2f} MB → {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
