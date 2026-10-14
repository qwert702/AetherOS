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
2. `toast_msg(zh, en)` / `toast_msg_1(zh, en, arg)` / `ToastMsg::new(zh, en)` 的英文位
   含 CJK → 判红（§3.5 遗留项 #43 收口后新增：通知正文也有英文位了）；
3. `title_en:` 与 `*_EN` 常量含 CJK → 判红；
4. **扫描覆盖面**不足 → 判红（防止判据形同虚设）。
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "aether-compositor/src"
CJK = re.compile(r'[\u4e00-\u9fff]')
STR = re.compile(r'"((?:[^"\\]|\\.)*)"')
#: 判据覆盖的"双语取词"调用形态。**每新增一种构造器都要加进来** ——
#: 否则新写法会静默逃出这条判据（这正是本文件要防的事）。
CALL = re.compile(r'\b(pick|tr|toast_msg(?:_1)?|ToastMsg::new)\s*[:(]')
MIN_CALLS = 250          # 改造前实测 274，收口后应更多；低于这个数说明解析器坏了

# 有意保留中文的例外（"用该语言自身拼写"的语言名）。**逐条写清理由**，不做模糊匹配。
ALLOW = {
    ("i18n.rs", "中文"),   # 语言下拉里"中文"就是中文的自身拼写
}


def calls(text: str):
    """产出 (行号, 函数名, 参数字符串列表)；括号配对，支持多行。"""
    for m in CALL.finditer(text):
        i = m.end() - 1
        # `ToastMsg::new(` 的 `:` 比 `(` 早一位；统一从最近的括号起算
        if text[i] != '(':
            i = text.find('(', m.end() - 1)
            if i < 0:
                continue
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


def scan_text(name: str, text: str) -> tuple[list[str], int, int]:
    """扫一段源码：返回 (问题列表, 扫到的调用数, 有意保留数)。

    抽成函数是为了**能自检** —— 判据本身必须能被"注入一个坏例子"验证会响，
    否则它和"永远绿"没有区别（本项目已栽过这个跟头：判据 ⑨ 的宽度模型错了，
    藏住 54 处真实超宽）。
    """
    bad: list[str] = []
    scanned = allowed = 0
    for line, fn, args in calls(text):
        if len(args) < 2:
            continue
        scanned += 1
        en = args[-1]
        if not CJK.search(en):
            continue
        if (name, en) in ALLOW:
            allowed += 1
            continue
        bad.append(f"{name}:{line}  {fn}() 的英文位含中文：{en[:60]}")
    # *_EN 常量与 title_en
    # 正则必须是 `[A-Z_]*_EN`（**不要求前导下划线**）：旧写法 `_[A-Z_]*_EN` 只认
    # 两段以上的名字（`SETTINGS_TITLE_EN` 认，`TITLE_EN` 不认）——
    # 是一处**静默漏检**，由 `--selftest` 的正向对照抓出来（2026-10-11）。
    for m in re.finditer(r'(title_en|[A-Z_]*_EN)\s*[:=]\s*"((?:[^"\\]|\\.)*)"', text):
        if CJK.search(m.group(2)):
            ln = text[:m.start()].count('\n') + 1
            bad.append(f"{name}:{ln}  {m.group(1)} 含中文：{m.group(2)[:60]}")
    return bad, scanned, allowed


def selftest() -> int:
    """正向对照：每条判据都要能被注入的坏例子打红，好例子不能误报。"""
    cases: list[tuple[str, str, bool]] = [
        # 好例子：三种双语取词形态都不该误报
        ("pick 正常", 'let s = pick(lang, "外观", "Appearance");', False),
        ("tr 正常", 'let s = tr("设置", "Settings");', False),
        ("toast_msg 正常", 'push_toast(t, n, toast_msg("已保存", "Saved"), K);', False),
        ("toast_msg_1 正常", 'push_toast(t, n, toast_msg_1("已保存 {}", "Saved {}", x), K);', False),
        ("ToastMsg::new 正常", 'push_toast(t, n, ToastMsg::new("已保存", "Saved"), K);', False),
        # 坏例子（正向对照）：英文位塞中文 → 必须判红
        ("pick 英文位含中文", 'let s = pick(lang, "外观", "外观");', True),
        ("tr 英文位含中文", 'let s = tr("设置", "设置");', True),
        ("toast_msg 英文位含中文", 'push_toast(t, n, toast_msg("已保存", "已保存"), K);', True),
        ("ToastMsg::new 英文位含中文", 'push_toast(t, n, ToastMsg::new("已保存", "已保存"), K);', True),
        # 真实写法是**字段 / 绑定**（`const X: &str = "…"` 中间隔着类型标注，判据本来就不看）
        ("title_en 字段含中文", 'Row { title_en: "设置" }', True),
        ("*_EN 绑定含中文", 'let TITLE_EN = "设置";', True),
        ("title_en 正常", 'Row { title_en: "Settings" }', False),
        # 有意保留：外部来源只给一种语言 → 不该误报（它本来就只有一个字符串）
        ("from_external 单串不参与", 'push_toast(t, n, ToastMsg::from_external(s), K);', False),
    ]
    failed = 0
    for label, src, want_bad in cases:
        bad, _, _ = scan_text("t.rs", src)
        got = bool(bad)
        mark = "OK  " if got == want_bad else "FAIL"
        if got != want_bad:
            failed += 1
        print(f"  [{mark}] {label}：期望{'判红' if want_bad else '通过'}，实得"
              f"{'判红' if got else '通过'}")
    if failed:
        print(f"[FAIL] i18n 判据自检不通过：{failed} 个用例与期望不符")
        return 1
    print(f"[ OK ] i18n 判据自检通过（{len(cases)} 个用例，含 5 条正向对照）")
    return 0


def main() -> int:
    if "--selftest" in sys.argv:
        return selftest()
    bad, scanned, allowed = [], 0, 0
    for f in sorted(SRC.glob("*.rs")):
        text = f.read_text(encoding='utf-8')
        b, s, a = scan_text(f.name, text)
        bad += b
        scanned += s
        allowed += a

    print(f"扫到 {scanned} 个双语取词的英文位（阈值 {MIN_CALLS}）")
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
