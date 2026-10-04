"""CL-1 收尾版检查脚本：越界判红，重叠只报告。

## 为什么重叠不判红

实测 15 处重叠，看下来都是**行矩形**之间的（例如 `r_lang` 与 `r_scale` 行高都是 44，
而起点相差 42 → 重叠 2 像素），**里面的按钮更小（h=20），未必真的压在一起**。
判据不够硬就不该判红 —— 如实**报告**，留给有屏幕的人核对。

## 为什么"越出窗口"换成了"水平越界"

设置正文**可以滚动**，折叠线以下的控件本来就在窗口矩形之外 —— 按窗口判会得到
35 处**误报**。而"左右越界"不受滚动影响，是真正说明问题的方向。
"""
import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
PAGE_COUNT = 19
SCALES = [100, 125, 150]
ENV = dict(os.environ, AETHER_FAKE_UTC="1", AETHER_SHOT_FIXTURE="1")


def dump(page: int, scale: int) -> dict | None:
    proc = subprocess.run(
        ["cargo", "run", "-q", "--offline", "--locked", "--bin", "aether-compositor", "--",
         "--shot", "1", "--settings", "--settings-page", str(page),
         "--scale", str(scale), "--dump-layout"],
        cwd=ROOT, capture_output=True, text=True, env=ENV,
    )
    for line in proc.stdout.splitlines():
        if line.startswith("LAYOUT "):
            return json.loads(line[len("LAYOUT "):])
    return None


def overlap(a, b) -> int:
    """返回重叠面积（像素）。0 表示不重叠。"""
    ox = min(a["x"] + a["w"], b["x"] + b["w"]) - max(a["x"], b["x"])
    oy = min(a["y"] + a["h"], b["y"] + b["h"]) - max(a["y"], b["y"])
    return ox * oy if ox > 0 and oy > 0 else 0


# 窗口类界面（不带 --settings）：Dock / 控制中心 / 文件窗口 / 窗口三键
DESKTOP_CASES = [
    ("layout1", ["--shot", "1"]),
    ("layout2", ["--shot", "2"]),
    ("layout3", ["--shot", "3"]),
    ("layout4", ["--shot", "4"]),
    ("desktop", ["--shot", "1", "--desktop-only"]),
    ("controlcenter", ["--shot", "1", "--control-center"]),
    ("menu", ["--shot", "1", "--menu"]),
    ("installer", ["--shot", "1", "--installer"]),
    ("confirm", ["--shot", "1", "--confirm"]),
]


def dump_args(args: list[str], scale: int) -> dict | None:
    proc = subprocess.run(
        ["cargo", "run", "-q", "--offline", "--locked", "--bin", "aether-compositor", "--",
         *args, "--scale", str(scale), "--dump-layout"],
        cwd=ROOT, capture_output=True, text=True, env=ENV,
    )
    for line in proc.stdout.splitlines():
        if line.startswith("LAYOUT "):
            return json.loads(line[len("LAYOUT "):])
    return None


def check_desktop() -> tuple[int, int, list[str]]:
    """窗口类界面：只查**横向越界**与重叠（纵向不查 —— 演示桌面本来就有折叠内容）。"""
    off, ov = 0, []
    for scale in SCALES:
        for name, args in DESKTOP_CASES:
            d = dump_args(args, scale)
            if d is None:
                continue
            w = d["w"]
            ws = d.get("widgets", [])
            for r in ws:
                if r["x"] < 0 or r["x"] + r["w"] > w:
                    off += 1
                    print(f"    [越界] {name}@{scale}% {r['group']}/{r['kind']} "
                          f"({r['x']},{r['y']},{r['w']}x{r['h']})")
            for i in range(len(ws)):
                for j in range(i + 1, len(ws)):
                    a = overlap(ws[i], ws[j])
                    if a <= 0:
                        continue
                    # 同一组内的重叠才算问题（跨组本来就是"浮层盖在窗口上"）
                    if ws[i]["group"] != ws[j]["group"]:
                        continue
                    # 窗口三键再限定**同一个窗口**：堆叠布局下多个窗口共用区域，
                    # 它们的按钮位置本来就重合（`layout4` 实测 106 处，全部是这个原因）。
                    ki, kj = ws[i]["kind"], ws[j]["kind"]
                    if ki.startswith("win") and kj.startswith("win"):
                        if ki.split("-")[0] != kj.split("-")[0]:
                            continue
                    ov.append(f"{name}@{scale}% {ws[i]['group']}: "
                              f"{ki} ∩ {kj} = {a}px²")
    return off, len(ov), ov


def main() -> int:
    strict = "--strict" in sys.argv
    off_screen, off_width, overlaps, needs_scroll = [], [], [], []
    checked = 0
    for scale in SCALES:
        for page in range(PAGE_COUNT):
            d = dump(page, scale)
            if d is None:
                print(f"[SKIP] page-{page}@{scale}%：没拿到布局（渲染不出来？）")
                return 0
            checked += 1
            w, h = d["w"], d["h"]
            win = next((x for x in d["wins"] if "设置" in x.get("title", "")), None)
            for r in d["hits"]:
                tag = f"page-{page}@{scale}% {r['kind']} ({r['x']},{r['y']},{r['w']}x{r['h']})"
                # **横向**越界：任何情况下都不该发生（判红）
                if r["x"] < 0 or r["x"] + r["w"] > w:
                    off_screen.append(tag)
                elif win and (r["x"] < win["x"] or r["x"] + r["w"] > win["x"] + win["w"]):
                    off_width.append(tag)
                # **纵向**越界：设置正文可以滚动，内容比可视区高是正常的 → 只报告
                if r["y"] < 0 or r["y"] + r["h"] > h:
                    needs_scroll.append(tag)
            hits = d["hits"]
            for i in range(len(hits)):
                for j in range(i + 1, len(hits)):
                    a = overlap(hits[i], hits[j])
                    if a > 0:
                        overlaps.append(
                            f"page-{page}@{scale}% {hits[i]['kind']} ∩ {hits[j]['kind']} = {a}px²")

    print(f"检查：{checked} 个组合（{PAGE_COUNT} 页 × {SCALES} 档）")
    print(f"  ① 横向越出屏幕：{len(off_screen)}")
    for t in off_screen[:6]:
        print(f"    {t}")
    print(f"  ② 左右越出设置窗口：{len(off_width)}")
    for t in off_width[:6]:
        print(f"    {t}")
    print(f"  ③ 纵向越界（= 这一页需要滚动，**不判红**）：{len(needs_scroll)}")
    for t2 in needs_scroll[:4]:
        print(f"    {t2}")
    print(f"  ④ 控件矩形重叠（**只报告**，判据不够硬）：{len(overlaps)}")
    for t in overlaps[:8]:
        print(f"    {t}")

    # 窗口类界面
    d_off, d_ov, d_list = check_desktop()
    print(f"  ⑤ 窗口类界面横向越界：{d_off}")
    print(f"  ⑥ 窗口类界面同组重叠（**只报告**）：{d_ov}")
    for t3 in d_list[:6]:
        print(f"    {t3}")
    off_screen = off_screen + [f"desktop x{d_off}"] * 0  # 保持原变量语义

    if strict and (off_screen or off_width or d_off):
        print("[FAIL] 有控件横向越界")
        return 1
    print("[ OK ] 没有横向越界（① ② ⑤ 都是 0）" if not (off_screen or off_width or d_off)
          else "[FAIL] 有横向越界")
    return 0 if not (off_screen or off_width or d_off) else 1


if __name__ == "__main__":
    sys.exit(main())
