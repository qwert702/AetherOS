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
PAGE_COUNT = 17  # 页 0..16（`settings.rs` 里 `title:` 共 17 条）。曾误写 19 → 后两页重复渲染，组合数虚高（附录 GA）
SCALES = [100, 125, 150]
# 中英**都跑三档**。上一版英文只跑 150%（省时间），但"100%/125% 英文无机器覆盖"这条
# 说明一直挂在附录里 —— 实测跑满三档只多约 80 秒，不值得留这个缺口（审计附录 DY）。
COMBOS = [(lang, s) for lang in ("zh", "en") for s in SCALES]
ENV = dict(os.environ, AETHER_FAKE_UTC="1", AETHER_SHOT_FIXTURE="1")


def band_overlaps(d: dict) -> list:
    """判据 ⑪：两段文字的**带区**在两个方向上都重叠超过 2px。

    为什么需要它：判据 ⑧ 从**像素**推文字带，两行完全叠在一起时会合并成一个带而看不见
    （附录 FL）。这里用渲染器自己登记的精确带区（附录 GE），不依赖任何启发式。
    """
    ts = d.get("texts") or []
    # **只在同一窗口内比**：按 clip 分组。窗口之间本来就会互相盖住，那不是问题。
    # **只在有明确窗口身份的组内比**：`set_clip` 目前只有设置窗口在调，
    # 其余窗口（文件/终端/音乐）的带区 clip 为 null —— 它们本来就会互相盖住，
    # 混在一起会产生大量假重叠（实测 303 段里 259 段是 null）。
    # 设置窗口这一组正是附录 FL 那个 bug 的所在，也是判据 ⑧ 的盲区所在。
    by: dict = {}
    for e in ts:
        if e.get("clip") is None:
            continue
        by.setdefault(str(e["clip"]), []).append(e)
    out = []
    for group in by.values():
        for i in range(len(group)):
            a = group[i]
            for j in range(i + 1, len(group)):
                b = group[j]
                ox = min(a["x"] + a["w"], b["x"] + b["w"]) - max(a["x"], b["x"])
                oy = min(a["y"] + a["h"], b["y"] + b["h"]) - max(a["y"], b["y"])
                if ox > 2 and oy > 2:
                    out.append((a["text"][:24], b["text"][:24], round(ox, 1), round(oy, 1)))
    return out


def dump(page: int, scale: int, lang: str = "zh") -> dict | None:
    """渲染一档并返回布局；**同时**把 `OVERFLOW` 行写进全局 OVERFLOW_SEEN。

    顺带做「文字超宽」判据 ⑨：不额外渲染，只是给同一次渲染加 `--report-overflow`。
    """
    extra = [] if lang == "zh" else ["--lang", lang]
    proc = subprocess.run(
        ["cargo", "run", "-q", "--offline", "--locked", "--bin", "aether-compositor", "--",
         "--shot", "1", "--settings", "--settings-page", str(page),
         "--scale", str(scale), *extra, "--dump-layout"],
        cwd=ROOT, capture_output=True, text=True, env=ENV,
    )
    for line in proc.stdout.splitlines():
        if line.startswith("LAYOUT "):
            return json.loads(line[len("LAYOUT "):])
    return None


def overflow_of(page: int, scale: int, lang: str) -> list:
    """单独跑一次拿「文字超宽」清单。

    **不能**和 `--dump-layout` 混用：实测两者同时给时 dump 拿不到 LAYOUT 行
    （于是走 SKIP 静默通过 —— 附录 FH 记了这个洞，本批已把它改成判红）。
    """
    extra = [] if lang == "zh" else ["--lang", lang]
    proc = subprocess.run(
        ["cargo", "run", "-q", "--offline", "--locked", "--bin", "aether-compositor", "--",
         "--shot", "1", "--settings", "--settings-page", str(page),
         "--scale", str(scale), *extra, "--report-overflow", "--report-cjk"],
        cwd=ROOT, capture_output=True, text=True, env=ENV,
    )
    out = []
    for line in proc.stdout.splitlines():
        if line.startswith("OVERFLOW "):
            out.append((page, scale, lang, line[len("OVERFLOW "):].strip()))
        elif line.startswith("CJK ") and lang == "en":
            # 只统计**英文模式**：中文模式下满屏中文，没有信号（附录 FQ）。
            CJK_SEEN.append((page, scale, lang, line[len("CJK "):].strip()))
    return out


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


