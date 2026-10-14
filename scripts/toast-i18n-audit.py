#!/usr/bin/env python3
"""通知正文双语审计（FEATURE-GAPS §3.5 遗留项 #43）。

## 为什么要有它

通知中心的历史与 toast 的正文，是各发送方在**产生通知时**写好的字符串。
§3.5 收口时把「框架文案」双语化了，但 `push_toast(...)` 的**正文**留在各处：

    改造前实测（2026-10-11）：aether-compositor/src/main.rs 里 **117 个生产调用点**
    （另有 1 处是函数定义本身）—— 38 处走 `pick_zh_en`、7 处是调用点自写的
    `if is_en(){…}` 内联分支、5 处**只有中文**、67 处把变量直接传进来（需溯源）。

    改造后（同日晚，本批提交）：117 处全部走结构化双语 —— 直接写下两种语言的
    48 处、外部来源（aetherd）29 处、其余为裸标识符（类型即保证）。

「79 处」是这次审计算出来的，不是估的 —— 这个脚本就是干这个的。
它同时也是**覆盖判据**：任何 `push_toast` 的正文参数只要不是 `ToastMsg` 表达式
（`toast(…)` / `toast_msg(…)` / `toast_msg_1(…)` / `ToastMsg::…`）即判红。

## 改造完成后的语义（2026-10-11 起）

`sink` 的类型已经收成 `ToastMsg`，于是**"忘了英文"在编译期就不成立**了：
写 `push_toast(…, "中文", …)` 根本编译不过。所以：

* **编译器是那条硬保证**，本脚本是它的**防御性复检**（有人把签名改回 `String`、
  又在调用点写回 `if is_en() {…}` 时能立刻发现）；
* **裸标识符算通过**（`msg` / `why`）—— 它只可能是 `ToastMsg`；
  早期版本把这 37 处报成"未双语"，那是**解析器的局限冒充缺陷**，已修正；
* 仍然判红的三类：`旧形态`（`pick_zh_en`）、`内联分支`（调用点自己写 `is_en()`）、
  `只有中文`（绕过构造器直接塞中文字面量 / `format!(中文)`）。

## 六个分类

| 分类 | 表达式形态 | 是否算通过 | 说明 |
|---|---|---|---|
| 已双语 | `toast_msg(zh, en)` / `toast_msg_1(…, zh, en, arg)` / `ToastMsg::…` | ✅ | — |
| 外部来源 | `ToastMsg::from_external(s)` | ✅ | 文案由 aetherd 按其**连接语言**渲染，**不得**再套一层翻译 |
| 裸标识符 | `msg` / `why` / 函数返回值 | ✅ | 类型即保证（编译不过就说明错了） |
| 旧形态 | `pick_zh_en(zh, en)` | ❌ | 已是双语，但要换成 `ToastMsg` |
| 内联分支 | 调用点自己写 `if is_en() { … } else { … }` | ❌ | 不漏译，但同一句话散在调用点上 |
| 只有中文 | 直接塞中文字面量 / `format!("中文…")` | ❌ | 绕过构造器，英文位必然没词 |

## 用法

    python scripts/toast-i18n-audit.py             # 打印分类清单（Markdown 表）
    python scripts/toast-i18n-audit.py --check     # 门禁：旧形态 / 内联分支 / 只有中文 → 判红
    python scripts/toast-i18n-audit.py --selftest  # 解析器自检（逐条比对分类）

## 关键设计：断言扫描覆盖面

`i18n-check.py` 的教训是「扫到 0 处不是证据，是扫描器没覆盖到」。
这里同样断言：生产调用点数必须 >= `MIN_SITES`（实测 117），低于它直接判红。
"""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
TARGET = ROOT / "aether-compositor" / "src" / "main.rs"

#: 实测 118 个生产调用点（2026-10-11）。低于它说明解析器坏了 —— 报错，不要静默通过。
MIN_SITES = 100

