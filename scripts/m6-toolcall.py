#!/usr/bin/env python3
"""M6 e2e：经 hostfwd 直调 guest aetherd 的 install_disk 工具，回读结果。"""
import json
import socket
import sys
import time

HOST, PORT = "127.0.0.1", 17311

s = socket.create_connection((HOST, PORT), timeout=10)
s.settimeout(180)
req = {"type": "tool_call", "payload": {
    "session_id": "m6-test",
    "tool": "install_disk",
    "arguments": {"disk": "/dev/vda"},
}}
s.sendall((json.dumps(req) + "\n").encode())
buf = b""
start = time.time()
while time.time() - start < 170:
    try:
        chunk = s.recv(65536)
    except socket.timeout:
        break
    if not chunk:
        break
    buf += chunk
    if b"\n" in buf:
        break
print("响应:", buf.decode(errors="replace").strip() or "（无响应/超时）")
s.close()
