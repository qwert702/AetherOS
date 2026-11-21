#!/usr/bin/env python3
"""R8 真实 LLM 端到端复测：经 aetherd IPC 发起一次真实对话 + 一次工具调用。

## 测什么

1. **网关探针**：用环境变量里的 OpenAI 兼容凭据直连一次 `/chat/completions`，
   证明凭据、模型名、网关本身能给出真实回复（不经过 aetherd）；
2. **IPC 对话**：经 aetherd IPC 发 `chat`，收 `chat_chunk`，校验有真实回复；
3. **IPC 工具调用**：发 `tool_call`（L0 只读工具 `sys_probe`），收 `tool_result`，
   校验工具被路由执行、结果回传；
4. **敏感上下文留本地**：带隐私关键词的对话若回传了通道标识，必须是 `local`；
5. **凭据不出本机**：所有 IPC 帧（出站 + 入站）逐帧扫描，不得出现 API Key 原文。

## 环境变量（优先级：`AETHER_LLM_*` 优先，回落到 aetherd 既有约定 `AETHER_*`）

| 变量 | 回落 | 说明 |
|---|---|---|
| `AETHER_LLM_BASE_URL` | `AETHER_API_BASE` | OpenAI 兼容 Base URL，如 `https://open.bigmodel.cn/api/paas/v4` |
| `AETHER_LLM_API_KEY` | `AETHER_API_KEY` | 网关 API Key（只用于本机探针，不进 IPC 帧） |
| `AETHER_LLM_MODEL` | `AETHER_MODEL` | 模型名，默认 `glm-4-flash` |

## 退出码（被 CI / 门禁脚本解析，不要随便改）

* `0` —— 全部断言 PASS；
* `1` —— 断言失败（回复为空 / 工具未执行 / 通道越界 / 凭据泄露等）；
* `2` —— 未配置凭据，**未执行**（不是失败，是没有条件跑）；
* `3` —— 连接失败（网关或 aetherd IPC 连不上）。

只依赖 Python 3 标准库；IPC 线协议见 `aether-ipc/src/lib.rs`。
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.request

DEFAULT_PORT = 7311  # aether_ipc::DEFAULT_PORT（QEMU hostfwd 时用 --port 映射端口）
DEFAULT_MODEL = "glm-4-flash"
READ_TIMEOUT = 180
CONNECT_TIMEOUT = 10

EXIT_PASS = 0
EXIT_ASSERT = 1
EXIT_NO_CRED = 2
EXIT_CONNECT = 3


def env_first(*names: str) -> str:
    for n in names:
        v = os.environ.get(n)
        if v is not None and v.strip():
            return v.strip()
    return ""


class FrameError(Exception):
    """连接 / 协议层错误 —— 映射到退出码 3。"""


class AssertFail(Exception):
    """断言失败 —— 映射到退出码 1。"""


def gateway_probe(base_url: str, api_key: str, model: str) -> str:
    """直连网关做一次最小补全，返回回复正文。任何连接 / 协议问题抛 FrameError。"""
    url = base_url.rstrip("/") + "/chat/completions"
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": "用一句话打招呼（不超过 20 字）。"}],
        "temperature": 0.4,
    }).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=CONNECT_TIMEOUT) as resp:
            raw = resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        detail = e.read().decode("utf-8", "replace")[:512]
        raise FrameError(f"网关 HTTP {e.code}：{detail}") from e
    except (urllib.error.URLError, OSError) as e:
        raise FrameError(f"网关连不上（{url}）：{e}") from e

    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        raise FrameError(f"网关响应不是 JSON（可能被网关拦截）：{raw[:200]!r}") from e
    message = (
        data.get("choices", [{}])[0].get("message", {})
    )
    content = (message.get("content") or "").strip()
    if not content:
        raise AssertFail("网关回复为空（choices[0].message.content 没有内容）")
    return content


class Ipc:
    """最小 NDJSON over IPC 客户端（TCP 或 Unix socket）。"""

    def __init__(self, host: str, port: int, socket_path: str, timeout: float):
        self._sock: socket.socket | None = None
        self._buf = b""
        self.timeout = timeout
        try:
            if socket_path:
                self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self._sock.settimeout(CONNECT_TIMEOUT)
                self._sock.connect(socket_path)
            else:
                self._sock = socket.create_connection((host, port), timeout=CONNECT_TIMEOUT)
                self._sock.settimeout(self.timeout)
        except OSError as e:
            raise FrameError(f"IPC 连不上（{host}:{port} / {socket_path}）：{e}") from e

    def send(self, frame: dict) -> None:
        assert self._sock is not None
        line = (json.dumps(frame, ensure_ascii=False) + "\n").encode("utf-8")
        try:
            self._sock.sendall(line)
        except OSError as e:
            raise FrameError(f"IPC 发送失败：{e}") from e

    def recv_line(self) -> dict:
        """读一帧 NDJSON；超时 / 断开抛 FrameError。"""
        assert self._sock is not None
        while b"\n" not in self._buf:
            try:
                chunk = self._sock.recv(65536)
            except socket.timeout as e:
                raise FrameError("IPC 读超时") from e
            except OSError as e:
                raise FrameError(f"IPC 读取失败：{e}") from e
            if not chunk:
                raise FrameError("IPC 对端已关闭连接")
            self._buf += chunk
        line, self._buf = self._buf.split(b"\n", 1)
        line = line.strip()
        if not line:
            return self.recv_line()
        try:
            return json.loads(line.decode("utf-8"))
        except json.JSONDecodeError as e:
            raise FrameError(f"IPC 帧不是合法 JSON：{line[:200]!r}") from e

    def close(self) -> None:
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass


def leak_scan(label: str, frame_text: str, api_key: str) -> None:
    """凭据不得出现在任何 IPC 帧里（出站 / 入站都扫）。"""
    if api_key and api_key in frame_text:
        raise AssertFail(f"{label}：API Key 原文出现在 IPC 帧里（凭据泄露）")


def wait_chat(ipc: Ipc, session_id: str, api_key: str) -> tuple[str, list[str]]:
    """等到本轮 chat 的 done 帧，拼回完整回复；回传通道标识列表。"""
    deltas: list[str] = []
    channels: list[str] = []
    deadline = time.time() + READ_TIMEOUT
    while time.time() < deadline:
        frame = ipc.recv_line()
        leak_scan("入站帧", json.dumps(frame, ensure_ascii=False), api_key)
        ftype = frame.get("type")
        payload = frame.get("payload") or {}
        if ftype == "chat_chunk":
            delta = payload.get("delta") or ""
            deltas.append(delta)
            if payload.get("channel"):
                channels.append(payload["channel"])
            if payload.get("done"):
                return "".join(deltas).strip(), channels
        elif ftype == "error":
            raise AssertFail(f"aetherd 返回错误：{payload.get('message', frame)}")
        elif ftype == "needs_confirmation":
            raise AssertFail(
                f"L0 对话不该弹确认；却收到 needs_confirmation：tool={payload.get('tool')}"
            )
        else:
            # 其它帧（action 等）忽略，继续等 done。
            continue
    raise FrameError("等待 chat_chunk done 超时")


def wait_tool_result(ipc: Ipc, tool: str, api_key: str) -> dict:
    deadline = time.time() + READ_TIMEOUT
    while time.time() < deadline:
        frame = ipc.recv_line()
        leak_scan("入站帧", json.dumps(frame, ensure_ascii=False), api_key)
        ftype = frame.get("type")
        payload = frame.get("payload") or {}
        if ftype == "tool_result":
            if payload.get("tool") != tool:
                raise AssertFail(f"工具结果不匹配：期望 {tool}，实得 {payload.get('tool')}")
            return payload
        if ftype == "error":
            raise AssertFail(f"aetherd 返回错误：{payload.get('message', frame)}")
        if ftype == "needs_confirmation":
            raise AssertFail(
                f"工具 {tool} 被权限闸门拦下（needs_confirmation）；L0 只读工具不应如此"
            )
    raise FrameError("等待 tool_result 超时")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="R8 真实 LLM E2E：网关探针 + aetherd IPC 对话 + 工具调用复测",
        epilog=(
            "环境变量：AETHER_LLM_BASE_URL/AETHER_LLM_API_KEY/AETHER_LLM_MODEL"
            "（回落 AETHER_API_BASE/AETHER_API_KEY/AETHER_MODEL）。\n"
            "退出码：0 PASS · 1 断言失败 · 2 未配置凭据未执行 · 3 连接失败。"
        ),
    )
    ap.add_argument("--host", default="127.0.0.1", help="IPC TCP 地址（默认 127.0.0.1）")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT,
                    help=f"IPC TCP 端口（默认 {DEFAULT_PORT}；QEMU hostfwd 见 m6-toolcall.py）")
    ap.add_argument("--socket", default="", help="Unix socket 路径（给了就不走 TCP）")
    ap.add_argument("--timeout", type=float, default=READ_TIMEOUT, help="单轮读取超时秒数")
    ap.add_argument("--session", default="r8-llm-e2e", help="IPC session_id")
    ap.add_argument("--skip-gateway-probe", action="store_true",
                    help="跳过直连网关探针（仅走 IPC；凭据仍会做存在性检查）")
    args = ap.parse_args(argv)

    base_url = env_first("AETHER_LLM_BASE_URL", "AETHER_API_BASE")
    api_key = env_first("AETHER_LLM_API_KEY", "AETHER_API_KEY")
    model = env_first("AETHER_LLM_MODEL", "AETHER_MODEL") or DEFAULT_MODEL

    # ── 未配置凭据：显式未执行，退出码 2，禁止假装成功 ──────────────────────
    if not base_url or not api_key:
        missing = [n for n, v in (("AETHER_LLM_BASE_URL/AETHER_API_BASE", base_url),
                                   ("AETHER_LLM_API_KEY/AETHER_API_KEY", api_key)) if not v]
        print(f"未配置凭据，未执行：缺少 {', '.join(missing)}")
        print("提示：export AETHER_LLM_BASE_URL=<OpenAI 兼容 Base URL>")
        print("      export AETHER_LLM_API_KEY=<网关 Key>")
        print("      export AETHER_LLM_MODEL=<模型名，可选>")
        return EXIT_NO_CRED

    print(f"[1/5] 网关探针：model={model} base={base_url}")
    try:
        if args.skip_gateway_probe:
            print("      已按 --skip-gateway-probe 跳过直连探针")
            gateway_reply = "(skipped)"
        else:
            gateway_reply = gateway_probe(base_url, api_key, model)
            print(f"      网关真实回复：{gateway_reply[:80]}")
    except FrameError as e:
        print(f"网关连接失败：{e}", file=sys.stderr)
        return EXIT_CONNECT
    except AssertFail as e:
        print(f"网关断言失败：{e}", file=sys.stderr)
        return EXIT_ASSERT

    ipc: Ipc | None = None
    try:
        print("[2/5] 连接 aetherd IPC …")
        ipc = Ipc(args.host, args.port, args.socket, args.timeout)
        ipc.send({"type": "ping"})
        pong = ipc.recv_line()
        if pong.get("type") != "pong":
            raise FrameError(f"未收到 pong，实得：{pong}")
        print("      IPC 握手 OK（pong）")

        # ── 普通对话 ──────────────────────────────────────────────────────
        print("[3/5] IPC 对话（chat）…")
        chat_req = {"type": "chat", "payload": {
            "session_id": args.session, "text": "你好，简单介绍一下你自己。", "en": False,
        }}
        ipc.send(chat_req)
        leak_scan("出站帧", json.dumps(chat_req, ensure_ascii=False), api_key)
        reply, channels = wait_chat(ipc, args.session, api_key)
        if not reply:
            raise AssertFail("对话回复为空（chat_chunk 拼不出正文）")
        print(f"      回复：{reply[:80]}")
        if channels:
            print(f"      通道：{', '.join(channels)}")

        # ── 隐私关键词必须留本地 ────────────────────────────────────────────
        print("[4/5] 敏感上下文路由检查（隐私关键词 → local）…")
        priv_req = {"type": "chat", "payload": {
            "session_id": args.session,
            "text": "帮我生成一个强密码，再保管好这把密钥。",
            "en": False,
        }}
        ipc.send(priv_req)
        leak_scan("出站帧", json.dumps(priv_req, ensure_ascii=False), api_key)
        _, priv_channels = wait_chat(ipc, args.session, api_key)
        if priv_channels and any(c != "local" for c in priv_channels):
            raise AssertFail(
                f"敏感上下文被路由到非本地通道：{priv_channels}（期望 local）"
            )
        print(f"      隐私对话通道：{', '.join(priv_channels) or '(未回传通道标识)'}")

        # ── 工具调用（L0 只读）─────────────────────────────────────────────
        print("[5/5] IPC 工具调用（tool_call: sys_probe/whoami）…")
        tool_req = {"type": "tool_call", "payload": {
            "session_id": args.session,
            "tool": "sys_probe",
            "arguments": {"probe": "whoami"},
        }}
        ipc.send(tool_req)
        leak_scan("出站帧", json.dumps(tool_req, ensure_ascii=False), api_key)
        result = wait_tool_result(ipc, "sys_probe", api_key)
        if not result.get("ok"):
            raise AssertFail(f"工具执行返回 ok=false：{result.get('output', '')}")
        if not (result.get("output") or "").strip():
            raise AssertFail("工具结果 output 为空")
        print(f"      工具结果：{result['output'].strip()[:80]}")

        print("\nR8 LLM-E2E: PASS（真实回复、工具路由、敏感留本地、无凭据泄露）")
        return EXIT_PASS
    except FrameError as e:
        print(f"连接/协议失败：{e}", file=sys.stderr)
        return EXIT_CONNECT
    except AssertFail as e:
        print(f"断言失败：{e}", file=sys.stderr)
        return EXIT_ASSERT
    finally:
        if ipc is not None:
            ipc.close()


if __name__ == "__main__":
    raise SystemExit(main())
