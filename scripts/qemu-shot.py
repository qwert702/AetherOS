#!/usr/bin/env python3
"""QMP screendump → PNG（stdout）。用法: qemu-shot.py [screen.ppm 相对名] > out.png"""
import json, os, socket, struct, sys, time, zlib

SOCK = '/home/aether/qmp.sock'
ppm = 'screen.ppm'

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(15); s.connect(SOCK)
f = s.makefile('rwb')
f.readline()
f.write(json.dumps({"execute": "qmp_capabilities"}).encode() + b'\n'); f.flush(); f.readline()

f.write(json.dumps({"execute": "screendump",
                    "arguments": {"filename": ppm}}).encode() + b'\n')
f.flush()
while True:
    line = f.readline()
    if not line: break
    m = json.loads(line)
    if 'return' in m or 'error' in m:
        print('screendump:', m, file=sys.stderr); break
s.close()

p = os.path.join('/home/aether', ppm)
for _ in range(30):
    if os.path.exists(p) and os.path.getsize(p) > 1000: break
    time.sleep(0.5)

d = open(p, 'rb').read()
def tok(i):
    while d[i:i+1].isspace(): i += 1
    j = i
    while not d[j:j+1].isspace(): j += 1
    return d[i:j], j
w, i = tok(2); h, i = tok(i); mv, i = tok(i); i += 1
w, h = int(w), int(h)
px = d[i:i+w*h*3]

rows = bytearray()
for y in range(h):
    rows.append(0); rows += px[y*w*3:(y+1)*w*3]
def chunk(t, x):
    return struct.pack('>I', len(x)) + t + x + struct.pack('>I', zlib.crc32(t+x) & 0xffffffff)
sys.stdout.buffer.write(b'\x89PNG\r\n\x1a\n' +
    chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) +
    chunk(b'IDAT', zlib.compress(bytes(rows), 6)) + chunk(b'IEND', b''))
print('PNG %dx%d -> stdout' % (w, h), file=sys.stderr)
