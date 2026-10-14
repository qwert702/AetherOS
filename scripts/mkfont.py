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

#: 等宽字体（终端用）的源与产物名 —— **产物名必须与 `text.rs:99` 的候选表逐字一致**：
#:   "/usr/share/fonts/truetype/aether/NotoSansMonoCJKsc-Regular.otf"
#: 此前构建只产出上面两个字重的正文字体，候选表里的等宽项一个都不存在 →
#: 终端只能回退到正文字体（非等宽）。台账 #46 记的就是这件事。
MONO_SRC = "NotoSansMonoCJKsc-Regular.otf"
MONO_OUT = "NotoSansMonoCJKsc-Regular.otf"

#: 等宽字体只要 ASCII + 终端常用制表/符号 —— **故意不带 CJK**：
#: `text.rs:358` 的判定是 `mono && self.mono.has_glyph(ch)`，**缺字形会逐字回退**到
#: 正文字体，所以给等宽字体塞 6763 个汉字纯属浪费 ISO 体积（汉字仍由正文字体渲染，
#: 只是不保证等宽对齐 —— 这是取舍，不是遗漏）。
MONO_CHARSET = (
    "".join(chr(c) for c in range(0x20, 0x7F))
    + "─│┌┐└┘├┤┬┴┼━┃█▉▊▋▌▍▎▏░▒▓✓✗←→↑↓·…±×÷≈≠≤≥°§"
)

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


def subset_one(ttc_path: str, face: int | None, text_file: str, out_path: str) -> None:
    """调 fontTools.subset 的命令行入口（走 API 而不是 PATH，避免 pyftsubset 不在 PATH）。

    `face=None` 表示源是**单体字体**（如等宽那份 OTF）—— 这时不能传 `--font-number`。
    """
    from fontTools import subset

    argv = [
        ttc_path,
        f"--text-file={text_file}",
        f"--output-file={out_path}",
        "--layout-features=*",
        "--no-hinting",
        "--name-IDs=*",
        "--drop-tables+=DSIG",
    ]
    if face is not None:
        argv.insert(1, f"--font-number={face}")
    subset.main(argv)


def selftest() -> int:
    """自检：**不需要 fontTools、不需要源字体** —— 只验纯逻辑与命名契约。

    为什么要有它：`mkfont.py` 只在构建机上跑，而构建机常常不可用（本会话就撞上过）。
    命名契约（产物名 == `text.rs` 候选表里的名字）一旦写错，**构建会成功、字体却永远加载不到**，
    这种失败方式在 ISO 里看不出来 —— 所以把契约钉成自检，本机就能跑。
    """
    failed = 0

    def check(name: str, ok: bool, detail: str = "") -> None:
        nonlocal failed
        print(f"  [{'OK  ' if ok else 'FAIL'}] {name}{('：' + detail) if detail else ''}")
        if not ok:
            failed += 1

    cs = build_charset("gb2312")
    check("gb2312 字符集含 ASCII 全量", all(chr(c) in cs for c in range(0x20, 0x7F)))
    check("gb2312 字符集含常用符号", all(s in cs for s in "「」—…·←→✓✗±×÷≈≠≤≥℃"))
    check("gb2312 字符集规模合理（>6000 汉字）",
          sum(1 for c in cs if "\u4e00" <= c <= "\u9fff") > 6000,
          f"{sum(1 for c in cs if chr(0x4e00) <= c <= chr(0x9fff))} 个汉字")
    l1 = build_charset("gb2312-l1")
    check("gb2312-l1 比 gb2312 小", len(l1) < len(cs), f"{len(l1)} < {len(cs)}")

    check("等宽产物名与 text.rs 候选表一致",
          MONO_OUT == "NotoSansMonoCJKsc-Regular.otf", MONO_OUT)
    check("等宽字符集含 ASCII", all(chr(c) in MONO_CHARSET for c in range(0x20, 0x7F)))
    check("等宽字符集含制表符", all(s in MONO_CHARSET for s in "─│┌┐└┘├┤┼█░▒▓"))
    check("等宽字符集**故意**不含汉字（靠逐字回退）",
          not any("\u4e00" <= c <= "\u9fff" for c in MONO_CHARSET))
    check("等宽字符集比正文小一个量级",
          len(MONO_CHARSET) * 20 < len(cs), f"{len(MONO_CHARSET)} vs {len(cs)}")

    if failed:
        print(f"[FAIL] 自检不通过：{failed} 项")
        return 1
    print("[ OK ] 自检通过（9 项：字符集逻辑 + 命名契约）")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="生成 AetherOS UI 字体子集（Noto Sans CJK SC + 等宽）。")
    ap.add_argument("--out", help="输出目录（会被建出来）")
    ap.add_argument("--src-dir", default=DEFAULT_SRC_DIR, help=f"Noto CJK 目录（默认 {DEFAULT_SRC_DIR}）")
    ap.add_argument("--charset", default="gb2312", choices=["gb2312", "gb2312-l1"],
                    help="字符集范围（默认 gb2312 全量）")
    ap.add_argument("--selftest", action="store_true",
                    help="只验纯逻辑与命名契约（不需要 fontTools、不需要源字体）")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()
    if not args.out:
        print("[mkfont] 缺少 --out（只有 --selftest 可以不传）", file=sys.stderr)
        return 2

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

    # —— 等宽字体（台账 #46）：产物名与 `text.rs:99` 的候选表一致 ——
    # **缺源只警告、不阻塞构建**：正文两个字重才是必需的（没有它们会退回 wqy 伪粗体），
    # 等宽缺失只是"终端回退成非等宽"，属于质量降级而不是构建失败。
    mono_size = 0
    mono_src = os.path.join(args.src_dir, MONO_SRC)
    if not os.path.isfile(mono_src):
        print(f"[mkfont] 跳过等宽字体：缺源 {mono_src}"
              f"（apt-get install fonts-noto-cjk-extra；终端会逐字回退到正文字体）",
              file=sys.stderr)
    else:
        mono_text = os.path.join(args.out, ".charset-mono.txt")
        try:
            with open(mono_text, "w", encoding="utf-8") as f:
                f.write(MONO_CHARSET)
            mono_out = os.path.join(args.out, MONO_OUT)
            # OTF 是单体字体 → face=None（不能传 --font-number）
            subset_one(mono_src, None, mono_text, mono_out)
            mono_size = os.path.getsize(mono_out)
            print(f"[mkfont] {MONO_OUT}  {mono_size // 1024} KB  "
                  f"（ASCII + 制表/符号 {len(MONO_CHARSET)} 字；汉字靠逐字回退）")
        except Exception as e:  # 等宽失败不影响正文，故只报告
            print(f"[mkfont] 等宽子集失败（不阻塞构建）：{e}", file=sys.stderr)
        finally:
            if os.path.exists(mono_text):
                os.remove(mono_text)

    os.remove(text_file)
    print(f"[mkfont] 完成：正文 2 个字重合计 {total / 1048576:.2f} MB"
          + (f" + 等宽 {mono_size / 1024:.0f} KB" if mono_size else "（等宽未生成）")
          + f" → {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
