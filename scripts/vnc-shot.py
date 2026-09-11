#!/usr/bin/env python3
"""极简 RFB/VNC 客户端：查询 guest 显示尺寸并抓取整屏 → PNG（stdout）。
用法: vnc-shot.py [host] [port] > out.png"""
import socket, struct, sys, zlib

port = 5900
host = '127.0.0.1'
if len(sys.argv) > 2:
    host = sys.argv[1]
    port = int(sys.argv[2])

s = socket.create_connection((host, port), timeout=15)
s.settimeout(20)

ver = s.recv(12)
print('server version:', ver.decode('ascii', 'replace').strip())
s.sendall(b'RFB 003.008\n')

n = s.recv(1)[0]
types = s.recv(n)
print('security types:', list(types))
assert 1 in types, 'no None auth'
s.sendall(bytes([1]))                      # 选 None
res = s.recv(4)
print('security result:', struct.unpack('>I', res)[0])
s.sendall(bytes([1]))                      # ClientInit shared

hdr = s.recv(24)
w, h = struct.unpack('>HH', hdr[:4])
pf = hdr[4:20]
name_len = struct.unpack('>I', hdr[20:24])[0]
name = s.recv(name_len) if name_len else b''
print('framebuffer: %dx%d  桌面名=%r' % (w, h, name.decode('utf-8', 'replace')))
print('server pixel format: bpp=%d depth=%d bigend=%d truecolor=%d' % (pf[0], pf[1], pf[2], pf[3]))

# 请求 32bpp 小端真彩：值 = R<<16 | G<<8 | B  → 字节序 [B,G,R,X]
mypf = struct.pack('>BBBBHHHBBB3x', 32, 24, 0, 1, 255, 255, 255, 16, 8, 0)
s.sendall(b'\x00' + b'\x00\x00\x00' + mypf)
s.sendall(struct.pack('>BBH', 2, 0, 1) + struct.pack('>i', 0))   # SetEncodings: Raw

s.sendall(struct.pack('>BBHHHH', 3, 0, 0, 0, w, h))              # Full update

# 读 FramebufferUpdate（可能有多个矩形）
buf = b''
def need(n):
    global buf
    while len(buf) < n:
        d = s.recv(65536)
        if not d: raise EOFError('连接断开')
        buf += d
    out, buf = buf[:n], buf[n:]
    return out

need(4)
mtype, _pad, nrect = struct.unpack('>BBH', need(4))
assert mtype == 0, 'unexpected message %d' % mtype
canvas = bytearray(w * h * 3)
for _ in range(nrect):
    x, y, rw, rh, enc = struct.unpack('>HHHHi', need(12))
    assert enc == 0, 'encoding %d 未支持' % enc
    data = need(rw * rh * 4)
    for row in range(rh):
        src = row * rw * 4
        dst = ((y + row) * w + x) * 3
        for col in range(rw):
            o = src + col * 4
            canvas[dst + col*3]     = data[o + 2]   # R
            canvas[dst + col*3 + 1] = data[o + 1]   # G
            canvas[dst + col*3 + 2] = data[o]       # B

rows = bytearray()
for yy in range(h):
    rows.append(0); rows += canvas[yy*w*3:(yy+1)*w*3]
def chunk(t, x):
    return struct.pack('>I', len(x)) + t + x + struct.pack('>I', zlib.crc32(t+x) & 0xffffffff)
sys.stdout.buffer.write(b'\x89PNG\r\n\x1a\n' +
    chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) +
    chunk(b'IDAT', zlib.compress(bytes(rows), 6)) + chunk(b'IEND', b''))
nz = sum(1 for k in range(0, len(canvas), 3*211) if canvas[k:k+3] != b'\x00\x00\x00')
print('PNG stdout  %dx%d  非黑采样 %d/%d' % (w, h, nz, len(range(0, len(canvas), 3*211))), file=sys.stderr)
s.close()
