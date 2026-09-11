#!/usr/bin/env python3
"""QMP 验证助手（M4 输入链路）：向 guest 注入键盘/鼠标事件并截图。
用法:
  qmp-verify.py key <qcode>...            # 逐键发送（qcode: t h r e e ret 等）
  qmp-verify.py mouse <dx> <dy> [click]   # 相对位移，可选左键单击
  qmp-verify.py shot > out.png            # screendump → PNG（stdout）
"""
import io
import json
import os
import socket
import struct
import sys
import time
import zlib

SOCK = '/home/aether/qmp.sock'


class QMP:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.settimeout(10)
        self.s.connect(SOCK)
        self._read()                       # 问候横幅
        self.cmd('qmp_capabilities')

    def _read(self):
        buf = b''
        while b'\n' not in buf:
            buf += self.s.recv(65536)
        return [json.loads(l) for l in buf.split(b'\n') if l.strip()]

    def cmd(self, name, args=None):
        m = {'execute': name}
        if args:
            m['arguments'] = args
        self.s.sendall(json.dumps(m).encode())
        time.sleep(0.2)
        try:
            return self._read()
        except Exception:
            return None


def main():
    op = sys.argv[1]
    q = QMP()
    if op == 'key':
        for k in sys.argv[2:]:
            q.cmd('send-key', {'keys': [{'type': 'qcode', 'data': k}]})
            time.sleep(0.15)
        print('keys sent:', ' '.join(sys.argv[2:]))
    elif op == 'mouse':
        dx, dy = int(sys.argv[2]), int(sys.argv[3])
        events = [{'type': 'rel', 'data': {'axis': 'x', 'value': dx}},
                  {'type': 'rel', 'data': {'axis': 'y', 'value': dy}}]
        if len(sys.argv) > 4 and sys.argv[4] == 'click':
            events += [{'type': 'btn', 'data': {'down': True, 'button': 'left'}},
                       {'type': 'btn', 'data': {'down': False, 'button': 'left'}}]
        q.cmd('input-send-event', {'events': events})
        print('mouse sent:', dx, dy)
    elif op == 'shot':
        ppm = '/home/aether/shot.ppm'
        if os.path.exists(ppm):
            os.unlink(ppm)
        q.cmd('screendump', {'filename': ppm})
        for _ in range(20):
            if os.path.exists(ppm) and os.path.getsize(ppm) > 1000:
                break
            time.sleep(0.5)
        else:
            sys.exit('screendump 未生成')
        d = open(ppm, 'rb').read()

        def tok(i):
            while d[i:i + 1].isspace():
                i += 1
            j = i
            while j < len(d) and not d[j:j + 1].isspace():
                j += 1
            return d[i:j], j

        w, i = tok(2)
        h, i = tok(i)
        i += 1
        w, h = int(w), int(h)
        px = d[i:i + w * h * 3]
        rows = bytearray()
        for y in range(h):
            rows.append(0)
            rows += px[y * w * 3:(y + 1) * w * 3]

        def chunk(tag, data):
            return struct.pack('>I', len(data)) + tag + data + \
                struct.pack('>I', zlib.crc32(tag + data) & 0xffffffff)

        sys.stdout.buffer.write(
            b'\x89PNG\r\n\x1a\n' +
            chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) +
            chunk(b'IDAT', zlib.compress(bytes(rows), 6)) +
            chunk(b'IEND', b''))
        print(f'PNG {w}x{h} -> stdout', file=sys.stderr)
    else:
        sys.exit('unknown op')


main()
