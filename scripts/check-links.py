"""Markdown 链接检查器：把仓库里所有 .md 的链接目标解析成真实路径，列出断链。

为什么需要它：这次整理把文件移进了子目录，**相对链接会静默失效** ——
而仓库现有门禁只查"规模数字"，不查链接（这正是"门禁之外的东西会腐烂"）。
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(r"D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether")
SKIP = {"target", ".git", "node_modules", "site"}

LINK = re.compile(r"\]\(([^)\s]+)\)")
broken, total = [], 0

for p in sorted(ROOT.rglob("*.md")):
    rel = p.relative_to(ROOT)
    if any(x in SKIP for x in rel.parts):
        continue
    try:
        text = p.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        continue
    for m in LINK.finditer(text):
        target = m.group(1)
        if target.startswith(("http://", "https://", "mailto:", "#", "tel:")):
            continue
        path_part = target.split("#", 1)[0]
        if not path_part:
            continue
        total += 1
        resolved = (p.parent / path_part).resolve()
        if not resolved.exists():
            line = text[: m.start()].count("\n") + 1
            broken.append((str(rel), line, target))

print(f"检查 {total} 条本地链接，断链 {len(broken)} 条")
for f, line, tgt in broken:
    print(f"  {f}:{line}  →  {tgt}")
sys.exit(1 if broken else 0)
