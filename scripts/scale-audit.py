"""CO-1 收尾：扫描器排除两类误报 + 重新验证。

两类误报（本批实测）：
1. 步长前面**紧跟着另一个 `ry +=`** —— 说明高度已经被那一步消化掉了，这里的 N 是**额外间隙**；
2. 配对的矩形离得太远（>10 行）—— 多半是别处的矩形（例如中间已经画了文本）。
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


def prev_meaningful(i: int) -> str:
    """往上找最近的非空、非单独括号的一行。"""
    for k in range(i - 1, max(-1, i - 6), -1):
        s = lines[k].strip()
        if s and s not in ('}', '{', ');', '} else {', ');'):
            return s
    return ''


rows = []
for i, line in enumerate(lines):
    m = re.search(r'Rect \{[^}]*y: ry[ ,][^}]*h: theme::size::px\(theme::size::([A-Z_0-9]+)\)', line)
    if not m:
        continue
    name = m.group(1)
    val = table.get(name)
    if not val:
        continue
    for j in range(i + 1, min(len(lines), i + 10)):   # 只在 10 行内配对
        s = re.match(r'\s*ry \+= (\d+);', lines[j])
        if not s:
            continue
        step = int(s.group(1))
        # 跨页 arm 不配对：`"某页" => {` 之后 `ry` 是重新起算的
        # （本批实测的最后一处误报就是这种：把上一页的矩形与下一页的第一步配上了）
        if any(re.match(r'\s*"[^"]+"\s*=>\s*\{', lines[k]) for k in range(i, j)):
            break
        if prev_meaningful(j).startswith('ry +='):   # 前一步已经消化了高度 → 这是额外间隙
            break
        rows.append((j + 1, step, name, val))
        break

bad = []
for (ln, step, name, val) in rows:
    x125, x150 = val * 125 // 100, val * 150 // 100
    if step < x150 or step < x125:
        bad.append((ln, step, name, val, x150))

print(f"严格配对（矩形 y 就是 ry，且 10 行内、前一步不是 ry +=）：{len(rows)} 处")
for (ln, step, name, val) in rows:
    print(f"  行 {ln:5}: ry += {step:3}  高度 {name}={val}  → 150% 需 ≥ {val * 150 // 100}")
print()
print(f"会重叠：{len(bad)} 处")
for b in bad:
    print(f"  {b}")
if '--strict' in sys.argv and bad:
    print("[FAIL] 有「步长小于缩放后高度」的地方")
    sys.exit(1)
print("[ OK ] 没有「步长小于缩放后高度」的地方" if not bad else "[报告] 见上")
