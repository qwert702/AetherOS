#!/usr/bin/env python3
"""mkapp.py —— 把一个"市面上的 Linux 程序"打包成 AetherOS 能装的包。

## 为什么需要它

AetherOS 镜像里没有包管理器，动态程序的依赖也不会自己出现。实测结论（2026-09-29）：

- **静态链接**的二进制直接就能跑；
- **动态链接**的能不能跑，只取决于"它的共享库依赖是否都在镜像里"。

镜像现在有 ncurses/zlib/openssl/libffi/expat + glibc 家族 + terminfo，但市面上的程序
还会依赖 libnl、libstdc++、libpng、libX11…… 不可能全塞进镜像。所以本工具做两件事：

1. **只打包镜像里没有的库**（`--image-lib-dir` 指向镜像的 rootfs，逐个比对）；
2. **逐层递归**：libA 依赖 libB、libB 又依赖 libC —— 少收一层，装上去照样挂。

## 两条硬规则（都有实测教训）

- **glibc 家族永不打包**。实测：把宿主的 `libc.so.6` 打进包、再让 `LD_LIBRARY_PATH`
  指向它，加载器会用**外来的 libc**，直接 `undefined symbol: __tunable_is_initialized,
  version GLIBC_PRIVATE`。libc/libm/libpthread/libdl/librt/libutil/libcrypt/libresolv/
  libnss_*/libmvec/libanl 一律排除。
- **符号版本必须校验**。镜像的 glibc 是 2.38；程序若要求 `GLIBC_2.39`，把库凑齐了也跑不起来。
  本工具读 ELF 的 `.gnu.version_r`（要求）与镜像 libc 的 `.gnu.version_d`（提供），超了就报错。

## 用法

    # 打包（`--image-lib-dir` 给了才会"只收缺的"）
    python3 scripts/mkapp.py /usr/bin/htop --id htop --name htop \\
        --image-lib-dir /home/aether/platform/build/output/target \\
        --terminfo xterm-256color --out dist/apps

    # 只检查，不产出（问"这个程序在镜像上能跑吗"）
    python3 scripts/mkapp.py /usr/bin/htop --check \\
        --image-lib-dir /home/aether/platform/build/output/target

    # 解析器自测（不需要网络/Linux 特有工具）
    python3 scripts/mkapp.py --selftest

产物目录结构（就是 `aetherd app install` 认的格式）：

    <out>/<id>/
      app.json            清单
      bin/<程序>          入口
      lib/*.so*           自带的共享库（启动时经 LD_LIBRARY_PATH 生效）
      share/terminfo/     终端条目（全屏程序必需，启动时经 TERMINFO_DIRS 生效）
      bundle.json         打包溯源信息（aetherd 不解析，只给人看）

加 `--tar` 会额外产出 `<id>.aep`（未压缩 tar）—— 未压缩是刻意的：目标端不需要 gzip，
少一个解析器就少一片攻击面。
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import struct
import sys
import tarfile
from dataclasses import dataclass, field
from pathlib import Path

# ── ELF 常量 ────────────────────────────────────────────────────────────────
PT_LOAD, PT_DYNAMIC, PT_INTERP = 1, 2, 3
DT_NULL, DT_NEEDED, DT_STRTAB, DT_STRSZ = 0, 1, 5, 10
DT_SONAME, DT_RPATH, DT_RUNPATH = 14, 15, 29
DT_VERNEED, DT_VERNEEDNUM = 0x6FFFFFFE, 0x6FFFFFFF
DT_VERDEF, DT_VERDEFNUM = 0x6FFFFFFC, 0x6FFFFFFD

#: **永不打包**的库名（glibc 家族）。混合两套 glibc 必然炸，见模块头注释。
GLIBC_FAMILY = {
    "libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2", "librt.so.1",
    "libutil.so.1", "libcrypt.so.1", "libresolv.so.2", "libmvec.so.1", "libanl.so.1",
    "libnsl.so.1", "libnss_dns.so.2", "libnss_files.so.2", "libnss_compat.so.2",
    "ld-linux-x86-64.so.2", "ld-linux.so.2",
}

#: 内核提供的虚拟库，永远跳过（不是文件）。
VDSO = {"linux-vdso.so.1", "linux-gate.so.1"}

#: 打包器默认的库搜索目录（构建机/宿主上的常见位置）。
DEFAULT_LIB_DIRS = [
    "/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu",
    "/lib64", "/usr/lib64", "/lib", "/usr/lib",
]

#: 镜像里的库目录（相对 `--image-lib-dir`）。
IMAGE_LIB_SUBDIRS = ["lib", "lib64", "usr/lib", "usr/lib64"]


# ── ELF 解析 ────────────────────────────────────────────────────────────────

@dataclass
class ElfInfo:
    path: str = ""
    is_dynamic: bool = False
    interp: str | None = None
    needed: list[str] = field(default_factory=list)
    soname: str | None = None
    rpaths: list[str] = field(default_factory=list)
    #: 本文件**要求**的版本符号（如 GLIBC_2.38、GLIBCXX_3.4.30）。
    verneed: list[str] = field(default_factory=list)
    #: 同上，但按"向哪个库要"分组：{"libncursesw.so.6": ["NCURSES6_..."], ...}。
    #: 用它做**按库的兼容性判断**：镜像里有同名库 ≠ 那个库能满足要求。
    verneed_by_lib: dict[str, list[str]] = field(default_factory=dict)
    #: 本文件**提供**的版本符号（共享库才有意义）。
    verdef: list[str] = field(default_factory=list)


def _u16(b: bytes, o: int) -> int:
    return struct.unpack_from("<H", b, o)[0]


def _u32(b: bytes, o: int) -> int:
    return struct.unpack_from("<I", b, o)[0]


def _u64(b: bytes, o: int) -> int:
    return struct.unpack_from("<Q", b, o)[0]


def _cstr(b: bytes, o: int, cap: int = 4096) -> str:
    if o < 0 or o >= len(b):
        return ""
    end = b.find(b"\0", o, min(len(b), o + cap))
    if end < 0:
        end = min(len(b), o + cap)
    return b[o:end].decode("utf-8", "replace")


def parse_elf(data: bytes) -> ElfInfo | None:
    """解析 ELF64 小端，取出打包需要的全部信息。

    这是**不可信输入**解析器：所有偏移都走边界检查，越界即返回 None，绝不抛异常
    （一个坏文件不该让打包流程崩掉，只该让它被拒）。
    """
    if len(data) < 64 or data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        return None
    e_type = _u16(data, 16)
    if e_type not in (2, 3):  # ET_EXEC / ET_DYN
        return None
    e_phoff = _u64(data, 0x20)
    e_phentsize = _u16(data, 0x36)
    e_phnum = _u16(data, 0x38)
    if e_phentsize < 56 or e_phoff + e_phnum * e_phentsize > len(data):
        return None

    info = ElfInfo()
    loads: list[tuple[int, int, int]] = []          # (vaddr, filesz, offset)
    dyn_off = dyn_sz = None
    for i in range(min(e_phnum, 256)):
        base = e_phoff + i * e_phentsize
        p_type = _u32(data, base)
        p_offset = _u64(data, base + 8)
        p_vaddr = _u64(data, base + 16)
        p_filesz = _u64(data, base + 32)
        if p_type == PT_LOAD:
            loads.append((p_vaddr, p_filesz, p_offset))
        elif p_type == PT_DYNAMIC:
            dyn_off, dyn_sz = p_offset, p_filesz
        elif p_type == PT_INTERP:
            if p_offset < len(data):
                info.interp = _cstr(data, p_offset, min(p_filesz, 4096))
    if not loads or dyn_off is None:
        return info  # 静态链接：interp/needed 都空

    def vaddr_to_off(addr: int) -> int | None:
        for vaddr, filesz, offset in loads:
            if vaddr <= addr < vaddr + filesz:
                return offset + (addr - vaddr)
        return None

    info.is_dynamic = True
    entries: dict[int, list[int]] = {}
    if dyn_off + dyn_sz > len(data):
        dyn_sz = len(data) - dyn_off
    for i in range(min(dyn_sz // 16, 4096)):
        off = dyn_off + i * 16
        tag = _u64(data, off)
        if tag == DT_NULL:
            break
        entries.setdefault(tag, []).append(_u64(data, off + 8))

    strtab_vaddr = (entries.get(DT_STRTAB) or [None])[0]
    strsz = (entries.get(DT_STRSZ) or [0])[0]
    if strtab_vaddr is None:
        return info
    strtab_off = vaddr_to_off(strtab_vaddr)
    if strtab_off is None:
        return info
    strtab = data[strtab_off:strtab_off + min(strsz, 1 << 20)]

    def str_at(idx: int) -> str:
        return _cstr(strtab, idx)

    info.needed = [str_at(v) for v in entries.get(DT_NEEDED, [])]
    if DT_SONAME in entries:
        info.soname = str_at(entries[DT_SONAME][0])
    for tag in (DT_RPATH, DT_RUNPATH):
        for v in entries.get(tag, []):
            info.rpaths += [p for p in str_at(v).split(":") if p]
    info.verneed, info.verneed_by_lib = _read_verneed(data, entries, str_at, vaddr_to_off)
    info.verdef = _read_verdef(data, entries, str_at, vaddr_to_off)
    return info


def _read_verneed(data, entries, str_at, vaddr_to_off) -> tuple[list[str], dict[str, list[str]]]:
    """读 `.gnu.version_r`：这个文件**要求**哪些版本符号，以及**向哪个库要**。

    返回（扁平列表, 按库分组）。按库分组是关键：镜像里"有 libncursesw.so.6"不等于
    "它能满足 NCURSES6_* 的要求" —— 实测差异会打印
    `no version information available (required by ...)`。
    """
    flat: list[str] = []
    by_lib: dict[str, list[str]] = {}
    off_v = (entries.get(DT_VERNEED) or [None])[0]
    if off_v is None:
        return flat, by_lib
    base = vaddr_to_off(off_v)
    if base is None:
        return flat, by_lib
    vn = base
    for _ in range(64):  # 有界：正常文件不会超过这个数
        if vn + 16 > len(data):
            break
        vn_cnt = _u16(data, vn + 2)
        vn_file = _u32(data, vn + 4)
        vn_aux = _u32(data, vn + 8)
        vn_next = _u32(data, vn + 12)
        lib = str_at(vn_file)
        aux = vn + vn_aux
        for _j in range(min(vn_cnt, 64)):
            if aux + 16 > len(data):
                break
            name_off = _u32(data, aux + 8)
            nxt = _u32(data, aux + 12)
            s = str_at(name_off)
            if s:
                flat.append(s)
                if lib:
                    by_lib.setdefault(lib, []).append(s)
            if not nxt:
                break
            aux += nxt
        if not vn_next:
            break
        vn += vn_next
    return flat, by_lib


def _read_verdef(data, entries, str_at, vaddr_to_off) -> list[str]:
    """读 `.gnu.version_d`：这个共享库**提供**哪些版本符号（镜像 libc 的 GLIBC_x.y 靠它）。"""
    out: list[str] = []
    off_v = (entries.get(DT_VERDEF) or [None])[0]
    if off_v is None:
        return out
    base = vaddr_to_off(off_v)
    if base is None:
        return out
    vd = base
    for _ in range(4096):
        if vd + 20 > len(data):
            break
        vd_cnt = _u16(data, vd + 6)
        vd_aux = _u32(data, vd + 12)
        vd_next = _u32(data, vd + 16)
        if vd_cnt >= 1:
            aux = vd + vd_aux
            if aux + 8 <= len(data):
                s = str_at(_u32(data, aux))
                if s:
                    out.append(s)
        if not vd_next:
            break
        vd += vd_next
    return out


# ── 版本比较 / glibc 家族判定 ───────────────────────────────────────────────

def version_key(sym: str) -> tuple[int, ...] | None:
    """`GLIBC_2.38` / `GLIBCXX_3.4.30` → 可比元组。取不出数字返回 None。"""
    parts = sym.split("_", 1)[-1].split(".")
    nums = []
    for p in parts:
        if not p.isdigit():
            break
        nums.append(int(p))
    return tuple(nums) if nums else None


def max_version(syms: list[str], prefix: str) -> tuple[int, ...] | None:
    """在给定前缀（如 GLIBC_）的符号里取最高版本。"""
    best = None
    for s in syms:
        if not s.startswith(prefix):
            continue
        k = version_key(s)
        if k and (best is None or k > best):
            best = k
    return best


def fmt_ver(v: tuple[int, ...] | None) -> str:
    return ".".join(str(x) for x in v) if v else "?"


def is_glibc_family(soname: str) -> bool:
    return soname in GLIBC_FAMILY


# ── 依赖收集 ────────────────────────────────────────────────────────────────

@dataclass
class CollectResult:
    entry: ElfInfo
    bundled: dict[str, Path] = field(default_factory=dict)   # soname -> 宿主上的路径
    skipped_in_image: dict[str, str] = field(default_factory=dict)  # soname -> 镜像里已有且能满足要求
    #: 镜像里有同名库、但它满足不了的版本要求 → 只能带我们自己的那份。
    incompatible: dict[str, list[str]] = field(default_factory=dict)
    skipped_glibc: list[str] = field(default_factory=list)
    missing: list[str] = field(default_factory=list)
    required_glibc: tuple[int, ...] | None = None
    provided_glibc: tuple[int, ...] | None = None
    target_ok: bool = True
    notes: list[str] = field(default_factory=list)


def incompatible_versions(image_lib_dir: Path | None, in_image_rel: str,
                         required: list[str]) -> list[str]:
    """镜像里那份库能满足这些版本要求吗？返回**满足不了**的版本符号。

    为什么必须查：镜像里"有 libncursesw.so.6"不等于"它能满足 NCURSES6_* 的要求"。
    2026-09-29 实测：Buildroot 编的 ncurses 没有版本符号表，于是 htop 每次运行都打印
    `no version information available (required by htop)` —— 程序能跑，但看着就是坏的。
    """
    if not required:
        return []
    if image_lib_dir is None:
        return list(required)
    p = image_lib_dir / in_image_rel
    if not p.is_file():
        return list(required)
    info = parse_elf(p.read_bytes())
    if info is None:
        return list(required)
    provided = set(info.verdef)
    return [v for v in required if v not in provided]


def find_lib(soname: str, dirs: list[Path]) -> Path | None:
    for d in dirs:
        p = d / soname
        if p.is_file():
            return p
    return None


def image_provides(image_lib_dir: Path | None, soname: str) -> str | None:
    """镜像里有没有这个库？返回它所在的镜像目录（给报告用）。"""
    if image_lib_dir is None:
        return None
    for sub in IMAGE_LIB_SUBDIRS:
        p = image_lib_dir / sub / soname
        if p.is_file():
            return f"{sub}/{soname}"
    return None


def image_libc(image_lib_dir: Path | None) -> Path | None:
    if image_lib_dir is None:
        return None
    for sub in IMAGE_LIB_SUBDIRS:
        p = image_lib_dir / sub / "libc.so.6"
        if p.is_file():
            return p
    return None


def collect(entry: Path, search_dirs: list[Path], image_lib_dir: Path | None,
            allow_missing: bool = False) -> CollectResult:
    """递归收集 entry 的依赖，决定"打包哪些 / 镜像已有哪些 / 缺哪些"。"""
    res = CollectResult(entry=ElfInfo())
    data = entry.read_bytes()
    info = parse_elf(data)
    if info is None:
        raise SystemExit(f"[mkapp] {entry} 不是能识别的 ELF64 可执行文件")
    info.path = str(entry)
    res.entry = info
    if not info.is_dynamic or (info.interp is None and not info.needed):
        res.notes.append("静态链接：不依赖任何共享库，可以直接跑")
        return res

    required: list[str] = list(info.verneed)
    # 向哪个库要哪些版本（entry 自己 + 后续带上来的每个库都要并进来）
    required_by_lib: dict[str, list[str]] = {k: list(v) for k, v in info.verneed_by_lib.items()}
    queue: list[tuple[str, list[Path]]] = [(n, [Path(p) for p in info.rpaths]) for n in info.needed]
    seen: set[str] = set()

    while queue:
        soname, extra_dirs = queue.pop(0)
        if soname in seen or soname in VDSO:
            continue
        seen.add(soname)
        if is_glibc_family(soname):
            res.skipped_glibc.append(soname)
            continue
        in_image = image_provides(image_lib_dir, soname)
        if in_image:
            # 镜像里有同名库 ≠ 它满足要求：先问一句"版本符号对得上吗"
            unmet = incompatible_versions(image_lib_dir, in_image, required_by_lib.get(soname, []))
            if not unmet:
                res.skipped_in_image[soname] = in_image
                # 镜像里的库自己不递归：镜像已经把它需要的东西装齐了
                continue
            res.incompatible[soname] = unmet
        found = find_lib(soname, extra_dirs + search_dirs)
        if found is None:
            if in_image:
                # 镜像里有（只是版本对不上），宿主上又没有 → 只能靠镜像那份，降级为提示
                res.skipped_in_image[soname] = in_image
                res.notes.append(
                    f"{soname}：镜像里有但版本符号对不上（{', '.join(res.incompatible[soname])}），"
                    f"宿主上也找不到可用副本 —— 装上可能打印 'no version information available'"
                )
            else:
                res.missing.append(soname)
            continue
        lib_data = found.read_bytes()
        lib_info = parse_elf(lib_data)
        res.bundled[soname] = found
        if lib_info:
            required += lib_info.verneed
            for k, vs in lib_info.verneed_by_lib.items():
                required_by_lib.setdefault(k, [])
                for v in vs:
                    if v not in required_by_lib[k]:
                        required_by_lib[k].append(v)
            for n in lib_info.needed:
                if n not in seen:
                    queue.append((n, [Path(p) for p in lib_info.rpaths]))

    res.required_glibc = max_version(required, "GLIBC_")
    libc = image_libc(image_lib_dir)
    if libc is not None:
        libc_info = parse_elf(libc.read_bytes())
        if libc_info:
            res.provided_glibc = max_version(libc_info.verdef, "GLIBC_")
    if res.required_glibc and res.provided_glibc and res.required_glibc > res.provided_glibc:
        res.target_ok = False
    if res.missing and not allow_missing:
        res.target_ok = False
    return res


def report(res: CollectResult, image_lib_dir: Path | None) -> str:
    """把结论写成人话 —— 用户要看的是"能不能跑、缺什么"。"""
    L = []
    if res.entry.interp is None and not res.entry.needed:
        L.append("  链接方式：静态 —— 不依赖共享库，直接能跑 ✅")
        return "\n".join(L)
    L.append(f"  链接方式：动态（解释器 {res.entry.interp or '?'}）")
    L.append(f"  入口依赖：{', '.join(res.entry.needed) or '（无）'}")
    if res.skipped_glibc:
        L.append(f"  跳过（glibc 家族，镜像自带，绝不打包）：{', '.join(sorted(set(res.skipped_glibc)))}")
    if res.skipped_in_image:
        L.append(f"  镜像已有，不必打包：{', '.join(f'{k}' for k in sorted(res.skipped_in_image))}")
    for k, vs in sorted(res.incompatible.items()):
        shown = ", ".join(vs[:3]) + ("…" if len(vs) > 3 else "")
        if k in res.bundled:
            L.append(f"  镜像里的 {k} 版本符号对不上（缺 {shown}）→ 改带宿主那份（否则每次运行都会告警）")
        else:
            L.append(f"  镜像里的 {k} 版本符号对不上（缺 {shown}），宿主上也没有可用副本 ⚠️")
    if res.bundled:
        L.append(f"  需要随包携带（{len(res.bundled)} 个）：")
        for k in sorted(res.bundled):
            L.append(f"    {k}  ←  {res.bundled[k]}")
    if res.missing:
        L.append(f"  ⚠️ 宿主机上也找不到：{', '.join(res.missing)}")
    if res.required_glibc:
        L.append(f"  要求 glibc ≥ {fmt_ver(res.required_glibc)}；"
                 f"镜像提供 {fmt_ver(res.provided_glibc)}")
    if not res.target_ok:
        L.append("  结论：**在当前镜像上跑不起来**（见上面的缺库/版本问题）")
    elif res.missing:
        L.append("  结论：缺库，装上也跑不起来（除非补上）")
    else:
        L.append("  结论：可以跑 ✅")
    for n in res.notes:
        L.append(f"  注：{n}")
    return "\n".join(L)


# ── terminfo 收集 ───────────────────────────────────────────────────────────

def collect_terminfo(names: list[str], src_dirs: list[Path], dst: Path) -> list[str]:
    """把终端条目拷进 `share/terminfo/<首字母>/<名字>`。

    为什么必须带：ncurses 程序靠 terminfo 把 `TERM` 映射成能力表；缺了会直接
    `Error opening terminal: xterm-256color`，而且比缺库更难查（库都在、就是起不来）。
    """
    got = []
    for name in names:
        for d in src_dirs:
            cand = d / name[0] / name
            if cand.is_file():
                out = dst / name[0] / name
                out.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(cand, out)
                got.append(name)
                break
        else:
            got.append(f"(缺) {name}")
    return got


# ── 打包 ────────────────────────────────────────────────────────────────────

def write_tar(pkg_dir: Path, tar_path: Path) -> None:
    """产出未压缩 tar。刻意不用 gzip：目标端因此不需要解压库（少一个解析器）。"""
    entries = sorted(p for p in pkg_dir.rglob("*"))
    with tarfile.open(tar_path, "w", format=tarfile.USTAR_FORMAT) as tf:
        for p in [pkg_dir] + entries:
            rel = p.relative_to(pkg_dir.parent)
            ti = tf.gettarinfo(str(p), arcname=str(rel))
            ti.uid = ti.gid = 0
            ti.uname = ti.gname = "root"
            ti.mtime = 0
            if ti.isfile():
                with p.open("rb") as f:
                    tf.addfile(ti, f)
            else:
                tf.addfile(ti)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(
        prog="mkapp.py",
        description="把市面上的 Linux 程序打包成 AetherOS 能装的包（自带依赖 + terminfo）。",
    )
    ap.add_argument("binary", nargs="?", help="要打包的可执行文件")
    ap.add_argument("--id", help="应用 id（默认取文件名；只允许字母数字与 - _ .）")
    ap.add_argument("--name", help="显示名（默认同 id）")
    ap.add_argument("--entry", help="包内入口相对路径（默认 bin/<文件名>）")
    ap.add_argument("--args", nargs="*", default=[], help="固定参数（写进包装脚本）")
    ap.add_argument("--desc", default="", help="一句话说明")
    ap.add_argument("--out", default="dist/apps", help="产物根目录（默认 dist/apps）")
    ap.add_argument("--tar", action="store_true", help="额外产出 <id>.aep（未压缩 tar）")
    ap.add_argument("--check", action="store_true", help="只检查依赖，不产出任何文件")
    ap.add_argument("--selftest", action="store_true", help="只跑解析器自测")
    ap.add_argument("--image-lib-dir", help="镜像 rootfs 目录（给了才只收「镜像里没有的」库）")
    ap.add_argument("--lib-dir", action="append", default=[],
                    help="额外的库搜索目录（可重复；默认走构建机常见路径）")
    ap.add_argument("--terminfo", nargs="*", default=[],
                    help="要随包携带的终端条目名（如 xterm-256color）")
    ap.add_argument("--terminfo-src", default="/usr/share/terminfo",
                    help="宿主上的 terminfo 目录（默认 /usr/share/terminfo）")
    ap.add_argument("--allow-missing", action="store_true",
                    help="即使有找不到的库也继续打包（默认拒绝，免得装完跑不起来）")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()

    if not args.binary:
        ap.error("缺少要打包的文件（或用 --selftest）")

    binary = Path(args.binary)
    if not binary.is_file():
        print(f"[mkapp] 找不到文件：{binary}", file=sys.stderr)
        return 2
    image_lib_dir = Path(args.image_lib_dir) if args.image_lib_dir else None
    if image_lib_dir is not None and not image_lib_dir.is_dir():
        print(f"[mkapp] --image-lib-dir 不存在：{image_lib_dir}", file=sys.stderr)
        return 2

    search_dirs = [Path(p) for p in args.lib_dir] + [Path(p) for p in DEFAULT_LIB_DIRS]
    try:
        res = collect(binary, search_dirs, image_lib_dir, args.allow_missing)
    except SystemExit as e:
        print(e, file=sys.stderr)
        return 2

    print(f"== {binary} ==")
    print(report(res, image_lib_dir))
    if not res.target_ok:
        print("\n[mkapp] 结论：这个程序在当前镜像上跑不起来，已停止（不加 --allow-missing 不会产出）。"
              "\n        两条路：① 在镜像里补上缺的库（改 platform/br2-external 的 defconfig）；"
              "\n                 ② 换一个静态链接的版本。", file=sys.stderr)
        return 1
    if args.check:
        print("\n[mkapp] --check：只检查，未产出任何文件。")
        return 0

    app_id = args.id or binary.name
    if not app_id or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_." for c in app_id):
        print(f"[mkapp] 非法 id：{app_id!r}（只允许字母数字与 - _ .）", file=sys.stderr)
        return 2
    entry_rel = args.entry or f"bin/{binary.name}"

    pkg = Path(args.out) / app_id
    if pkg.exists():
        shutil.rmtree(pkg)
    (pkg / Path(entry_rel).parent).mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, pkg / entry_rel)
    os.chmod(pkg / entry_rel, 0o755)

    for soname, src in sorted(res.bundled.items()):
        (pkg / "lib").mkdir(exist_ok=True)
        # 顺着符号链接取真实文件，但落盘时用 soname（加载器按 soname 找）
        shutil.copy2(src.resolve(), pkg / "lib" / soname)
        os.chmod(pkg / "lib" / soname, 0o755)
    terminfo_names = args.terminfo or []
    if not terminfo_names and res.entry.needed and any(
        n.startswith(("libncurses", "libtinfo", "libtermcap")) for n in res.entry.needed
    ):
        terminfo_names = ["xterm-256color"]  # 镜像的 PTY 就是这个 TERM（compositor/src/pty.rs）
        print("  （检测到 curses 程序，自动带上 terminfo：xterm-256color）")
    if terminfo_names:
        got = collect_terminfo(terminfo_names, [Path(args.terminfo_src)], pkg / "share" / "terminfo")
        print(f"  随包 terminfo：{', '.join(got)}")

    manifest = {
        "id": app_id,
        "name": args.name or app_id,
        "version": "",
        "entry": entry_rel,
        "args": list(args.args),
        "desc": args.desc,
    }
    (pkg / "app.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
                                 encoding="utf-8")
    bundle = {
        "packed_from": str(binary.resolve()),
        "packed_by": "scripts/mkapp.py",
        "bundled_libs": {k: str(v) for k, v in sorted(res.bundled.items())},
        "skipped_in_image": dict(sorted(res.skipped_in_image.items())),
        "image_incompatible": {k: v for k, v in sorted(res.incompatible.items())},
        "skipped_glibc_family": sorted(set(res.skipped_glibc)),
        "glibc_required": fmt_ver(res.required_glibc),
        "glibc_provided_by_image": fmt_ver(res.provided_glibc),
        "note": "本文件仅供人看；aetherd 不解析它（目标端越薄越好）。",
    }
    (pkg / "bundle.json").write_text(json.dumps(bundle, ensure_ascii=False, indent=2) + "\n",
                                     encoding="utf-8")

    print(f"\n[mkapp] 已产出 {pkg}")
    if args.tar:
        tar_path = Path(args.out) / f"{app_id}.aep"
        write_tar(pkg, tar_path)
        print(f"[mkapp] 已产出 {tar_path}（{tar_path.stat().st_size} 字节，未压缩 tar）")
    print(f"[mkapp] 安装：把 {pkg} 拷进 guest，然后 `aetherd app install <路径>`")
    return 0


# ── 自测：用构造出来的 ELF 字节测解析器（不需要 Linux、不联网）───────────────

def _synth_elf(interp: str | None, needed: list[str], soname: str | None = None,
               verneed: list[str] = None, verdef: list[str] = None,
               verneed_lib: str | None = None) -> bytes:
    """造一个最小 ELF64：PT_LOAD + 可选 PT_INTERP + PT_DYNAMIC + 可选 verneed/verdef。

    verneed/verdef 按真实的 Elf64 结构拼出来（否则测的就不是解析器而是我的假设）。
    """
    PHOFF, PHENT = 64, 56
    LOAD_OFF, LOAD_VADDR = 0x100, 0x100
    STRTAB_VADDR = 0x180
    VERNEED_VADDR = 0x300
    VERDEF_VADDR = 0x380
    INTERP_OFF = 0x2C0
    DYN_OFF = 0x100
    nph = 3 if interp else 2
    b = bytearray(0x800)
    b[0:4] = b"\x7fELF"
    b[4], b[5] = 2, 1
    struct.pack_into("<H", b, 16, 2)
    struct.pack_into("<Q", b, 0x20, PHOFF)
    struct.pack_into("<H", b, 0x36, PHENT)
    struct.pack_into("<H", b, 0x38, nph)

    strtab = bytearray(b"\0")
    offs = {}

    def add(s: str) -> int:
        offs[s] = len(strtab)
        strtab.extend(s.encode() + b"\0")
        return offs[s]

    need_offs = [add(n) for n in needed]
    sn_off = add(soname) if soname else None
    vn_offs = [add(v) for v in (verneed or [])]
    vn_file_off = add(verneed_lib) if (verneed_lib and vn_offs) else 0
    vd_offs = [add(v) for v in (verdef or [])]
    b[STRTAB_VADDR:STRTAB_VADDR + len(strtab)] = strtab

    # Elf64_Verneed(16) + n×Elf64_Vernaux(16)
    vn_off = None
    if vn_offs:
        vn_off = VERNEED_VADDR
        struct.pack_into("<HHIII", b, vn_off, 1, len(vn_offs), vn_file_off, 20, 0)
        aux = vn_off + 20
        for i, o in enumerate(vn_offs):
            nxt = 16 if i < len(vn_offs) - 1 else 0
            struct.pack_into("<IHHII", b, aux, 0, 0, i + 2, o, nxt)
            aux += 16

    # n×(Elf64_Verdef(20) + Elf64_Verdaux(8))
    vd_off = None
    if vd_offs:
        vd_off = VERDEF_VADDR
        cur = vd_off
        for i, o in enumerate(vd_offs):
            nxt = 28 if i < len(vd_offs) - 1 else 0
            struct.pack_into("<HHHHIII", b, cur, 1, 0, i + 1, 1, 0, 20, nxt)
            struct.pack_into("<II", b, cur + 20, o, 0)
            cur += 28

    items = [(1, o) for o in need_offs]
    if sn_off is not None:
        items.append((14, sn_off))
    items.append((5, STRTAB_VADDR))
    items.append((10, len(strtab)))
    if vn_off is not None:
        items.append((DT_VERNEED, vn_off))
        items.append((DT_VERNEEDNUM, len(vn_offs)))
    if vd_off is not None:
        items.append((DT_VERDEF, vd_off))
        items.append((DT_VERDEFNUM, len(vd_offs)))
    items.append((0, 0))
    for i, (tag, val) in enumerate(items):
        struct.pack_into("<QQ", b, DYN_OFF + i * 16, tag, val)

    def put_ph(idx, ptype, off, vaddr, filesz):
        base = PHOFF + idx * PHENT
        struct.pack_into("<IIQQQQQQ", b, base, ptype, 0, off, vaddr, vaddr, filesz, filesz, 0x1000)

    put_ph(0, 1, LOAD_OFF, LOAD_VADDR, 0x300)  # 必须覆盖 strtab/verneed/verdef，否则地址映射不到
    idx = 1
    if interp:
        b[INTERP_OFF:INTERP_OFF + len(interp) + 1] = interp.encode() + b"\0"
        put_ph(idx, 3, INTERP_OFF, INTERP_OFF, len(interp) + 1)
        idx += 1
    put_ph(idx, 2, DYN_OFF, LOAD_VADDR, len(items) * 16)
    return bytes(b)


def selftest() -> int:
    ok = 0
    bad = []

    def check(cond, msg):
        nonlocal ok
        if cond:
            ok += 1
        else:
            bad.append(msg)

    e = parse_elf(_synth_elf("/lib64/ld-linux-x86-64.so.2", ["libc.so.6", "libm.so.6"]))
    check(e is not None and e.interp == "/lib64/ld-linux-x86-64.so.2", "PT_INTERP 解析")
    check(e is not None and e.needed == ["libc.so.6", "libm.so.6"], "DT_NEEDED 解析")

    e2 = parse_elf(_synth_elf(None, [], soname="libmine.so.1"))
    check(e2 is not None and e2.soname == "libmine.so.1", "DT_SONAME 解析")

    e3 = parse_elf(_synth_elf("/lib64/ld-linux-x86-64.so.2", ["libc.so.6"],
                              verneed=["GLIBC_2.2.5", "GLIBC_2.38"]))
    check(e3 is not None and e3.verneed == ["GLIBC_2.2.5", "GLIBC_2.38"], "verneed 解析")
    check(max_version(e3.verneed, "GLIBC_") == (2, 38), "verneed 取最高版本")

    check(parse_elf(b"not an elf at all") is None, "非 ELF 应被拒")
    check(parse_elf(b"\x7fELF" + b"\0" * 200) is None, "截断的 ELF 应被拒且不抛异常")

    check(version_key("GLIBC_2.38") == (2, 38), "version_key GLIBC")
    check(version_key("GLIBCXX_3.4.30") == (3, 4, 30), "version_key GLIBCXX")
    check(version_key("WEAK") is None, "无数字应返回 None")

    check(is_glibc_family("libc.so.6") and is_glibc_family("libm.so.6")
          and is_glibc_family("libpthread.so.0"), "glibc 家族识别")
    check(not is_glibc_family("libstdc++.so.6") and not is_glibc_family("libnl-3.so.200"),
          "非 glibc 家族不该被排除")

    # verneed 按库分组：这是"镜像里有同名库 ≠ 它能满足要求"的判断依据
    e4 = parse_elf(_synth_elf("/lib64/ld-linux-x86-64.so.2", ["libncursesw.so.6"],
                              verneed=["NCURSES6_5.0.19991023"], verneed_lib="libncursesw.so.6"))
    check(e4 is not None and e4.verneed_by_lib == {"libncursesw.so.6": ["NCURSES6_5.0.19991023"]},
          "verneed 按库分组")

    e5 = parse_elf(_synth_elf(None, [], soname="libncursesw.so.6",
                              verdef=["NCURSES6_5.0.19991023", "NCURSES6_6.0.20200118"]))
    check(e5 is not None and sorted(e5.verdef) == ["NCURSES6_5.0.19991023", "NCURSES6_6.0.20200118"],
          "verdef 解析（共享库提供哪些版本）")

    # 兼容性判定：这是 htop 那条 "no version information available" 告警的根因
    import tempfile  # 仅自测用，不进主流程
    with tempfile.TemporaryDirectory() as td:
        img = Path(td) / "usr" / "lib"
        img.mkdir(parents=True)
        need = e4.verneed_by_lib["libncursesw.so.6"]
        (img / "libncursesw.so.6").write_bytes(
            _synth_elf(None, [], soname="libncursesw.so.6"))
        check(incompatible_versions(Path(td), "usr/lib/libncursesw.so.6", need) == need,
              "镜像库没有版本符号 → 判不兼容（应改带宿主那份）")
        (img / "libncursesw.so.6").write_bytes(
            _synth_elf(None, [], soname="libncursesw.so.6", verdef=["NCURSES6_5.0.19991023"]))
        check(incompatible_versions(Path(td), "usr/lib/libncursesw.so.6", need) == [],
              "镜像库提供了所需版本 → 判兼容（不必打包）")
        check(incompatible_versions(Path(td), "usr/lib/libncursesw.so.6", []) == [],
              "没有版本要求时不该被误判为不兼容")

    print(f"[selftest] 通过 {ok} 项")
    for m in bad:
        print(f"  ✗ {m}", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
