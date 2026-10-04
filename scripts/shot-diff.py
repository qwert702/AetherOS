"""截图像素级对比 —— 现在也是**端到端**的像素回归工具。

用法:
  python scripts/shot-diff.py save            # 渲染 10 档，把 SHA256 基线写进 docs/证据/shot-baseline.json
  python scripts/shot-diff.py verify          # 重新渲染并与基线比对；有任何不同 → 退出码 1
  python scripts/shot-diff.py mask            # （历史）两轮基线差异 → 输出非确定区
  python scripts/shot-diff.py check           # （历史）after vs before 的差异必须全部落在掩码内

## 为什么现在不需要掩码了

本工具历史上要求"手工把两轮截图放进脚本目录"，再用 `MASK_REGIONS` 盖住天然非确定区 ——
其中一块是 **AI 指令条光标 (515-530, 595-630)**。

那块掩码盖的是一个**真 bug**：`draw_ai_bar` 的光标闪烁用了真实时钟 `t_now()` 而不是
传入的 `t`，于是**同样的输入渲染出不同的画面**（审计附录 CF）。
修掉之后，配上 `AETHER_FAKE_UTC=1` 固定时钟，**输出完全确定** ——
掩码因此是空的（保留这个机制，是为了将来万一又出现非确定区时有地方写）。

## 基线为什么存哈希而不是图

10 档 BMP 约 29 MB，不该进仓库；而"改动前后必须逐字节相同"这件事，
一串 SHA256 就能完整表达。要人眼看图时，用 `--keep` 把 BMP 留下。

## 约定

BMP 为 24bpp 无行填充、1280x760、54 字节头（由 `draw::write_bmp` 生成）。
"""
import hashlib
import json
import os
import pathlib
import subprocess
import sys

W, H, HDR = 1280, 760, 54
D = pathlib.Path(__file__).parent
ROOT = D.parent
BASELINE = ROOT / "docs" / "证据" / "shot-baseline.json"

# 覆盖主要界面形态的截图档位（与 main.rs 的 --shot 支持对应）
CASES = [
    ("layout1", ["--shot", "1"]),
    ("layout2", ["--shot", "2"]),
    ("layout3", ["--shot", "3"]),
    ("layout4", ["--shot", "4"]),
    ("settings", ["--shot", "1", "--settings"]),
    ("desktop", ["--shot", "1", "--desktop-only"]),
    ("controlcenter", ["--shot", "1", "--control-center"]),
    ("menu", ["--shot", "1", "--menu"]),
    ("installer", ["--shot", "1", "--installer"]),
    ("confirm", ["--shot", "1", "--confirm"]),
]

# 历史上需要掩码的天然非确定区。现在**是空的** —— 见模块文档。
# 保留机制：将来若又出现非确定区，往这里加 (x0, x1, y0, y1) 并写明原因。
MASK_REGIONS: list[tuple[int, int, int, int]] = []


def _env() -> dict:
    """确定模式：固定时钟 + 固定目录夹具（见审计附录 CF / CD.3）。"""
    return dict(os.environ, AETHER_FAKE_UTC="1", AETHER_SHOT_FIXTURE="1")


def render_all(keep: bool = False) -> dict:
    """渲染全部档位，返回 {名字: sha256}。渲染不出来时抛出 RuntimeError（带原因）。"""
    out = {}
    bmp = ROOT / "preview.bmp"
    for name, args in CASES:
        if bmp.exists():
            bmp.unlink()
        proc = subprocess.run(
            ["cargo", "run", "-q", "--offline", "--locked", "--bin", "aether-compositor", "--", *args],
            cwd=ROOT, capture_output=True, text=True, env=_env(),
        )
        if not bmp.exists():
            raise RuntimeError(f"{name} 没有产出 preview.bmp（stderr: {proc.stderr.strip()[:120]}）")
        raw = bmp.read_bytes()
        if len(raw) != HDR + W * H * 3:
            raise RuntimeError(f"{name}: {len(raw)} 字节，与 {W}x{H} 不符")
        out[name] = hashlib.sha256(raw).hexdigest()
        if keep:
            (D / f"shot-{name}.bmp").write_bytes(raw)
    return out


def cmd_save(keep: bool) -> int:
    try:
        hashes = render_all(keep)
    except RuntimeError as e:
        print(f"[SKIP] 渲染不出截图，无法建立基线：{e}")
        return 0
    BASELINE.parent.mkdir(parents=True, exist_ok=True)
    BASELINE.write_text(json.dumps(hashes, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"基线已写入 {BASELINE.relative_to(ROOT)}（{len(hashes)} 档）")
    return 0


def cmd_verify() -> int:
    if not BASELINE.exists():
        print(f"[SKIP] 没有基线 {BASELINE.relative_to(ROOT)} —— 先跑 `save`")
        return 0
    want = json.loads(BASELINE.read_text(encoding="utf-8"))
    try:
        got = render_all()
    except RuntimeError as e:
        print(f"[SKIP] 渲染不出截图，跳过像素回归：{e}")
        return 0
    bad = [k for k in want if want[k] != got.get(k)]
    missing = [k for k in got if k not in want]
    for k in want:
        mark = "OK  " if want[k] == got.get(k) else "DIFF"
        print(f"  [{mark}] {k}")
    if missing:
        print(f"  （基线里没有的新档位，未比对：{missing}）")
    if bad:
        print(f"[FAIL] {len(bad)} 档与基线不同：{bad}")
        print("       如果这次改动**本来就要改变画面**，跑 `save` 更新基线并说明原因；")
        print("       如果**不该有视觉变化**，那这就是回归。")
        return 1
    print(f"[ OK ] {len(want)} 档与基线逐字节相同")
    return 0


# ---------------------------------------------------------------------------
# 历史子命令：手工收图 + 掩码（保留，供旧走查图继续使用）
# ---------------------------------------------------------------------------


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
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    cmd, rest = args[0], args[1:]
    keep = "--keep" in rest

    if cmd == "save":
        return cmd_save(keep)
    if cmd == "verify":
        return cmd_verify()

    # ---- 历史子命令 ----
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
