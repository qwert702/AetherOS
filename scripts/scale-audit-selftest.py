"""FF-1：给 scale-audit 写常驻正向对照 scripts/scale-audit-selftest.py。

## 为什么

缺口清单里背书的「6 处缩放写死」是这个扫描器抓的，但**没人证明它真会响**。
按附录 FD/FE 的规矩：**判据要能真的响**。

## 过程里的发现（如实记）

第一次对照把违规片段写进**临时 .rs 文件** → 扫描器报「0 处」。
隔离后查明：**①②③④ 四类规则只读写死的 `draw.rs`**（后面的 glob 是 ⑤⑥⑦ 用的）——
**是我的对照放错了地方**，不是扫描器坏了。
顺带给扫描器加了 `--file <路径>`：既让对照可行（**不碰产品源码**），也补上这个覆盖边界。

## 本脚本做什么

1. 默认（`draw.rs`）应当**干净**（退出码 0）；
2. 造一个临时 .rs（含 ③ 的违规：`x: other.x + 40`）→ `--file` 指向它 → 必须**判红并点名**；
3. `finally` 删除临时文件，并再次确认默认运行**干净**。
"""
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(r'D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether')
TMP = ROOT / "aether-compositor/src/zz_scale_audit_selftest.rs"

# ③「矩形.y/x + 裸数字（>=20）」的违规写法；同时含 ④（`base + i * 40`）
VIOLATION = """fn selftest_violations(other: Rect, base: i32, i: i32) {
    let r = Rect { x: other.x + 40, y: 0, w: 10, h: 10 };
    let y = base + i * 40;
}
"""


def run(*extra):
    r = subprocess.run([sys.executable, "scripts/scale-audit.py", "--strict", *extra],
                       cwd=ROOT, capture_output=True, text=True, encoding='utf-8')
    return r.returncode, r.stdout


code0, out0 = run()
print(f"① 默认（draw.rs）：退出码 {code0}（应为 0）")
if code0 != 0:
    print(out0[-800:])
    sys.exit(1)

try:
    TMP.write_text(VIOLATION, encoding='utf-8')
    print(f"② 已写临时违规文件 {TMP.name}")
    code1, out1 = run(str(TMP))
    named = TMP.name in out1
    caught3 = "③ 「矩形.y/x + 裸数字」" in out1 and "：1 处" in out1
    print(f"   退出码 {code1}（应为 1）；点名文件：{named}；③ 计到 1 处：{caught3}")
    if not (code1 == 1 and named):
        print(out1[-800:])
        sys.exit(1)
finally:
    if TMP.exists():
        TMP.unlink()
        print("③ 临时文件已删除")

code2, out2 = run()
print(f"④ 清理后默认运行：退出码 {code2}（应为 0）")
if code2 != 0:
    print(out2[-800:])
    sys.exit(1)

print("[ OK ] scale-audit 正向对照通过：注入判红并点名文件，清理后放行")
sys.exit(0)
