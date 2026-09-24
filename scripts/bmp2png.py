#!/usr/bin/env python3
"""BMP → PNG（无外部依赖，仅标准库）。

用途：aether-compositor 的 --shot 自检产物是 24bpp BMP，
需要转成 PNG 才能被预览/比对工具直接查看。

用法：python scripts/bmp2png.py <输入.bmp> <输出.png>
"""
import struct
import sys
import zlib


def bmp2png(src: str, dst: str) -> tuple[int, int]:
    with open(src, "rb") as f:
        data = f.read()
    if data[:2] != b"BM":
        raise SystemExit(f"{src}: 不是 BMP 文件")
    off = struct.unpack("<I", data[10:14])[0]
    w, h = struct.unpack("<ii", data[18:26])
    if h < 0:  # 自顶向下
        h = -h
        flip = False
    else:
        flip = True
    bpp = struct.unpack("<H", data[28:30])[0] // 8
    stride = ((w * bpp + 3) // 4) * 4
    raw = data[off:]

    rows = []
    for y in range(h):
        row = raw[y * stride : y * stride + w * bpp]
        px = bytearray()
        for x in range(w):
            b, g, r = row[x * bpp : x * bpp + 3]
            px += bytes([r, g, b])
        rows.append(b"\x00" + bytes(px))
    if flip:
        rows.reverse()
    img = b"".join(rows)

    def chunk(tag: bytes, payload: bytes) -> bytes:
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(img, 6))
    png += chunk(b"IEND", b"")
    with open(dst, "wb") as f:
        f.write(png)
    return w, h


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    width, height = bmp2png(sys.argv[1], sys.argv[2])
    print(f"{sys.argv[2]} written ({width}x{height})")
