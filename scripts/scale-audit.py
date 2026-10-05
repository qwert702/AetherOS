"""静态扫描「尺寸缩放而步长/宽度写死」的同类 bug（§2.4）。

## 为什么

本会话修掉的一类 bug 有共同形状：**尺寸走了 `theme::size::px(...)`（跟着缩放），
而排布的步长是裸数字**。它们在 100% 下恰好不重叠，125%/150% 下才压住。

几何断言能撞到它们，但那是**碰运气**（要正好覆盖到那一页那一档）。
这个脚本**从源码里找**，判据是精确的，不是猜。

## 判据

**① 纵向**：`ry += N`（裸数字），且它前面最近的、`y` 就是 `ry` 的矩形高度走了 `size::px(H)`
→ `N < 1.5×H` 就一定在 150% 下重叠（`N < 1.25×H` 则 125% 就会）。

**② 横向**：矩形里同时有 `w: size::px(W)` 与 `x: … + … * STEP`（写死的步长）
→ `STEP < 1.5×W` 时相邻两个会重叠。

**② 目前查出 0 处 —— 而且这不是"没问题"，是"这一类的两处真 bug 不长这样"**：
息屏档位与鼠标速度段控件的问题是**整行超出容器**（相邻按钮之间并不重叠），
那只有 `layout-check.py` 的**横向越界**断言能抓。② 留着是给"相邻重叠"这个子类当守卫。

## 误报规则（都是实测踩出来的）

1. 步长前面紧跟着另一个 `ry +=` → 高度已被那一步消化，这里的 N 是**额外间隙**；
2. 配对距离超过 10 行 → 多半是别处的矩形（中间已经画了文本）；
3. **跨页 arm** → `"某页" => {` 之后 `ry` 是**重新起算**的；
4. 宽度是**字面量**（不缩放）→ 步长写死也不会重叠。

用法:
  python scripts/scale-audit.py            # 报告
  python scripts/scale-audit.py --strict   # 有会重叠的就退出码 1（门禁用）
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(r'D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether')
DRAW = ROOT / "aether-compositor/src/draw.rs"
text = DRAW.read_text(encoding='utf-8')
cut = text.rfind('#[cfg(test)]')
prod = text[:cut] if cut > 0 else text
lines = prod.split('\n')
table = {m.group(1): int(m.group(2))
         for m in re.finditer(r'pub const ([A-Z_0-9]+): i32 = (\d+);', prod)}


def main() -> int:
    strict = '--strict' in sys.argv
    print(f"尺寸表：{len(table)} 个常量")

    # ---- ① 纵向：ry += N（原有）----
    def prev_meaningful(i: int) -> str:
        for k in range(i - 1, max(-1, i - 6), -1):
            s = lines[k].strip()
            if s and s not in ('}', '{', ');', '} else {', ');'):
                return s
        return ''

    vertical = []
    for i, line in enumerate(lines):
        m = re.search(r'Rect \{[^}]*y: ry[ ,][^}]*h: theme::size::px\(theme::size::([A-Z_0-9]+)\)', line)
        if not m:
            continue
        val = table.get(m.group(1))
        if not val:
            continue
        for j in range(i + 1, min(len(lines), i + 10)):
            s = re.match(r'\s*ry \+= (\d+);', lines[j])
            if not s:
                continue
            if any(re.match(r'\s*"[^"]+"\s*=>\s*\{', lines[k]) for k in range(i, j)):
                break
            if prev_meaningful(j).startswith('ry +='):
                break
            vertical.append((j + 1, int(s.group(1)), m.group(1), val))
            break

    # ---- ② 横向：x 里 * STEP 且宽度走了 size::px ----
    horizontal = []
    for i, line in enumerate(lines):
        if 'Rect {' not in line:
            continue
        mw = re.search(r'w: theme::size::px\(theme::size::([A-Z_0-9]+)\)', line)
        ms = re.search(r'x: [^,}]*\*\s*(\d+)', line)
        if mw and ms:
            val = table.get(mw.group(1))
            if val:
                horizontal.append((i + 1, int(ms.group(1)), mw.group(1), val))

    # ---- ④ 循环/累积步长写死（`i * N` / `y += N` / `base += N`，N >= 20）----
    # 与 ③ 同类但写法不同：位置由「基准 + i * 步长」算出。本批的文件侧栏就是这样。
    loop_steps = []
    for i, line in enumerate(lines):
        if 'theme::size::px' in line:
            continue
        if '.len()' in line:      # `items.len() * N` 是**数量乘**，不是步长（实测误报）
            continue
        for m in re.finditer(r'(?:\*|\+=)\s*(\d+)\b', line):
            n = int(m.group(1))
            if n < 20:
                continue
            st = line.strip()
            if not any(k in st for k in (' y', 'y ', 'base', 'x')):
                continue
            loop_steps.append((i + 1, st[:90]))
            break

    # ---- ③ 「矩形.y + N」的布局步长（N >= 20 才算；小偏移是细线/内衬，故意不缩）----
    fixed_offsets = []
    for i, line in enumerate(lines):
        for m in re.finditer(r'[xy]:\s*([a-z_0-9]+)\.([xy])\s*\+\s*(\d+)', line):
            n = int(m.group(3))
            if n >= 20 and 'theme::size::px' not in line:
                fixed_offsets.append((i + 1, m.group(0), n))

    print()
    print(f"① 纵向「ry += N vs 缩放高度」：{len(vertical)} 处")
    v_bad = []
    for (ln, step, name, val) in vertical:
        x150 = val * 150 // 100
        if step < x150:
            v_bad.append((ln, step, name, val, x150))
            print(f"   行 {ln}: ry += {step}  高度 {name}={val}  150% 需 ≥ {x150}  ← 会重叠")
    print(f"   其中会重叠：{len(v_bad)}")

    print(f"④ 循环/累积步长写死（>=20）：{len(loop_steps)} 处")
    for (ln, frag) in loop_steps:
        print(f"   行 {ln}: {frag}")
    print(f"③ 「矩形.y/x + 裸数字」的布局步长（>=20）：{len(fixed_offsets)} 处")
    for (ln, frag, n) in fixed_offsets:
        print(f"   行 {ln}: {frag}")
    print(f"② 横向「x 的 STEP vs 缩放宽度」：{len(horizontal)} 处")
    h_bad = []
    for (ln, step, name, val) in horizontal:
        x150 = val * 150 // 100
        flag = ''
        if step < x150:
            h_bad.append((ln, step, name, val, x150))
            flag = '  ← 150% 会重叠'
        print(f"   行 {ln}: STEP {step}  宽度 {name}={val}  150% 需 ≥ {x150}{flag}")
    print(f"   其中会重叠：{len(h_bad)}")

    if strict and (v_bad or h_bad or fixed_offsets or loop_steps):
        print("[FAIL] 有「步长小于缩放后尺寸」或「矩形偏移写死」的地方")
        return 1
    print("[ OK ] 四类都没有问题" if not (v_bad or h_bad or fixed_offsets or loop_steps)
          else "[报告] 见上")
    return 0


if __name__ == '__main__':
    sys.exit(main())