def dump_args(args: list[str], scale: int, lang: str = "zh") -> dict | None:
    extra = [] if lang == "zh" else ["--lang", lang]
    proc = subprocess.run(
        ["cargo", "run", "-q", "--offline", "--locked", "--bin", "aether-compositor", "--",
         *args, "--scale", str(scale), *extra, "--dump-layout"],
        cwd=ROOT, capture_output=True, text=True, env=ENV,
    )
    for line in proc.stdout.splitlines():
        if line.startswith("LAYOUT "):
            return json.loads(line[len("LAYOUT "):])
    return None


def check_desktop() -> tuple[int, int, list[str]]:
    """窗口类界面：只查**横向越界**与重叠（纵向不查 —— 演示桌面本来就有折叠内容）。"""
    off, ov = 0, []
    for lang, scale in COMBOS:
        for name, args in DESKTOP_CASES:
            d = dump_args(args, scale, lang)
            colors = blank_of_shot()
            if d is None:
                continue
            if 0 <= colors <= BLANK_MAX_COLORS:
                blanks.append(f"{name}@{scale}%（内容色 {colors}）")
            w = d["w"]
            ws = d.get("widgets", [])
            for r in ws:
                if r["x"] < 0 or r["x"] + r["w"] > w:
                    off += 1
                    print(f"    [越界] {name}@{scale}% {r['group']}/{r['kind']} "
                          f"({r['x']},{r['y']},{r['w']}x{r['h']})")
            # 屏幕外的矩形不参与「重叠」判断：控制中心的裁剪把「完全在面板之外的行」
            # 挪到 y = -1000（附录 DD），多行共用同一个屏幕外坐标**不是**布局重叠。
            vis = [w for w in ws if w["y"] >= 0 and w["y"] + w["h"] <= d["h"]]
            for i in range(len(vis)):
                for j in range(i + 1, len(vis)):
                    a = overlap(vis[i], vis[j])
                    if a <= 0:
                        continue
                    # 同一组内的重叠才算问题（跨组本来就是「浮层盖在窗口上」）
                    if vis[i]["group"] != vis[j]["group"]:
                        continue
                    # 窗口三键再限定**同一个窗口**：堆叠布局下多个窗口共用区域，
                    # 它们的按钮位置本来就重合（`layout4` 实测 106 处，全部是这个原因）。
                    ki, kj = vis[i]["kind"], vis[j]["kind"]
                    if ki.startswith("win") and kj.startswith("win"):
                        if ki.split("-")[0] != kj.split("-")[0]:
                            continue
                    ov.append(f"{name}@{scale}% {vis[i]['group']}: "
                              f"{ki} ∩ {kj} = {a}px²")
    return off, len(ov), ov


# ---- 空白判定（与 shot-diff.py 同源，见审计附录 CK / DB）----
# 内容区（去掉顶栏与左侧导航栏）
CONTENT_X0 = 260
CONTENT_Y0 = 60
# 实测：非空白档的内容色最少 782（desktop），阈值取 24 极保守，只抓「整档没画出来」
BLANK_MAX_COLORS = 24
# 判据 ⑧：同一窗口内相邻**文字带**的最小间隙（px）。<=0 表示两行文字压在一起。
# 实测基线（外观页）：中文 150% 为 3px、英文 4px —— 取 1 留余量。
TEXT_MIN_GAP = 1
# 判据 ⑨：文字超宽（绘制调用超出可用宽度）的收集处，由 dump() 填充。
OVERFLOW_SEEN: list = []
# 判据 ⑪：两段文字带区重叠（精确；用渲染器登记的带区 —— 附录 GE / GF）。
BAND_OVERLAPS: list = []
# 判据 ⑩：**英文模式下画出来的中文**（漏译信号，来源不限 —— 附录 FQ）。
# 注意：里面含**夹具数据**（例如文件管理器里夹具建的「文档」「图片」目录名），
# 所以用棘轮而不是判 0；"上升"才是新漏译的信号。
CJK_SEEN: list = []
# 判据 ⑨ 的**棘轮**：当前实测值。超过就判红（防回归）；减少也允许（提示更新此数）。
# 59 条**全在 125%/150%**（100% 是 0）—— 尚未逐条判读是"取整的 1–2px"还是"真被裁"。
# 如实记：所以这里用棘轮，不假装它们已经解决（见审计附录 FH）。
OVERFLOW_MAX = 0
HDR = 54          # 24bpp BMP 头
W, H = 1280, 760


