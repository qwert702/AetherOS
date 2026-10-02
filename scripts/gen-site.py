#!/usr/bin/env python3
"""AetherOS 官网生成器：把"会变的东西"从仓库真实数据生成，杜绝口径漂移。

## 为什么必须有生成器

站点上的版本号、ISO 体积、代码行数、测试数**每周都在变**。手写必然漂移 ——
本仓库已经吃过这个亏：`INDEX.md` 曾落后 4,639 行、14 个文件，`ui-design-handover.md`
描述的还是重做前的视觉。所以页面里**只写占位符**，值由本脚本注入：

    <!--DATA:iso_size-->        →  38.5 MB
    <!--DATA:rust_lines-->      →  23,694
    <!--DATA:tests-->           →  356

`--check` 会重新渲染一遍并与磁盘逐字节比对，不一致即非零退出（进现有门禁）。

## 用法

    python scripts/gen-site.py --refresh     # 重新测量真实数据 + 生成图片 + 渲染页面
    python scripts/gen-site.py               # 用 site/data.json 渲染页面
    python scripts/gen-site.py --check       # 只校验页面与数据一致（门禁用）
    python scripts/gen-site.py --refresh --skip-tests   # 迭代时跳过 cargo test（约 60 秒）

## 生成物（不提交，见 .gitignore）

    site/data.json            实测数据（版本/体积/行数/测试数/ISO 校验和）
    site/assets/img/*.webp    从 docs/ 的走查图转换而来（体积小、加载快）
    site/assets/img/og-cover.png  1200×630 社交预览图（分享到微信/Twitter 时的门面）
    site/sitemap.xml  site/robots.txt  site/404.html
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
DATA = SITE / "data.json"
IMG = SITE / "assets" / "img"
ISO = ROOT / "aetheros-0.1-amd64.iso"
#: 已发布产物的权威事实（人工维护）。存在时**优先于**本地 ISO —— 官网要描述的是
#: 用户实际下载到的文件。见文件内 `_comment` 与 `collect()` 里的说明。
RELEASE = SITE / "release.json"
CHANGELOG = ROOT / "CHANGELOG.md"

#: 从 docs/ 走查图转成站点图片（源 → 目标名）。源是仓库里已有的视觉基线，
#: 不复制进 site/ 以免二进制文件重复入库。
IMAGE_SOURCES = {
    "host-ui-light-desktop.png": "desktop",
    "host-ui-light-desktop-clean.png": "desktop-clean",
    "host-ui-light-settings.png": "settings",
    "host-ui-light-control-center.png": "control-center",
    "host-ui-light-ime.png": "ime",
    "host-ui-light-installer.png": "installer",
    "screenshot-htop-app.png": "htop-app",
}
GIF_SOURCE = "demo-htop.gif"

#: 页面清单（生成 sitemap 用）。zh 在根，en 在 /en/。
PAGES = [
    ("index.html", "1.0", "weekly"),
    ("download.html", "0.9", "weekly"),
    ("changelog.html", "0.8", "weekly"),
    ("faq.html", "0.7", "monthly"),
    ("en/index.html", "0.9", "weekly"),
    ("en/download.html", "0.8", "weekly"),
    ("en/changelog.html", "0.7", "weekly"),
    ("en/faq.html", "0.6", "monthly"),
]


def run(args: list[str]) -> str:
    r = subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace")
    return r.stdout or ""


def count_archived_shots() -> int:
    """数 `scripts/archive-ui-shots.py` 的 `SHOTS` 清单条数。

    **口径是"被逐像素门禁覆盖的张数"，不是 `docs/` 目录里的文件数。**
    两者会不一致（当前目录 20 张、清单 19 张，多出的 `host-ui-terminal-detail.png`
    是没进清单的孤儿图），用文件数会宣称"比实际被门禁保护的更多" —— 那正是
    这个项目最忌讳的"看起来做了 ≠ 真的做了"（2026-10-02 安全审计）。
    """
    path = ROOT / "scripts" / "archive-ui-shots.py"
    if not path.is_file():
        return 0
    n = len(
        re.findall(r'^\s*\(\[.*?\],\s*"host-ui-[^"]+"\),\s*$', path.read_text(encoding="utf-8"), re.M)
    )
    if n == 0:
        # 命中 0 说明清单格式变了 —— **必须报错**，不能静默沿用上次值
        # （代码审查发现：静默回退会让"页面 + data.json 都不变 → 门禁全绿"，
        #  而站点上的张数从此永久脱节。同文件 load_release() 对格式坏就是报错的。）
        sys.exit("无法从 scripts/archive-ui-shots.py 数出走查图清单条数（格式变了？）")
    return n


def load_release() -> dict | None:
    """读 `site/release.json`（已发布产物的权威事实）。

    不存在 → `None`（回退到"用本地构建产物"，兼容没有发布过的仓库）。
    存在但**格式坏** → 直接报错退出，不静默回退：静默回退会让站点继续公布
    错误的校验和，而那正是这次要修的问题（2026-10-02 安全审计）。
    """
    if not RELEASE.is_file():
        return None
    try:
        data = json.loads(RELEASE.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise SystemExit(f"[gen-site] {RELEASE} 不是合法 JSON：{exc}")
    if not isinstance(data, dict):
        raise SystemExit(f"[gen-site] {RELEASE} 顶层必须是对象")
    return data


def measure(skip_tests: bool, previous: dict) -> dict:
    """从仓库实测出所有对外数字。"""
    d: dict = {}

    # 版本号：最近的 tag（没有 tag 就退回 v0.1.0，与 GitHub Release 一致）
    tag = run(["git", "describe", "--tags", "--abbrev=0"]).strip()
    d["version"] = tag or "v0.1.0"

    # 代码规模：口径唯一来源是 scripts/repo-stats.py（它自己带 --check 门禁）
    stats = run([sys.executable, str(ROOT / "scripts" / "repo-stats.py")])
    m = re.search(r"\|\s*\*\*合计\*\*\s*\|\s*\*\*([\d,]+)\*\*\s*\|\s*\*\*(\d+)\*\*", stats)
    if not m:
        sys.exit("无法从 scripts/repo-stats.py 解析规模（口径源变了？）")
    d["rust_lines"] = m.group(1)
    d["rust_files"] = m.group(2)
    d["rust_lines_num"] = int(m.group(1).replace(",", ""))
    d["crates"] = len(list(ROOT.glob("*/Cargo.toml")))

    # 测试数：跑一遍工作区测试（慢，所以 --skip-tests 时沿用上次的值）
    if skip_tests:
        d["tests"] = previous.get("tests", "—")
    else:
        out = run(["cargo", "test", "--workspace", "--offline", "--no-fail-fast"])
        total = sum(int(x) for x in re.findall(r"test result: ok\. (\d+) passed", out))
        d["tests"] = str(total) if total else previous.get("tests", "—")

    # —— ISO 事实：已发布产物优先 ——
    #
    # 为什么不是"永远用仓库根那份 ISO"：官网要回答的是"用户下载到的文件是什么"。
    # 二者曾经不一致 —— Release 资产 40,327,168 字节 / sha256 7c50f481…，
    # 本地工作区 ISO 40,359,936 字节 / sha256 43c53f4c…，于是下载页让人拿一个
    # **对不上的哈希**去校验自己下载的文件（2026-10-02 安全审计发现）。
    # 发布流程：重建 ISO → 上传 Release → 更新 site/release.json → --refresh。
    rel = load_release()
    if rel is not None:
        for k in ("iso_bytes", "iso_size", "iso_sha256", "iso_date"):
            if k in rel:
                d[k] = rel[k]
        d["release_tag"] = rel.get("tag", "")
        d["release_asset"] = rel.get("asset", "")
    elif ISO.exists():
        size = ISO.stat().st_size
        d["iso_bytes"] = size
        d["iso_size"] = f"{size / 1048576:.1f} MB"
        h = hashlib.sha256()
        with ISO.open("rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        d["iso_sha256"] = h.hexdigest()
        d["iso_date"] = subprocess.run(
            ["git", "log", "-1", "--format=%ad", "--date=short"], capture_output=True,
            text=True, encoding="utf-8").stdout.strip()
    else:
        # 既没有 release.json 也没有本地构建产物时沿用上次实测值，绝不留空
        # （页面不能出现 "— MB"）
        for k in ("iso_bytes", "iso_size", "iso_sha256", "iso_date"):
            d[k] = previous.get(k, "—")

    # 走查图：以**视觉门禁清单**为准，不是目录文件数（见 count_archived_shots 的说明）
    shots = count_archived_shots()
    d["shots"] = str(shots) if shots else previous.get("shots", "—")
    d["repo"] = "github.com/qwert702/AetherOS"
    d["site"] = "aether.cbnac.com"
    d["generated"] = subprocess.run(["git", "log", "-1", "--format=%ad", "--date=short"],
                                    capture_output=True, text=True, encoding="utf-8").stdout.strip()
    return d


# —— Markdown → HTML（只支持 CHANGELOG.md 用到的那一小撮语法）——

#: 允许的协议。**白名单**而不是黑名单：漏掉一个危险协议
#: （`javascript:` / `data:` / `vbscript:`）就等于把 XSS 写进公开站点。
_SAFE_SCHEME = re.compile(r"^(?:https?|mailto):", re.I)
#: 有 scheme 的判据（`xxx:` 形式）。没有 scheme 的就是相对路径（`docs/x.md`、`/x`、`#x`），放行。
_HAS_SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.\-]*:")


def _safe_href(url: str) -> str | None:
    """把 Markdown 链接目标转成可安全放进 `href` 的字符串；不安全返回 None。

    2026-10-02 审计 L-10：此前是 `re.sub(..., r'<a href="\\2">')` 直接拼 ——
    `[x](javascript:alert(1))` 会原样变成可点击的 JS 链接；URL 里带一个 `"`
    还能提前闭合属性注入任意属性（与 M-10 的 CSP 缺失叠加即可执行）。
    站点内容来自仓库里的 Markdown（CHANGELOG / 更新日志），门槛不高，
    但"发布出去的页面"不该依赖上游作者的自觉。
    """
    u = url.strip()
    if not u:
        return None
    if _HAS_SCHEME.match(u) and not _SAFE_SCHEME.match(u):
        return None  # 只允许 http/https/mailto；其余（javascript/data/vbscript…）一律拒
    # 引号/尖括号/控制字符一律拒绝：它们能闭合属性或改变解析
    if any(c in u for c in ('"', "'", "<", ">", "`")) or any(ord(c) < 0x20 for c in u):
        return None
    return u


def inline(s: str) -> str:
    s = s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

    def _link(m: re.Match) -> str:
        text, url = m.group(1), m.group(2)
        href = _safe_href(url)
        if href is None:
            # 不安全就把链接**降级成纯文本**（保留内容，去掉可点击性）
            return f"{text}（链接已移除：{url}）"
        return f'<a href="{href}">{text}</a>'

    s = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", _link, s)
    s = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"(?<!\*)\*([^*]+?)\*(?!\*)", r"<em>\1</em>", s)
    s = re.sub(r"`([^`]+?)`", r"<code>\1</code>", s)
    return s


def render_changelog(md: str) -> str:
    """把 Markdown 渲染成页面正文：h2 版本 / h3 分组 / ul 条目 / **表格** / 行内格式。

    每日更新日志里用了表格（例如"三页一览"那一节），所以表格必须支持 ——
    否则站点上会直接显示 `| 页 | 数据来源 | 是否新增 IPC |` 这样的原始文本。
    """
    out: list[str] = []
    in_list = False
    in_table = False

    def close_list() -> None:
        nonlocal in_list, in_table
        if in_list:
            out.append("</ul>")
            in_list = False
        if in_table:
            out.append("</tbody></table>")
            in_table = False

    for raw in md.splitlines():
        line = raw.rstrip()
        if line.startswith("> "):
            continue  # 文件头的说明只给维护者看
        if not line.strip():
            continue
        if line.startswith("---"):
            close_list()
            continue
        if line.startswith("## "):
            close_list()
            title = inline(line[3:])
            out.append(f'<h2 id="{re.sub(r"[^a-z0-9]+", "-", line[3:].lower()).strip("-")}">{title}</h2>')
            continue
        if line.startswith("### "):
            close_list()
            out.append(f"<h3>{inline(line[4:])}</h3>")
            continue
        if line.startswith("|"):
            cells = [c.strip() for c in line.strip("|").split("|")]
            if all(set(c) <= set("-: ") for c in cells):  # |---|---| 分隔行
                continue
            if in_list:
                out.append("</ul>")
                in_list = False
            if not in_table:
                out.append("<table><tbody>")
                in_table = True
            out.append("  <tr>" + "".join(f"<td>{inline(c)}</td>" for c in cells) + "</tr>")
            continue
        if line.startswith("- "):
            if in_table:
                out.append("</tbody></table>")
                in_table = False
            if not in_list:
                out.append("<ul>")
                in_list = True
            out.append(f"  <li>{inline(line[2:])}</li>")
            continue
        close_list()
        out.append(f"<p>{inline(line)}</p>")
    close_list()
    return "\n".join(out)


# —— 图片：走查图 → WebP + 社交预览图 ——

def build_images(d: dict) -> None:
    try:
        from PIL import Image, ImageDraw, ImageFont
    except ImportError:
        print("  警告：没有 Pillow，跳过图片生成（页面会引用不存在的图片）")
        return
    IMG.mkdir(parents=True, exist_ok=True)
    for src, dst in IMAGE_SOURCES.items():
        p = ROOT / "docs" / src
        if not p.exists():
            print(f"  警告：缺图 {src}")
            continue
        im = Image.open(p).convert("RGB")
        im.save(IMG / f"{dst}.webp", "WEBP", quality=86, method=5)
    gif = ROOT / "docs" / GIF_SOURCE
    if gif.exists():
        shutil.copy2(gif, IMG / GIF_SOURCE)
    make_og_cover(d, Image, ImageDraw, ImageFont)


def _font(ImageFont, size: int):
    """找一个带中文的字体；找不到就返回 None（封面退化为纯图，不报错）。"""
    for cand in (
        "C:/Windows/Fonts/msyhbd.ttc", "C:/Windows/Fonts/msyh.ttc",
        "/usr/share/fonts/truetype/aether/NotoSansSC-Bold.otf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    ):
        if Path(cand).exists():
            try:
                return ImageFont.truetype(cand, size)
            except OSError:
                continue
    return None


def make_og_cover(d: dict, Image, ImageDraw, ImageFont) -> None:
    """1200×630 社交预览图：分享到微信/Twitter/Reddit 时决定有没有人点。

    配色与产品一致：纯白底、单一 teal、1px 描边、无渐变。
    """
    W, H = 1200, 630
    bg, fg, dim, teal, line = (255, 255, 255), (24, 26, 30), (110, 118, 130), (11, 132, 150), (228, 230, 234)
    im = Image.new("RGB", (W, H), bg)
    dr = ImageDraw.Draw(im)
    dr.rectangle([0, 0, W - 1, H - 1], outline=line)
    dr.rectangle([0, 0, 8, H], fill=teal)  # 左侧 teal 细条 = 品牌位

    f_title = _font(ImageFont, 74)
    f_sub = _font(ImageFont, 30)
    f_small = _font(ImageFont, 24)
    dr.text((64, 74), "AetherOS", font=f_title, fill=fg)
    dr.text((66, 168), "从 Linux 内核向上，用户态全部自研", font=f_sub, fill=dim)
    dr.text((66, 214), "An operating system with a userspace written from scratch",
            font=f_small, fill=dim)

    # 关键数字（真实注入，不是写死的）
    nums = [
        (d.get("iso_size", "—"), "ISO"),
        (d.get("rust_lines", "—"), "行 Rust"),
        (d.get("tests", "—"), "项测试"),
        (d.get("crates", "—"), "个 crate"),
    ]
    x = 66
    for value, label in nums:
        dr.text((x, 300), str(value), font=f_sub, fill=teal)
        dr.text((x, 342), label, font=f_small, fill=dim)
        x += 230

    # 桌面实拍（白色主题）压进右下角卡片
    shot = IMG / "desktop.webp"
    if shot.exists():
        s = Image.open(shot).convert("RGB")
        sw = 520
        sh = int(s.height * sw / s.width)
        s = s.resize((sw, sh), Image.LANCZOS)
        px, py = W - sw - 64, H - sh - 64
        dr.rectangle([px - 1, py - 1, px + sw, py + sh], fill=bg, outline=line)
        im.paste(s, (px, py))
    im.save(IMG / "og-cover.png", "PNG", optimize=True)
    print("  og-cover.png 1200x630")


# —— 渲染与校验 ——

def data_placeholders(data: dict) -> dict[str, str]:
    return {k: str(v) for k, v in data.items()}


def patch_values(text: str, old: dict, new: dict) -> str:
    """把页面上"上次注入的值"替换成本次的值。

    为什么需要它：占位符 `<!--DATA:key-->` **只在第一次渲染时存在**，渲染后就被具体值取代了。
    所以后续更新不能靠占位符，只能靠"上次的值 → 本次的值"。
    上次的值存在 `site/data.json`（**已入库**，所以新克隆也能正确打补丁）。

    数字类一律加词边界，避免 `356` 命中 `13560` 这种误伤。
    """
    def num(key: str, pattern: str) -> None:
        nonlocal text
        o, n = str(old.get(key, "")), str(new.get(key, ""))
        if o and n and o != n:
            # **替换串只放新值**，并且用 lambda —— 不能用字符串替换串：
            # 搜索模式里带环视 `(?<!…)`/`\d`，若整条模式也塞进替换串，
            # `re.sub` 会把替换串当**模板**再解析一次，`\d` 触发
            # `re.error: bad escape \d` 直接抛异常 → 渲染中断。
            # 更隐蔽的是旧代码**先写 data.json 再渲染**：崩了之后 data.json 已是新值，
            # 下次 `o == n` 连替换都不尝试 → 页面**静默**永远停在旧值。
            text = re.sub(pattern.replace("%OLD%", re.escape(o)), lambda _m: n, text)

    for key in ("rust_lines", "tests", "iso_bytes"):
        num(key, r"(?<![\d,])%OLD%(?![\d,])")
    num("iso_sha256", r"\b%OLD%\b")

    # 带上下文的（`16` 这种裸数字直接替换太危险，必须靠上下文锚定）
    o, n = str(old.get("rust_files", "")), str(new.get("rust_files", ""))
    if o and n and o != n:
        text = re.sub(rf"{re.escape(o)}(?=\s*(?:个源文件|source files))", n, text)
    o, n = str(old.get("shots", "")), str(new.get("shots", ""))
    if o and n and o != n:
        text = re.sub(rf"{re.escape(o)}(?=\s*(?:张界面走查图|UI screenshots))", n, text)

    for key in ("iso_size", "version"):
        o, n = str(old.get(key, "")), str(new.get(key, ""))
        if o and n and o != n:
            text = text.replace(o, n)
    return text


def render_page(path: Path, data: dict, changelog_html: str, previous: dict) -> str:
    text = path.read_text(encoding="utf-8")
    for k, v in data_placeholders(data).items():
        text = text.replace(f"<!--DATA:{k}-->", v)  # 首次渲染（模板里还有占位符）
    # changelog 页**不参与数值补丁**：它的正文是每次从 `更新日志/*.md` 整体重算的，
    # 里面会出现**历史数字**（当时的行数/测试数）。套用"上次值 → 本次值"会把历史文字也改掉，
    # 于是"写盘结果"与"下次重算结果"来回不一致 —— 门禁会反复红/绿，且历史记录被污染。
    if path.name != "changelog.html":
        text = patch_values(text, previous, data)  # 之后靠"上次值 → 本次值"
    # 更新日志页的正文完全由 更新日志/ 与 CHANGELOG.md 决定 —— 每次整体重算
    if path.name == "changelog.html":
        text = re.sub(
            r'(<div class="log">).*?(</div>\s*</div>\s*</section>)',
            lambda m: m.group(1) + "\n" + changelog_html + "\n  " + m.group(2),
            text, flags=re.S)
    text = text.replace("<!--CHANGELOG-->", changelog_html)
    return text


def html_pages() -> list[Path]:
    return sorted(p for p in SITE.rglob("*.html") if p.is_file())


def write_generated(data: dict) -> None:
    """sitemap / robots / 404 —— 形式固定，直接生成，避免手写漂移。"""
    base = "https://" + data["site"]
    urls = []
    for rel, prio, freq in PAGES:
        alt = ""
        if rel.startswith("en/"):
            alt = f'\n    <xhtml:link rel="alternate" hreflang="zh-CN" href="{base}/{rel[3:]}"/>' \
                  f'\n    <xhtml:link rel="alternate" hreflang="en" href="{base}/{rel}"/>'
        else:
            alt = f'\n    <xhtml:link rel="alternate" hreflang="zh-CN" href="{base}/{rel}"/>' \
                  f'\n    <xhtml:link rel="alternate" hreflang="en" href="{base}/en/{rel}"/>'
        urls.append(f'  <url>\n    <loc>{base}/{rel}</loc>\n    <priority>{prio}</priority>'
                    f'\n    <changefreq>{freq}</changefreq>{alt}\n  </url>')
    (SITE / "sitemap.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"\n'
        '        xmlns:xhtml="http://www.w3.org/1999/xhtml">\n'
        + "\n".join(urls) + "\n</urlset>\n", encoding="utf-8", newline="\n")
    (SITE / "robots.txt").write_text(
        f"User-agent: *\nAllow: /\n\nSitemap: {base}/sitemap.xml\n", encoding="utf-8", newline="\n")
    (SITE / "404.html").write_text(
        f"""<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>页面不存在 · AetherOS</title>
