#!/usr/bin/env python3
"""纯标准库 PPM(P6) → PNG 转换。用法: ppm2png.py in.ppm > out.png（PNG 走 stdout）"""
import os, struct, sys, zlib

# 读取仅限项目树内（VM 上项目根即 /home/aether）
_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
src = sys.argv[1]
if not os.path.realpath(src).startswith(_ROOT + os.sep):
    raise SystemExit('读取路径越界: %s' % src)

d = open(src, 'rb').read()
assert d[:2] == b'P6', 'not P6 PPM'

# 解析头：P6 <ws> w <ws> h <ws> maxval <single-ws> 之后是像素
def tok(i):
    while d[i:i+1].isspace(): i += 1
    j = i
    while j < len(d) and not d[j:j+1].isspace(): j += 1
    return d[i:j], j

w, i = tok(2)
h, i = tok(i); mv, i = tok(i)
i += 1
w, h = int(w), int(h)
px = d[i:i + w*h*3]

rows = bytearray()
stride = w*3
for y in range(h):
    rows.append(0)                      # PNG filter type 0 (None)
    rows += px[y*stride:(y+1)*stride]

def chunk(tag, data):
    return (struct.pack('>I', len(data)) + tag + data +
            struct.pack('>I', zlib.crc32(tag + data) & 0xffffffff))

png = (b'\x89PNG\r\n\x1a\n' +
       chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) +
       chunk(b'IDAT', zlib.compress(bytes(rows), 6)) +
       chunk(b'IEND', b''))
sys.stdout.buffer.write(png)

# 快速内容判断
nz = sum(1 for k in range(0, len(px), 3*101) if px[k:k+3] != b'\x00\x00\x00')
tot = len(range(0, len(px), 3*101))
print('OK %dx%d  非黑采样 %d/%d' % (w, h, nz, tot), file=sys.stderr)
