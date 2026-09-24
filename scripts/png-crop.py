#!/usr/bin/env python3
"""裁剪并放大走查图，用于 1:1 检查细节（边框、字重、图标比例）。

用法：python scripts/png-crop.py 输入.png 输出.png X Y W H [放大倍数]
"""
import struct
import sys
import zlib


def read_png(path):
    data = open(path, "rb").read()
    pos, idat, w, h = 8, b"", 0, 0
    while pos < len(data):
        ln = struct.unpack(">I", data[pos : pos + 4])[0]
        tag = data[pos + 4 : pos + 8]
        payload = data[pos + 8 : pos + 8 + ln]
        if tag == b"IHDR":
            w, h = struct.unpack(">II", payload[:8])
        if tag == b"IDAT":
            idat += payload
        pos += 12 + ln
    raw = zlib.decompress(idat)
    stride = w * 3 + 1
    rows = []
    prev = bytearray(w * 3)
    for y in range(h):
        line = raw[y * stride : (y + 1) * stride]
        ft = line[0]
        cur = bytearray(line[1:])
        if ft == 1:
            for i in range(3, len(cur)):
                cur[i] = (cur[i] + cur[i - 3]) & 255
        elif ft == 2:
            for i in range(len(cur)):
                cur[i] = (cur[i] + prev[i]) & 255
        elif ft == 3:
            for i in range(len(cur)):
                cur[i] = (cur[i] + ((cur[i - 3] if i >= 3 else 0) + prev[i]) // 2) & 255
        elif ft == 4:
            def pa(a, b, c):
                p = a + b - c
                x, y, z = abs(p - a), abs(p - b), abs(p - c)
                return a if x <= y and x <= z else (b if y <= z else c)
            for i in range(len(cur)):
                cur[i] = (cur[i] + pa(cur[i - 3] if i >= 3 else 0, prev[i], prev[i - 3] if i >= 3 else 0)) & 255
        rows.append(bytes(cur))
        prev = cur
    return w, h, rows


def write_png(path, w, h, rows):
    img = b"".join(b"\x00" + r for r in rows)

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(img, 6))
    png += chunk(b"IEND", b"")
    open(path, "wb").write(png)


if __name__ == "__main__":
    if len(sys.argv) < 7:
        raise SystemExit(__doc__)
    src, dst = sys.argv[1], sys.argv[2]
    cx, cy, cw, ch = (int(v) for v in sys.argv[3:7])
    scale = int(sys.argv[7]) if len(sys.argv) > 7 else 1
    w, h, rows = read_png(src)
    cx, cy = max(0, min(cx, w - 1)), max(0, min(cy, h - 1))
    cw, ch = min(cw, w - cx), min(ch, h - cy)
    out = []
    for y in range(cy, cy + ch):
        row = rows[y][cx * 3 : (cx + cw) * 3]
        if scale > 1:
            up = bytearray()
            for x in range(cw):
                # 水平放大：每个像素横向重复 scale 次
                up += row[x * 3 : x * 3 + 3] * scale
            row = bytes(up)
            # 垂直放大：同一行重复放 scale 次（注意不能再乘行字节本身，
            # 否则行宽会变成 scale² 倍，PNG 直接损坏）
            for _ in range(scale):
                out.append(row)
        else:
            out.append(row)
    write_png(dst, cw * scale, ch * scale, out)
    print(f"{dst} written ({cw * scale}x{ch * scale})")
