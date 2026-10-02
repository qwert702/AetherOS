"""截图 BMP 像素级对比（Step 1/2 纯重构改动的验收工具）。

用法（在放截图的目录下执行）:
  python shot-diff.py mask    # 两轮基线差异 → 输出非确定区（时钟/光标闪烁等）
  python shot-diff.py check   # after vs before 的差异必须全部落在掩码内

约定：BMP 为 24bpp 无行填充、1280x760、54 字节头（由 draw::write_bmp 生成）。

注意：这套方法只适用于"不该有任何视觉变化"的重构（Step 1 令牌收口、Step 2 字体参数）。
Step 3 之后视觉已按设计大幅变化，不要再用它作为验收门槛。
"""
import sys
import pathlib

W, H, HDR = 1280, 760, 54
D = pathlib.Path(__file__).parent

# 天然非确定区：时钟（跨分钟必然不同）与 AI 光标闪烁框
MASK_REGIONS = [
    (1215, 1280, 0, 32),     # 顶栏时钟
    (515, 530, 595, 630),    # AI 指令条光标
]


def pixels(name):
    raw = (D / name).read_bytes()
    if len(raw) != HDR + W * H * 3:
        # 不用 assert：-O 下会被剥离，尺寸不对的帧会被当成正常帧比对（审计 L-6）
        raise SystemExit(f"{name}: 只有 {len(raw)} 字节，与 {W}x{H} 不符")
    return raw[HDR:]


def diff_set(a_name, b_name):
    a, b = pixels(a_name), pixels(b_name)
    return {i for i in range(W * H * 3) if a[i] != b[i]}


def coords(idx_set):
    out = set()
    for i in idx_set:
        p, _ = divmod(i, 3)
        y = H - 1 - p // W
        x = p % W
        out.add((x, y))
    return out


def bounding(pts):
    if not pts:
        return "EMPTY"
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    return f"x[{min(xs)},{max(xs)}] y[{min(ys)},{max(ys)}] n={len(pts)}"


def mask_set():
    mask = set()
    for l in (1, 2, 3, 4):
        mask |= coords(diff_set(f"before-r1-l{l}.bmp", f"before-r2-l{l}.bmp"))
    for (x0, x1, y0, y1) in MASK_REGIONS:
        for y in range(y0, y1):
            for x in range(x0, x1):
                mask.add((x, y))
    return mask


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    cmd = sys.argv[1]

    if cmd == "mask":
        for l in (1, 2, 3, 4):
            d = diff_set(f"before-r1-l{l}.bmp", f"before-r2-l{l}.bmp")
            print(f"L{l}: {bounding(coords(d))}")
        return 0

    if cmd == "check":
        mask = mask_set()
        ok = True
        for l in (1, 2, 3, 4):
            d = coords(diff_set(f"before-r1-l{l}.bmp", f"after-l{l}.bmp"))
            extra = d - mask
            status = "OK" if not extra else f"FAIL extra={bounding(extra)}"
            if extra:
                ok = False
            print(f"L{l}: changed={len(d)} mask={len(mask)} outside_mask={len(extra)} {status}")
        return 0 if ok else 1

    print(f"未知子命令: {cmd}")
    return 2


if __name__ == "__main__":
    sys.exit(main())