<meta name="robots" content="noindex">
<link rel="icon" href="/assets/favicon.svg">
<link rel="stylesheet" href="/assets/app.css">
</head><body class="wrap narrow">
<header class="hero"><h1>页面不存在</h1>
<p class="lede">这个地址没有内容。<a href="/">回到首页</a> 或 <a href="/en/">English</a>。</p></header>
<footer class="foot"><p>AetherOS · GPL-3.0-only · <a href="/changelog.html">更新日志</a></p></footer>
</body></html>
""", encoding="utf-8", newline="\n")
    print("  sitemap.xml / robots.txt / 404.html")


def build_changelog() -> str:
    """站点更新日志页的正文 = **每日更新日志**（新→旧）+ 版本发布说明（放最后 = 最早）。

    单一来源：日常改动只写 `更新日志/YYYY-MM-DD.md`（仓库既定约定，面向"你能看到的变化"），
    版本级说明只写 `CHANGELOG.md`。两者都**不必为站点再维护一份** —— 这是这个生成器存在的理由。
    """
    parts: list[str] = []
    daily = sorted((p for p in (ROOT / "更新日志").glob("*.md") if p.name != "README.md"),
                   reverse=True)
    for p in daily:
        parts.append(render_changelog(p.read_text(encoding="utf-8")))
    if CHANGELOG.exists():
        parts.append(render_changelog(CHANGELOG.read_text(encoding="utf-8")))
    if not parts:
        sys.exit("既没有 更新日志/*.md 也没有 CHANGELOG.md —— 更新日志页没有来源")
    return "\n".join(parts)


def selftest() -> int:
    """`--selftest`：链接白名单等纯函数自检。不读网络、不写文件、不碰 site/。

    只为让"危险链接被降级"这条规则有地方**被真的执行一次** ——
    此前它只存在于 `--check` 的渲染结果里，而渲染结果里没有危险链接，
    等于这条规则从未被验证过。
    """
    ok = 0
    bad: list[str] = []

    def check(cond: bool, msg: str) -> None:
        nonlocal ok
        if cond:
            ok += 1
        else:
            bad.append(msg)

    # L-10：危险协议/可疑字符必须被降级成纯文本
    for evil in (
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        "data:text/html,<script>x</script>",
        "vbscript:msgbox(1)",
        'https://a/b"onload="alert(1)',
        "https://a/b'c",
    ):
        html = inline(f"[点我]({evil})")
        check("<a href" not in html, f"危险链接未被降级：{evil!r} → {html}")

    # 正常链接不能被误杀
    for good in ("https://example.com/x", "http://a/b", "/download.html", "mailto:a@b.c", "#sec"):
        html = inline(f"[x]({good})")
        check(f'<a href="{good}">' in html, f"正常链接被误杀：{good!r} → {html}")

    # 行内代码里的链接语法不该被当成链接处理（保持既有行为）
    check("<code>" in inline("`[x](javascript:alert(1))`"), "行内代码应保留为 code")

    print(f"[selftest] 通过 {ok} 项")
    for m in bad:
        print(f"  ✗ {m}", file=sys.stderr)
    return 0 if not bad else 1


def main(argv: list[str]) -> int:
    if "--selftest" in argv:
        return selftest()
    refresh = "--refresh" in argv
    check = "--check" in argv
    skip_tests = "--skip-tests" in argv
    previous = json.loads(DATA.read_text(encoding="utf-8")) if DATA.exists() else {}

    data = measure(skip_tests, previous) if refresh else previous
    if not data:
        sys.exit("没有 site/data.json —— 先跑 python scripts/gen-site.py --refresh")

    if refresh:
        # **不在这里写 data.json**：页面渲染在下面（第 428 行起），而补丁靠
        # "上次值（data.json）→ 本次值"。若先落盘再渲染，一旦渲染这步失败
        # （典型：解释器缺 Pillow，图片与页面渲染被跳过），data.json 记的是新值、
        # 页面还是旧值，下次运行"上次值 → 本次值"就恒等，补丁**永不触发** ——
        # 站点从此再也更新不了，而且看不出原因。改为渲染+自检通过后再落盘（见函数末尾）。
        build_images(data)
        write_generated(data)

    changelog_html = build_changelog()

    drift: list[str] = []
    for page in html_pages():
        want = render_page(page, data, changelog_html, previous)
        have = page.read_text(encoding="utf-8")
        if want != have:
            drift.append(str(page.relative_to(ROOT)))
            if not check:
                page.write_text(want, encoding="utf-8", newline="\n")
                print("  渲染", page.relative_to(ROOT))

    # —— refresh 路径：渲染完成后**先自检、再落盘 data.json** ——
    #
    # 自检判据与 `--check` 的"真门禁"一致：本次的值必须真的出现在页面上。
    # 不通过就**不写 data.json**，从而保住"上次值 → 本次值"的锚点，下次运行还能把补丁打上。
    # 这样即使某次渲染失败（缺 Pillow 等），站点也不会陷入"再也更新不了"的状态。
    if refresh and not check:
        idx = ROOT / "site" / "index.html"
        text = idx.read_text(encoding="utf-8") if idx.exists() else ""
        miss = [k for k in ("rust_lines", "tests") if str(data.get(k, "")) not in text]
        if miss:
            print("[FAIL] 页面没拿到本次值 %s —— **不写 data.json**（保住补丁锚点，可重跑修复）" % miss)
            return 1
        DATA.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n",
                        encoding="utf-8", newline="\n")
        print("  data.json:", " ".join(f"{k}={v}" for k, v in data.items()
                                      if k in ("version", "iso_size", "rust_lines", "tests")))

    if check:
        if drift:
            print("[FAIL] 站点与实测数据不一致（跑 python scripts/gen-site.py --refresh）:")
            for f in drift:
                print("   -", f)
            return 1
        # **--check 也要重新测量事实**（代码审查发现）：此前 check 模式直接用 data.json
        # 的旧值渲染并比对，"页面 == data.json"永远成立 —— 于是改了 release.json、
        # 换了本地 ISO、或代码规模变了而忘了 --refresh，门禁照样全绿，而线上公布的
        # 字节数/哈希/规模已经过期。这里重新测一次（跳过跑测试那一步，保持秒级），
        # 与 data.json 不一致就报出来。
        fresh = measure(skip_tests=True, previous=data)
        stale = [f"{k}: data.json={data.get(k)} 实测={fresh.get(k)}"
                 for k in ("iso_bytes", "iso_sha256", "rust_lines", "shots", "iso_size")
                 if data.get(k) != fresh.get(k)]
        if stale:
            print("[FAIL] data.json 里的发布事实已过期（跑 python scripts/gen-site.py --refresh）:")
            for s in stale:
                print("   -", s)
            return 1
        missing = [p for p in html_pages()
                   if "<!--DATA:" in p.read_text(encoding="utf-8")
                   or "<!--CHANGELOG-->" in p.read_text(encoding="utf-8")]
        if missing:
            print("[FAIL] 还有未替换的占位符：", ", ".join(str(p.relative_to(ROOT)) for p in missing))
            return 1
        # —— 真门禁：值必须真的出现在页面上 ——
        #
        # 只比较"渲染结果 vs 磁盘"是不够的：两者可能同样陈旧（曾经就因此让线上数字冻结了
        # 23,694 行 / 356 项测试，而 --check 一直"通过"）。所以这里直接断言当前值在页面里。
        need = {
            # shots 也纳入门禁：站点上那个数字曾是手写的 17，而实际门禁覆盖 19 张 ——
            # 因为 patch_values 只在"上次值 ≠ 本次值"时才替换，常量一旦与 data.json
            # 脱节就永远补不上（2026-10-02 审计，与 iso_bytes 同一类问题）。
            "index.html": ["rust_lines", "tests", "iso_size", "shots"],
            "en/index.html": ["rust_lines", "tests", "iso_size", "shots"],
            # iso_bytes/iso_sha256 也纳入门禁：它的字面量曾长期停在 40,351,744
            # （与实测差 8,192 字节）而没人发现；哈希则一度是本地产物那份
            # （2026-10-02 安全审计 §3.2/§3.3）。
            "download.html": ["iso_size", "iso_bytes", "iso_sha256"],
            "en/download.html": ["iso_size", "iso_bytes", "iso_sha256"],
            # faq 页也写着 ISO 体积，此前不在门禁范围内（代码审查发现 4.8）
            "faq.html": ["iso_size"],
            "en/faq.html": ["iso_size"],
        }
        # **带上下文的锚定匹配**（代码审查发现）：原先只判"这个值出现在页面任意位置" ——
        # 于是把"19 张走查图"改成 10 张、只要页面别处还有个 19 就能过。
        # 这里给每个键规定它必须紧邻的量词；数字与量词之间允许有 HTML 标签
        # （页面里常见 `<b>25,750</b><span>行 Rust…</span>` 这种写法）。
        between = r"(?:\s|<[^>]*>|&nbsp;)*"
        anchor = {
            # 中英两版页面各用各的量词，两种都接受（锚定只为了防止"数字出现在别处"）
            "rust_lines": between + r"(?:行|lines)",
            "tests": between + r"(?:项|unit tests|tests)",
            "shots": between + r"(?:张|UI screenshots|screenshots|images)",
            "iso_size": r"",
            "iso_bytes": between + r"(?:字节|bytes)",
            "iso_sha256": r"",
        }
        for rel, keys in need.items():
            body = (SITE / rel).read_text(encoding="utf-8")
            for k in keys:
                want_v = str(data.get(k, "—"))
                if want_v == "—":
                    continue
                pat = re.escape(want_v) + anchor.get(k, "")
                if not re.search(pat, body):
                    print(f"[FAIL] {rel} 里找不到当前 {k}={want_v}（页面与数据脱节，"
                          f"期望形如 /{pat}/）")
                    return 1

        # —— 禁止站点上出现"非已发布产物"的 sha256 ——
        #
        # 为什么（2026-10-02 审计自查）：下载页与更新日志都曾公布**本地构建产物**的
        # 哈希（43c53f4c…），而用户下载的是 Release 资产（7c50f481…）—— 让人拿一个
        # 必然校验失败的值去校验，比不校验更糟（他会以为文件坏了）。
        # 逐页扫 64 位十六进制串，只允许等于 release.json 里那个值。
        published = str(data.get("iso_sha256", ""))
        foreign: list[tuple[str, str]] = []
        for page in html_pages():
            body = page.read_text(encoding="utf-8")
            for h in sorted(set(re.findall(r"\b[0-9a-f]{64}\b", body))):
                if h != published:
                    foreign.append((str(page.relative_to(ROOT)), h))
        if foreign:
            print("[FAIL] 站点上出现了非已发布产物的 sha256（会让用户校验失败）：")
            for name, h in foreign:
                print(f"   - {name}: {h}")
            print("     期望值来自 site/release.json；历史条目请改写成「本地构建产物」并注明，不要只删数字")
            return 1

        # —— 下载脚本内置的哈希必须与已发布产物一致 ——
        #
        # 为什么（代码审查发现 5.4）：同一份 SHA256 在 4 处各写一遍
        # （try-aether.sh / try-aether.ps1 / release.json / data.json），换版本时漏改一处
        # 就会**拒收用户的合法下载**。脚本是 shell/PowerShell，没法 import，只能读源码。
        if published:
            for rel in ("scripts/try-aether.sh", "scripts/try-aether.ps1"):
                text = (ROOT / rel).read_text(encoding="utf-8")
                found = set(re.findall(r"\b[0-9a-f]{64}\b", text))
                if not found:
                    print(f"[FAIL] {rel} 里找不到 SHA256 常量（下载校验被移除了？）")
                    return 1
                wrong = sorted(found - {published})
                if wrong:
                    print(f"[FAIL] {rel} 内置的 SHA256 与已发布产物不一致：{wrong}")
                    print(f"     已发布：{published}（site/release.json）—— 用户校验会失败")
                    return 1

        print(f"[ OK ] site: {len(html_pages())} 个页面与实测数据一致 · 下载脚本哈希与发布一致"
              f"（{data['version']} · ISO {data['iso_size']} · {data['rust_lines']} 行 · {data['tests']} 项测试）")
        return 0

    print(f"完成：{len(html_pages())} 个页面")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
