"""基线门禁：仓库里的**基线**必须有人比对，而且那个比对必须在门禁里。

## 为什么（附录 GZ）

`assets/ui-archive/` 漂了 79% 而没人知道 —— 因为没有任何东西在比对它。
扫了一遍发现当时所有真正的基线都已接好，但**那是"这一刻"的结论**：
下次有人加一份基线（golden 文件、快照、期望值）却忘了接比对，就重演。

## 判据

对 `BASELINES` 里每一项：必须存在某个 `scripts/*.py` **同时**满足
① 引用这个文件/目录名；② **被 `gates.py` 引用**（也就是它会在门禁里跑）。
两条缺一就判红，并说明缺哪条。

**注意区分**：`docs/证据/*.png` 那类**证据图**是历史记录，
**按设计没有比对对象**，不属于基线 —— 不要往 BASELINES 里塞。
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 基线清单：这些文件/目录是"应该长这样"的**期望值**。
# 加新基线时，**同时**把"谁比对它"接进门禁 —— 否则它只是躺着。
# (基线路径, **比对它的脚本**, 说明)
#
# **必须显式指名"谁比对"**，不能只查"有没有脚本提到它"（附录 GZ）：
# `gen-site.py` 也提到 `ui-archive`，但它是把图当**站点素材**用，不是比对 ——
# 只查"提到"的话，把真正的比对从门禁里去掉，检查照样通过（正向对照当场失败）。
BASELINES = [
    ("docs/证据/shot-baseline.json", "shot-diff.py", "像素回归基线"),
    ("docs/证据/gate-count.txt", "doc-numbers-check.py", "门禁数（数字门禁读它）"),
    ("site/release.json", "gen-site.py", "已发布产物的权威事实"),
    ("assets/ui-archive", "archive-ui-shots.py", "归档走查图（逐像素回归）"),
]


def main() -> int:
    gates = (ROOT / "scripts" / "gates.py").read_text(encoding="utf-8")
    scripts = sorted((ROOT / "scripts").glob("*.py"))
    bad = 0
    for rel, comparer, why in BASELINES:
        if not (ROOT / rel).exists():
            print(f"[FAIL] {rel}：清单里写了，但文件不存在（{why}）")
            bad += 1
            continue
        src = ROOT / "scripts" / comparer
        if not src.exists():
            print(f"[FAIL] {rel}（{why}）：清单指的比对脚本 {comparer} 不存在")
            bad += 1
            continue
        body = src.read_text(encoding="utf-8", errors="ignore")
        if rel.split("/")[-1] not in body and rel not in body:
            print(f"[FAIL] {rel}（{why}）：{comparer} 里**看不到它** —— 清单与代码脱节")
            bad += 1
            continue
        if comparer not in gates:
            print(f"[FAIL] {rel}（{why}）：比对脚本 {comparer} **不在门禁里** —— "
                  f"这个比对永远不会跑")
            bad += 1
            continue
        print(f"[ OK ] {rel}（{why}）← {comparer} [门禁]")
    if bad:
        print(f"\n基线门禁未通过：{bad}/{len(BASELINES)} 项")
        return 1
    print(f"\n基线门禁通过：{len(BASELINES)} 项基线都有门禁内的比对")
    return 0


if __name__ == "__main__":
    sys.exit(main())