#: 正文参数的**正确**形态：结构化双语类型 `ToastMsg` 的构造器。
#: 认全主程序里的构造助手：`toast_msg` / `toast_msg_1` / `toast_msg_lang`，
#: 以及直接构造 `ToastMsg::…`。
#: （初版只认 `toast` / `toast_1..3`，于是改造后新增的 `toast_msg(` 全被误判成"只有中文" ——
#: 自检里现在有这两条正向对照。）
OK_PREFIX = re.compile(r"^(?:toast(?:_msg(?:_lang)?)?(?:_[1-3])?|ToastMsg)\s*[:(]")
#: 同一件事的**任意位置**版本（`if … { toast_msg(..) } else { toast_msg(..) }` 也算已双语）。
OK_ANY = re.compile(r"\btoast(?:_msg(?:_lang)?)?(?:_[1-3])?\s*[:(]|\bToastMsg\s*[:(]")

#: 外部来源：文案由 aetherd 按其连接语言渲染，**不得**再套一层翻译。
EXTERNAL_PREFIX = re.compile(r"^ToastMsg::from_external\s*[:(]")
EXTERNAL_ANY = re.compile(r"\bToastMsg::from_external\s*[:(]")

#: 阶段 2.2 之前的过渡形态（返回 String）。算"已双语"，但要换成 ToastMsg。
LEGACY = re.compile(r"^pick_zh_en(?:_[1-3])?\s*\(")

#: 同一个东西的**任意位置**版本。`^` 锚定的 `LEGACY` 只能认参数位，
#: 认不出 `if mount { pick_zh_en_1(…) } else { … }` 这种内联形态
#: （本脚本初版就因此把 3 处**已双语**误报成"只有中文"，把真漏译夸大成三倍）。
LEGACY_ANY = re.compile(r"\bpick_zh_en(?:_[1-3])?\s*\(")

#: 已是双语、但**形态不合规**：调用点自己写 `if is_en() { … } else { … }`。
#: 它不漏译，但同一句话散在调用点上，改一处必漏另一处 —— 正是本项目要消灭的写法。
INLINE = re.compile(r"\bis_en\s*\(\s*\)")

#: 表达式里出现 CJK → 可以确定是"只有中文"（真漏译）。
CJK = re.compile(r"[\u4e00-\u9fff]")

CALL = re.compile(r"\bpush_toast\s*\(")
TEST_CFG = re.compile(r"^#\[cfg\(test\)\]", re.M)

#: 分类 → 中文标签（打印与审计表共用，避免两处各写一份）
LABEL = {
    "ok": "已双语",
    "external": "外部来源",
    "legacy": "旧形态",
    "inline": "内联分支",
    "zh_only": "**只有中文**",
    "passthru": "裸标识符",
}

#: 门禁判定：这几类算"已过"。
#: `passthru`（裸标识符）也算 —— 参数类型是 `ToastMsg`，编译不过就说明写错了，
#: **编译器才是那条硬保证**；把类型正确的东西报成缺陷，是解析器的局限冒充问题。
DONE = ("ok", "external", "passthru")


