#!/usr/bin/env python3
"""分时点击：btn down 与 up 分两次 QMP 命令、间隔数秒（跨渲染帧）。"""
import json
import socket
import sys
import time

delay = float(sys.argv[1]) if len(sys.argv) > 1 else 8.0

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(5)
s.connect("/home/aether/qmp.sock")
time.sleep(0.3)
s.recv(65536)


def cmd(m):
    s.sendall(json.dumps(m).encode())
    time.sleep(0.3)
    try:
        return s.recv(65536).decode().strip()
    except Exception as e:
        return f"ERR {e}"


print("caps:", cmd({"execute": "qmp_capabilities"}))
print("down:", cmd({"execute": "input-send-event", "arguments": {"events": [
    {"type": "btn", "data": {"down": True, "button": "left"}}]}}))
time.sleep(delay)
print("up:", cmd({"execute": "input-send-event", "arguments": {"events": [
    {"type": "btn", "data": {"down": False, "button": "left"}}]}}))
s.close()
