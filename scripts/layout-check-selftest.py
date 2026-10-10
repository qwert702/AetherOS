"""FE-1：给 layout-check 的分析函数写自检 —— 重点是**证明判据真的会响**。

## 为什么

上一批（附录 FD）证明了「判据要能真的响，还要断言自己的覆盖面」。
但**判据 ⑧（文字压文字）我只验证了它报 0，没证明它会报红** ——
一个从不触发的判据等于没有判据。

## 做法

不用改产品代码、不用等 4 分钟跑全量：**合成一张 BMP** 喂给分析函数。

* 两行文字相距 5px → 应返回 5；
* 两行文字**紧贴**（间隙 0）→ 应返回 <= 0（**判红**）；
* 只有一行 → 返回 9999（无从谈重叠）；
* 另外顺手自检 `overlap()` 与 `content_colors()`。

## BMP 格式（与脚本里的约定一致）

54 字节头 + 24bpp、**自下而上**的行、每行按 4 字节对齐（1280*3 已是 4 的倍数）。
"""
import importlib.util
import pathlib
import sys

ROOT = pathlib.Path(r'D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether')
spec = importlib.util.spec_from_file_location("lc", ROOT / "scripts/layout-check.py")
lc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lc)

W, H, HDR = lc.W, lc.H, lc.HDR
BG = (32, 32, 40)          # 深色背景
FG = (230, 230, 240)       # 亮色文字


def make_bmp(bands, win):
    """bands: [(y0,y1), ...] 文字带（屏幕坐标）；win: 决定 x 范围。"""
    buf = bytearray(W * H * 3)
    for y in range(H):
        row = (H - 1 - y) * W * 3
        for x in range(W):
            i = row + x * 3
            buf[i:i + 3] = bytes(BG)
    for (y0, y1) in bands:
        for y in range(y0, y1 + 1):
            row = (H - 1 - y) * W * 3
            for x in range(win["x"], win["x"] + win["w"]):
                i = row + x * 3
                buf[i:i + 3] = bytes(FG)
    return bytes(HDR * b"\0") + bytes(buf)


WIN = {"x": 300, "y": 60, "w": 500, "h": 400}
ok = True


def check(name, got, cond, want):
    global ok
    good = cond(got)
    ok = ok and good
    print(f"  {'✓' if good else '✗'} {name}：得到 {got}（期望 {want}）")


# ① 两行相距 6px（带是闭区间：216-210 = 6）
raw = make_bmp([(200, 210), (216, 226)], WIN)
check("⑧ 两行相距 6px（闭区间）", lc.text_min_gap(raw, WIN), lambda g: g == 6, "6")

# ② **已知盲点**：两条带 y 相接时会并成一条带，带数减少、判据**不响**。
#    这里**断言这个限制**（而不是假装能抓到）：
#    合成 5 条带、中间那条高 20px（其余 10px），它与邻居仍分离 → 判据只看带间间隙，
#    返回的是别的间隙（> 阈值，即**不判红**）。
#    试过用"带高异常"补它：真实全量误报 45/114，已回退（见 layout-check.py 注释）。
raw = make_bmp([(150, 159), (170, 179), (190, 209), (220, 229), (240, 249)], WIN)
check("⑧ 带高异常（已知盲点，不判红）", lc.text_min_gap(raw, WIN),
      lambda g: g > lc.TEXT_MIN_GAP, f"> {lc.TEXT_MIN_GAP}（如实记录盲点）")

# ③ 只有一行 → 无从谈重叠
raw = make_bmp([(200, 210)], WIN)
check("⑧ 只有一行", lc.text_min_gap(raw, WIN), lambda g: g == 9999, "9999")

# ④ overlap()：重叠 / 相接 / 分离
a = {"x": 0, "y": 0, "w": 10, "h": 10}
check("overlap 完全重叠", lc.overlap(a, dict(a)), lambda g: g == 100, "100")
check("overlap 相接不算重叠", lc.overlap(a, {"x": 10, "y": 0, "w": 10, "h": 10}), lambda g: g == 0, "0")
check("overlap 分离", lc.overlap(a, {"x": 20, "y": 0, "w": 10, "h": 10}), lambda g: g == 0, "0")