def content_colors(raw: bytes) -> int:
    """数内容区里**不同颜色**的个数。"""
    if len(raw) != HDR + W * H * 3:
        return -1
    px = raw[HDR:]
    seen = set()
    for y in range(CONTENT_Y0, H - 8):
        row = (H - 1 - y) * W
        for x in range(CONTENT_X0, W - 8):
            i = (row + x) * 3
            seen.add(px[i:i + 3])
            if len(seen) > 4000:
                return len(seen)
    return len(seen)


def blank_of_shot() -> int:
    """读渲染器刚写出的 preview.bmp，返回内容色个数（读不到返回 -1）。"""
    bmp = ROOT / "preview.bmp"
    if not bmp.exists():
        return -1
    return content_colors(bmp.read_bytes())


def text_min_gap(raw: bytes, win: dict) -> int:
    """设置窗口**文字区**内相邻文字带的最小间隙（px）；<=0 = 两行文字压在一起。

    * 背景色取文字区里**出现次数最多**的颜色（不假设深色/浅色主题）；
    * x 范围避开行右侧的开关列（否则每行都有开关墨迹，永远只有一条带）。
    """
    if len(raw) != HDR + W * H * 3 or not win:
        return -1
    px = raw[HDR:]
    x0 = win["x"] + int(win["w"] * 0.42)
    x1 = win["x"] + win["w"] - 120
    y0 = win["y"] + 60
    y1 = win["y"] + win["h"] - 6

    counts: dict = {}
    for y in range(y0, y1, 2):
        row = (H - 1 - y) * W
        for x in range(x0, x1, 2):
            i = (row + x) * 3
            c = px[i:i + 3]
            counts[c] = counts.get(c, 0) + 1
    if not counts:
        return -1
    bg = max(counts.items(), key=lambda kv: kv[1])[0]

    bands, cur = [], None
    for y in range(y0, y1):
        row = (H - 1 - y) * W
        ink = 0
        for x in range(x0, x1):
            i = (row + x) * 3
            if abs(px[i] - bg[0]) + abs(px[i + 1] - bg[1]) + abs(px[i + 2] - bg[2]) > 60:
                ink += 1
        if ink > 2:
            cur = [y, y] if cur is None else [cur[0], y]
        elif cur is not None:
            bands.append(tuple(cur))
            cur = None
    if cur is not None:
        bands.append(tuple(cur))
    if len(bands) < 2:
        # 只有一条带：无从谈重叠。
        # **已知盲点**：两行文字**完全并在一起**时会被识别成一条带 → 本判据抓不到。
        # 试过"带高异常"（某带 >= 中位带高的 2 倍）来补，但大标题带天然就是小提示带的
        # 2 倍以上 —— 真实全量误报 45/114，等于废掉判据，故回退。
        # 自检 scripts/layout-check-selftest.py 里断言了这个限制。
        return 9999
    return min(bands[i + 1][0] - bands[i][1] for i in range(len(bands) - 1))


def settings_win(d: dict | None) -> dict:
    """从 layout dump 里取设置窗口矩形（中英标题都认）。"""
    if not d:
        return {}
    for w in d.get("wins", []):
        if w.get("title") in ("Settings", "设置"):
            return w
    return {}


def dump_with_blank(page: int, scale: int, lang: str = "zh"):
    """渲染一档，返回 (布局, 内容色数, 文字带最小间隙)。"""
    d = dump(page, scale, lang)
    bmp = ROOT / "preview.bmp"
    gap = text_min_gap(bmp.read_bytes(), settings_win(d)) if bmp.exists() else -1
    return d, blank_of_shot(), gap


