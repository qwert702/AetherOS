"""FD-1：把「英文位是否漏译」做成常驻检查 scripts/i18n-check.py。

## 为什么要有

本会话把界面双语化了，但**没有任何判据保证"以后新增的串也带英文"**。
漏译的表现是：英文界面里冒出中文。机器能查的硬事实：
**英文位里出现了 CJK 字符**。

## 关键设计：断言扫描覆盖面

第一版扫描按**行**做正则 → 漏掉所有多行调用 → 报「0 处」。
**那个 0 不是证据，是扫描器没覆盖到。**
所以本脚本**断言扫到的调用数 >= 200** —— 解析器一坏就**报错**，而不是静默通过。

## 检查项

1. `pick(lang, zh, en)` / `tr(zh, en)` 的**英文位**含 CJK → 判红；
2. `title_en:` 与 `*_EN` 常量含 CJK → 判红；
3. **扫描覆盖面**不足 → 判红（防止判据形同虚设）。
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "aether-compositor/src"
CJK = re.compile(r'[\u4e00-\u9fff]')
STR = re.compile(r'"((?:[^"\\]|\\.)*)"')
MIN_CALLS = 200          # 实测 270 个；低于这个数说明解析器坏了

# 有意保留中文的例外（"用该语言自身拼写"的语言名）。**逐条写清理由**，不做模糊匹配。
ALLOW = {
    ("i18n.rs", "中文"),   # 语言下拉里"中文"就是中文的自身拼写
}


def calls(text: str):
    """产出 (行号, 函数名, 参数字符串列表)；括号配对，支持多行。"""
    for m in re.finditer(r'\b(pick|tr)\s*\(', text):
        i = m.end() - 1
        depth, j, in_str = 0, i, False
        while j < len(text):
            c = text[j]
            if in_str:
                if c == '\\':
                    j += 2
                    continue
                if c == '"':
                    in_str = False
            elif c == '"':
                in_str = True
            elif c == '(':
                depth += 1
            elif c == ')':
                depth -= 1
                if depth == 0:
                    break
            j += 1
        yield text[:m.start()].count('\n') + 1, m.group(1), STR.findall(text[i + 1:j])


def main() -> int:
    bad, scanned, allowed = [], 0, 0
    for f in sorted(SRC.glob("*.rs")):
        text = f.read_text(encoding='utf-8')
        for line, fn, args in calls(text):
            if len(args) < 2:
                continue
            scanned += 1
            en = args[-1]
            if not CJK.search(en):
                continue
            if (f.name, en) in ALLOW:
                allowed += 1
                continue
            bad.append(f"{f.name}:{line}  {fn}() 的英文位含中文：{en[:60]}")
        # *_EN 常量与 title_en
        for m in re.finditer(r'(title_en|_[A-Z_]*_EN)\s*[:=]\s*"((?:[^"\\]|\\.)*)"', text):
            if CJK.search(m.group(2)):
                ln = text[:m.start()].count('\n') + 1
                bad.append(f"{f.name}:{ln}  {m.group(1)} 含中文：{m.group(2)[:60]}")

    print(f"扫到 {scanned} 个 pick/tr 调用的英文位（阈值 {MIN_CALLS}）")
    if scanned < MIN_CALLS:
        print(f"[FAIL] 扫描覆盖面不足（{scanned} < {MIN_CALLS}）—— 解析器可能坏了，"
              f"这个 0 不是证据")
        return 1
    if bad:
        print(f"[FAIL] 英文位含中文：{len(bad)} 处")
        for b in bad[:20]:
            print(f"  {b}")
        return 1
    print(f"[ OK ] 英文位无中文（有意保留 {allowed} 处，见脚本里的 ALLOW 与理由）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