def blank_comments(text: str) -> str:
    """把注释替换成等长空格（保留换行），使后续下标与原文一一对应。

    必须自己走一遍词法，不能用正则：`"http://x"` 里的 `//` 不是注释，
    `r#"…"#` 里的内容也不能当字符串结束。
    """
    out = list(text)
    i, n = 0, len(text)

    def blank(a: int, b: int) -> None:
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = text[i]
        # 行注释
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
            continue
        # 块注释（Rust 允许嵌套）
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
            continue
        # 原始字符串 r"…" / r#"…"# / r##"…"##
        if c == "r" and i + 1 < n and text[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and text[j] == "#":
                hashes += 1
                j += 1
            if j < n and text[j] == '"':
                close = '"' + "#" * hashes
                end = text.find(close, j + 1)
                i = n if end < 0 else end + len(close)
                continue
        # 普通字符串（含字节串 b"…"）
        if c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    j += 1
                    break
                j += 1
            i = j
            continue
        # 字符字面量 / 生命周期：'a 是生命周期，'x' 或 '\n' 是字符
        if c == "'":
            m = re.compile(r"'(?:\\.|[^\\'])'").match(text, i)
            i = m.end() if m else i + 1
            continue
        i += 1
    return "".join(out)


def literal_mask(text: str) -> list[bool]:
    """逐字符标记「是否处在字符串/字符字面量内部」。

    为什么需要：参数切分必须在**字符串里**的逗号上不切。
    `toast("中, 逗号", "en, comma")` 就是这条的自检用例（曾经真的切错过）。
    """
    mask = [False] * len(text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c == "r" and i + 1 < n and text[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and text[j] == "#":
                hashes += 1
                j += 1
            if j < n and text[j] == '"':
                close = '"' + "#" * hashes
                end = text.find(close, j + 1)
                end = n if end < 0 else end + len(close)
                for k in range(i, end):
                    mask[k] = True
                i = end
                continue
        if c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    j += 1
                    break
                j += 1
            for k in range(i, min(j, n)):
                mask[k] = True
            i = j
            continue
        if c == "'":
            m = re.compile(r"'(?:\\.|[^\\'])'").match(text, i)
            if m:
                for k in range(i, m.end()):
                    mask[k] = True
                i = m.end()
                continue
        i += 1
    return mask


def split_args(text: str, open_idx: int) -> tuple[list[str], int]:
    """从 `(` 的下标开始，按**顶层逗号**切分参数；返回 (参数列表, 右括号下标)。

    `text` 必须是已抹掉注释的版本（下标与原文一致）。
    """
    mask = literal_mask(text)
    args: list[str] = []
    depth, start, i, n = 0, open_idx + 1, open_idx, len(text)
    while i < n:
        if not mask[i]:
            c = text[i]
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
                if depth == 0:
                    args.append(text[start:i])
                    return args, i
            elif c == "," and depth == 1:
                args.append(text[start:i])
                start = i + 1
        i += 1
    raise ValueError("括号不配对：源码被截断或解析器有 bug")


def classify(expr: str) -> str:
    """把一个正文表达式分类。

    **改造完成后的语义（2026-10-11 起）**：`push_toast` 的第 3 个参数类型就是
    `ToastMsg`，所以"这里是 String"在编译期已经不成立 ——
    **编译器才是那条硬保证**，本函数是它的**防御性复检**：

    * 有人把 `push_toast` 的签名改回 `impl Into<String>`（那就等于把门又打开）；
    * 有人又在调用点上写 `if is_en() { … }` 这类内联分支（形态不合规）；
    * 有人绕过构造器直接塞一个中文字面量 / `format!(中文)`。

    因此"裸标识符"（`msg`、`why`）**算通过**：它只可能是 `ToastMsg`，否则编译不过。
    这一点很关键 —— 早期版本把 37 个 `msg` 当成"未双语"，那是**解析器的局限冒充缺陷**。
    """
    e = expr.strip()
    if EXTERNAL_ANY.search(e):
        return "external"
    # 双语构造器出现在表达式**任意位置**（含 `if … { toast_msg(..) } else { toast_msg(..) }`）
    if OK_ANY.search(e):
        return "ok"
    if LEGACY.match(e):
        return "legacy"
    if INLINE.search(e) or LEGACY_ANY.search(e):
        return "inline"
    # 裸字符串 / `format!` / `.to_string()`：只有在这一档才真的可疑
    if CJK.search(e) and re.match(r'^(format!|"[^"]*"|\'[^\']*\')', e):
        return "zh_only"
    return "passthru"


def first_line(expr: str) -> str:
    """表达式压成一行，供打印。"""
    return " ".join(expr.split())


#: 带占位符的构造器：`toast_msg_1(…)` / `ToastMsg::new_1(…)`。
#: `toast_msg(` 与 `toast_msg_1(` 都在此列（后者期望 1 个 `{}`，前者 0 个）。
CTOR = re.compile(r"\b(toast_msg(?:_[0-9])?|ToastMsg::new_1)\s*\(")


def placeholder_problems(text: str) -> list[str]:
    """中英模板的 `{}` 个数必须**与构造器后缀一致**。

    为什么值得一条判据：`toast_msg_1` 只替换**第一个** `{}`。
    若英文模板里写了两个 `{}`（而中文只有一个），界面上会**原样显示一个 `{}`** ——
    不报错、不崩、单测也未必覆盖到，属于最难发现的那类界面缺陷。
    """
    problems: list[str] = []
    blanked = blank_comments(text)
    mask = literal_mask(blanked)
    for m in CTOR.finditer(blanked):
        # 跳过函数定义（`fn toast_msg(` / `fn toast_msg_1(`）
        head = blanked[max(0, m.start() - 6):m.start()]
        if re.search(r"\bfn\s+$", head):
            continue
        args, _ = split_args(blanked, m.end() - 1)
        if len(args) < 2:
            continue
        tmpl = []
        for a in args[:2]:
            # 取模板里**第一个字符串字面量**（模板必是字面量；`format!` 不算，它自带参数）
            mm = re.search(r'"((?:[^"\\]|\\.)*)"', a)
            if not mm:
                tmpl = []
                break
            tmpl.append(mm.group(1))
        if len(tmpl) != 2:
            continue
        want = 1 if m.group(1).endswith(("_1", "new_1")) else 0
        line = blanked[:m.start()].count("\n") + 1
        for label, t in (("中文", tmpl[0]), ("英文", tmpl[1])):
            got = t.count("{}")
            if got != want:
                problems.append(
                    f"main.rs:{line}  {label}模板的占位符数是 {got}，"
                    f"而 `{m.group(1)}` 只替换 {want} 个：{t[:50]!r}"
                )
    return problems


def scan(text: str) -> list[tuple[int, str, str]]:
    """产出 (行号, 分类, 表达式) —— 只看**生产**代码（`#[cfg(test)]` 之前）。"""
    blanked = blank_comments(text)
    m_test = TEST_CFG.search(blanked)
    prod_end = m_test.start() if m_test else len(blanked)

    sites: list[tuple[int, str, str]] = []
    for m in CALL.finditer(blanked):
        if m.start() >= prod_end:
            continue
        # 跳过定义本身：`fn push_toast(`
        head = blanked[max(0, m.start() - 12):m.start()]
        if re.search(r"\bfn\s+$", head):
            continue
        args, _ = split_args(blanked, m.end() - 1)
        if len(args) < 3:
            raise ValueError(f"第 {blanked[:m.start()].count(chr(10)) + 1} 行的 push_toast "
                             f"只有 {len(args)} 个参数（期望 >= 3）")
        msg = args[2]
        line = blanked[:m.start()].count("\n") + 1
        # 分类只看**表达式形态**，字符串内容不受抹注释影响（字符串从未被抹掉）
        sites.append((line, classify(msg), first_line(msg)))
    return sites


def run(src: str) -> list[tuple[int, str, str]] | None:
    """对一段源码跑一遍；解析失败返回 None（自检据此判红）。"""
    try:
        return scan(src)
    except ValueError:
        return None


def selftest() -> int:
    """解析器自检：**逐条比对分类**，而不是只比对"判红/判绿"。

    为什么必须比分类：初版自检只断言"这条会判红"，于是「三目里嵌 pick_zh_en」
    被错分成"只有中文"时**照样通过** —— 判据在自以为是的路上没被发现。
    现在每条用例都要给出**期望分类**（`none` = 该片段不该有生产调用点）。
    """
    cases: list[tuple[str, str, str]] = [
        ("单行已双语", 'fn f(){ push_toast(&mut t, &mut n, toast("中", "en"), K); }', "ok"),
        ("主程序的 toast_msg 助手也算已双语",
         'fn f(){ push_toast(&mut t, &mut n, toast_msg("中", "en"), K); }', "ok"),
        ("toast_msg_1 / toast_msg_lang 也算",
         'fn f(){ push_toast(&mut t, &mut n, toast_msg_1("中 {}", "en {}", x), K); }\n'
         'fn g(){ push_toast(&mut t, &mut n, toast_msg_lang(|l| l.to_string()), K); }', "ok"),
        ("多行 + 嵌套括号",
         'fn f(){ push_toast(\n  &mut t,\n  &mut n,\n  toast_1("中 {}", "en {}", x.len()),\n  K,\n); }', "ok"),
        ("旧形态 pick_zh_en 应视为待改",
         'fn f(){ push_toast(&mut t, &mut n, pick_zh_en("中", "en"), K); }', "legacy"),
        ("外部来源（aetherd 已本地化）",
         'fn f(){ push_toast(&mut t, &mut n, ToastMsg::from_external(s), K); }', "external"),
        ("中文漏译",
         'fn f(){ push_toast(&mut t, &mut n, format!("中文 {}", x), K); }', "zh_only"),
        ("内联 is_en 分支（双语但形态不合规）",
         'fn f(){ push_toast(&mut t, &mut n, if is_en() { "a" } else { "中" }, K); }', "inline"),
        ("三目里嵌 pick_zh_en（**必须**是 inline，不能算漏译）",
         'fn f(){ push_toast(&mut t, &mut n, if m { pick_zh_en("中", "en") } else { pick_zh_en("甲", "b") }, K); }',
         "inline"),
        ("条件里两个分支都双语 → ok",
         'fn f(){ push_toast(&mut t, &mut n, if m { toast_msg("中", "en") } else { toast_msg("甲", "b") }, K); }',
         "ok"),
        ("裸标识符（类型即保证）", 'fn f(){ push_toast(&mut t, &mut n, msg, K); }', "passthru"),
        ("函数返回值（类型即保证）",
         'fn f(){ push_toast(&mut t, &mut n, set_layout(&mut d, l).unwrap(), K); }', "passthru"),
        ("注释里的调用不算数",
         'fn f(){ /* push_toast(&mut t, &mut n, format!("中文"), K); */ }', "none"),
        ("定义本身不算调用", 'fn push_toast(a: A, b: B, msg: M, k: K) {}', "none"),
        ("测试块里的不算生产",
         'fn f(){}\n#[cfg(test)]\nmod t { fn g(){ push_toast(&mut t, &mut n, format!("中"), K); } }', "none"),
        ("参数里的逗号不切错（字符串感知）",
         'fn f(){ push_toast(&mut t, &mut n, toast("中, 逗号", "en, comma"), K); }', "ok"),
    ]
    # 占位符一致性单独用一组用例（它判的是构造器，不是 push_toast 分类）
    ph_cases = [
        ("中英各一个占位符 → 通过",
         'fn f(){ p(toast_msg_1("正在重启 {}", "Restarting {}"), K); }', False),
        ("英文多一个占位符 → 判红（界面上会留 {}）",
         'fn f(){ p(toast_msg_1("正在重启 {}", "Restarting {} of {}"), K); }', True),
        ("无占位符构造器里写了 {} → 判红",
         'fn f(){ p(toast_msg("已保存 {}", "Saved {}"), K); }', True),
        ("定义本身不算", 'fn toast_msg_1(zh: &str, en: &str, a: u8) {}', False),
    ]
    failed = 0
    for name, src, expect in cases:
        sites = run(src)
        if sites is None:
            got = "解析失败"
        elif not sites:
            got = "none"
        else:
            got = sites[0][1]
        mark = "OK  " if got == expect else "FAIL"
        if got != expect:
            failed += 1
        print(f"  [{mark}] {name}: 期望 {expect}，实得 {got}"
              f"（{0 if sites is None else len(sites)} 个调用点）")
    for label, src, want_bad in ph_cases:
        probs = placeholder_problems(src)
        got_bad = bool(probs)
        mark = "OK  " if got_bad == want_bad else "FAIL"
        if got_bad != want_bad:
            failed += 1
        print(f"  [{mark}] {label}：期望{'判红' if want_bad else '通过'}，"
              f"实得{'判红' if got_bad else '通过'} {probs}")
    if failed:
        print(f"[FAIL] 自检不通过：{failed} 个用例与期望不符")
        return 1
    print(f"[ OK ] 自检通过（{len(cases)} 个分类用例 + {len(ph_cases)} 个占位符用例）")
    return 0


def main() -> int:
    if "--selftest" in sys.argv:
        return selftest()
    if not TARGET.exists():
        print(f"[FAIL] 找不到 {TARGET}")
        return 1
    text = TARGET.read_text(encoding="utf-8")
    try:
        sites = scan(text)
    except ValueError as e:
        print(f"[FAIL] 解析失败：{e}")
        return 1

    buckets: dict[str, list[tuple[int, str]]] = {k: [] for k in LABEL}
    for line, kind, expr in sites:
        buckets[kind].append((line, expr))

    summary = " · ".join(f"{LABEL[k]} {len(buckets[k])}" for k in LABEL if k != "ok")
    ph = placeholder_problems(text)
    if "--check" in sys.argv:
        print(f"生产 push_toast 调用点：{len(sites)} 个（已双语 {len(buckets['ok'])} · {summary}）")
        if len(sites) < MIN_SITES:
            print(f"[FAIL] 扫描覆盖面不足（{len(sites)} < {MIN_SITES}）—— 解析器可能坏了，"
                  f"这个数字不是证据")
            return 1
        todo = [k for k in LABEL if k not in DONE]
        n_todo = sum(len(buckets[k]) for k in todo)
        if ph:
            print(f"[FAIL] 中英模板的占位符数不一致：{len(ph)} 处"
                  f"（界面上会原样留下一个 {{}}）")
            for p in ph[:15]:
                print(f"  {p}")
            return 1
        if n_todo:
            print(f"[FAIL] 仍有 {n_todo} 处通知正文未走结构化双语（ToastMsg）：")
            for k in todo:
                if not buckets[k]:
                    continue
                print(f"  —— {LABEL[k]}（{len(buckets[k])} 处）")
                for line, expr in buckets[k][:8]:
                    print(f"     main.rs:{line}  {expr[:76]}")
                if len(buckets[k]) > 8:
                    print(f"     …… 另有 {len(buckets[k]) - 8} 处")
            return 1
        print("[ OK ] 通知正文全部为结构化双语（ToastMsg / from_external）")
        return 0

    # 默认：打印分类清单（Markdown 表，供审计与阶段 2.2 的改造清单）
    print(f"# 通知正文（push_toast）双语审计 · {TARGET.relative_to(ROOT)}")
    print()
    print(f"生产调用点 **{len(sites)}** 个 —— 已双语 **{len(buckets['ok'])}** · {summary}")
    print()
    print("| 行号 | 分类 | 正文表达式 |")
    print("|---|---|---|")
    for kind in sorted(LABEL, key=lambda k: (k in DONE, k)):
        for line, expr in buckets[kind]:
            esc = expr.replace("|", "\\|")
            print(f"| {line} | {LABEL[kind]} | `{esc[:110]}` |")
    if len(sites) < MIN_SITES:
        print()
        print(f"[WARN] 扫描覆盖面不足（{len(sites)} < {MIN_SITES}）—— 这个清单不可当证据")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