def main() -> int:
    strict = "--strict" in sys.argv
    off_screen, off_width, overlaps, needs_scroll = [], [], [], []
    blanks: list[str] = []
    checked = 0
    gaps_bad: list = []
    for lang, scale in COMBOS:
        for page in range(PAGE_COUNT):
            d, colors, gap = dump_with_blank(page, scale, lang)
            if 0 <= colors <= BLANK_MAX_COLORS:
                blanks.append(f"page-{page}@{scale}%（内容色 {colors}）")
            # 判据 ⑨：文字超宽（单独一次渲染，见 overflow_of 的注释）
            OVERFLOW_SEEN.extend(overflow_of(page, scale, lang))
            # 判据 ⑧：同一窗口内两行文字压在一起（间隙 <= TEXT_MIN_GAP）
            if 0 <= gap <= TEXT_MIN_GAP:
                gaps_bad.append(f"page-{page}@{scale}%·{lang}（文字带最小间隙 {gap}px）")
            if d is None:
                # **不能静默通过**：检查没跑成就是失败（附录 FH 的洞）。
                print(f"[FAIL] page-{page}@{scale}%·{lang}：没拿到布局（渲染不出来？）")
                return 1
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
            for _ov in band_overlaps(d):
                BAND_OVERLAPS.append((page, scale, lang, *_ov))
            hits = d["hits"]
            for i in range(len(hits)):
                for j in range(i + 1, len(hits)):
                    a = overlap(hits[i], hits[j])
                    if a > 0:
                        overlaps.append(
                            f"page-{page}@{scale}% {hits[i]['kind']} ∩ {hits[j]['kind']} = {a}px²")

    print(f"检查：{checked} 个组合（{PAGE_COUNT} 页 × 中英各 {len(SCALES)} 档）")
    print(f"  ⑪ 两段文字带区重叠（>2px）：{len(BAND_OVERLAPS)}")
    for _b in BAND_OVERLAPS[:8]:
        print(f"    page-{_b[0]}@{_b[1]}%·{_b[2]}：{_b[3]!r} ∩ {_b[4]!r} = {_b[5]}x{_b[6]}px")
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
    print(f"  ⑦ 空白档（内容色 <= {BLANK_MAX_COLORS}）：{len(blanks)}")
    for b in blanks[:6]:
        print(f"    {b}")
    print(f"  ⑧ 文字压文字（相邻文字带间隙 <= {TEXT_MIN_GAP}px）：{len(gaps_bad)}")
    for b in gaps_bad[:6]:
        print(f"    {b}")
    print(f"  ⑨ 文字超宽（绘制调用超出可用宽度）：{len(OVERFLOW_SEEN)}（棘轮上限 {OVERFLOW_MAX}）")
    # 判据 ⑩：交集 = 背景噪声（每页都有的）；差集 = 该页自己的文字 → 必须为空
    _en = [(pg, sc, tx) for (pg, sc, lg, tx) in CJK_SEEN if lg == "en"]
    _by = {}
    for _pg, _sc, _tx in _en:
        _by.setdefault((_pg, _sc), set()).add(_tx)
    _noise = set.intersection(*_by.values()) if _by else set()
    cjk_leaks = sorted({tx for _pg, _sc, tx in _en if tx not in _noise})
    print(f"  ⑩ 英文模式下**页面专属**的中文（已扣掉每页都有的噪声 {len(_noise)} 条）：{len(cjk_leaks)}")
    for _x in cjk_leaks[:8]:
        print(f"    {_x[:70]}")
    for _pg, _sc, _lg, _tx in OVERFLOW_SEEN[:6]:
        print(f"    page-{_pg}@{_sc}%·{_lg}：{_tx[:70]}")
    print(f"  ⑥ 窗口类界面同组重叠（判红）：{d_ov}")
    for t3 in d_list[:6]:
        print(f"    {t3}")
    off_screen = off_screen + [f"desktop x{d_off}"] * 0  # 保持原变量语义

    over_ratchet = len(OVERFLOW_SEEN) > OVERFLOW_MAX or bool(cjk_leaks) or bool(BAND_OVERLAPS)
    if strict and (off_screen or off_width or d_off or blanks or d_ov or gaps_bad or over_ratchet):
        print("[FAIL] 有控件横向越界")
        return 1
    print("[ OK ] 没有横向越界（① ② ⑤ 都是 0）" if not (off_screen or off_width or d_off)
          else "[FAIL] 有横向越界")
    return 0 if not (off_screen or off_width or d_off or blanks or d_ov or gaps_bad or over_ratchet) else 1


if __name__ == "__main__":
    sys.exit(main())