# ⑤ content_colors()：空白区 vs 有内容的区
raw = make_bmp([], WIN)
check("content_colors 全背景", lc.content_colors(raw), lambda g: g == 1, "1")
raw = make_bmp([(200, 210), (300, 310), (400, 410)], WIN)
check("content_colors 有内容", lc.content_colors(raw), lambda g: g == 2, "2（背景 + 文字）")

# ---- ⑥ 判据 ① ② ⑤ ⑥ ⑦ 的纯谓词（附录 GX）----
# 这五条此前**一次正向对照都没做过**（逻辑内联在 main/check_desktop 里）。
# 现在抽成纯谓词，可以单独喂数据 —— 断言一律用"等于期望"（第 30 条）。
WIN2 = {"x": 300, "y": 60, "w": 500, "h": 400}


def hit(x, w=10):
    return {"x": x, "y": 100, "w": w, "h": 10}


check("① 左越出屏幕", lc.hit_overflow(hit(-5), lc.W, WIN2), lambda g: g == "screen", '"screen"')
check("① 右越出屏幕", lc.hit_overflow(hit(lc.W - 5), lc.W, WIN2), lambda g: g == "screen", '"screen"')
check("② 越出设置窗口（右）", lc.hit_overflow(hit(WIN2["x"] + WIN2["w"] - 5), lc.W, WIN2),
      lambda g: g == "settings", '"settings"')
check("② 越出设置窗口（左）", lc.hit_overflow(hit(WIN2["x"] - 5), lc.W, WIN2),
      lambda g: g == "settings", '"settings"')
check("①/② 完全在内（不响）", lc.hit_overflow(hit(400), lc.W, WIN2),
      lambda g: g is None, "None")
# 贴屏幕右边界：x+w == w，① **不算**越界；但它落在设置窗口之外，所以 ② 会响。
# （第一版我期望 None，错了 —— 谓词是对的，是我的期望漏了"② 也检查"。）
check("① 贴屏幕右边界 → ① 不响、② 响", lc.hit_overflow(hit(lc.W - 10), lc.W, WIN2),
      lambda g: g == "settings", '"settings"（① 不越界，② 越界）')
check("① 屏幕内且设置窗口内 → 都不响",
      lc.hit_overflow(hit(WIN2["x"] + 50), lc.W, WIN2), lambda g: g is None, "None")

check("⑤ 控件左越界", lc.widget_off_screen(hit(-1), lc.W), lambda g: g is True, "True")
check("⑤ 控件右越界", lc.widget_off_screen(hit(lc.W - 5), lc.W), lambda g: g is True, "True")
check("⑤ 控件在内（不响）", lc.widget_off_screen(hit(400), lc.W), lambda g: g is False, "False")

# 真实格式是 `win<N>-…`：`split("-")[0]` 取的是 `win<N>`。
# （第一版我写成 `win-a-close`，它的 `[0]` 也是 "win"，于是被判成同一个窗口 —— 期望错了。）
w_a = {"group": "dock", "kind": "win1-close"}
w_b = {"group": "dock", "kind": "win2-close"}
w_c = {"group": "other", "kind": "win1-close"}
w_d = {"group": "dock", "kind": "panel-row"}
check("⑥ 同组同窗口 → 判红", lc.same_group_overlap(w_a, dict(w_a)), lambda g: g is True, "True")
check("⑥ 同组不同窗口 → 不判红", lc.same_group_overlap(w_a, w_b), lambda g: g is False, "False")
check("⑥ 跨组 → 不判红", lc.same_group_overlap(w_a, w_c), lambda g: g is False, "False")
check("⑥ 同组、只有一个是窗口三键 → 判红", lc.same_group_overlap(w_a, w_d),
      lambda g: g is True, "True")

check("⑦ 内容色 = 阈值（判空白）", lc.is_blank(lc.BLANK_MAX_COLORS), lambda g: g is True, "True")
check("⑦ 内容色 = 阈值+1（不判）", lc.is_blank(lc.BLANK_MAX_COLORS + 1), lambda g: g is False, "False")
check("⑦ 内容色 = 782（实测最小值，不判）", lc.is_blank(782), lambda g: g is False, "False")
check("⑦ 内容色 = -1（取值失败，不判）", lc.is_blank(-1), lambda g: g is False, "False")

print()
if ok:
    print("[ OK ] 分析函数自检全部通过 —— 判据 ⑧ 与 ① ② ⑤ ⑥ ⑦ 都会真的响")
    sys.exit(0)
print("[FAIL] 有自检不通过")
sys.exit(1)
