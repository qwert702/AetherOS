"""L2+ 权限确认链路的端到端验证（真实 IPC，不 mock）。

前置：
  1. cargo build -p aetherd
  2. ./target/debug/aetherd serve      # 监听 127.0.0.1:7311
  3. python scripts/e2e-permission-confirm.py

验证的语义（对应 docs/ui-design-handover.md §2 Step 4a）：
  1. 无令牌的 L3 ToolCall        → 期望 NeedsConfirmation（含等级/回显目标/后果/令牌）
  2. 带令牌重发                  → 期望放行到工具体（非权限拦截）
  3. 重复使用同一令牌            → 期望 403（一次性）
  4. 用令牌但篡改参数            → 期望 403（令牌绑定参数）
  5. 无令牌的 L1 工具            → 期望照常执行，不受确认机制影响
"""
import json
import socket
import sys

PORT = 7311
DISK = {"disk": "/dev/vda", "confirm": "/dev/vda"}

fails = []


def rpc(reqs):
    """逐个请求逐行读响应，返回响应 dict 列表。"""
    out = []
    s = socket.create_connection(("127.0.0.1", PORT), timeout=10)
    f = s.makefile("rw", encoding="utf-8", newline="\n")
    for r in reqs:
        f.write(json.dumps(r) + "\n")
        f.flush()
        line = f.readline()
        if not line:
            break
        out.append(json.loads(line))
    s.close()
    return out


def toolcall(args, approval=None, tool="install_disk"):
    payload = {"session_id": "e2e", "tool": tool, "arguments": args}
    if approval is not None:
        payload["approval"] = approval
    return {"type": "tool_call", "payload": payload}


def check(name, cond, detail):
    print(f"{'PASS' if cond else 'FAIL'}  {name}: {detail}")
    if not cond:
        fails.append(name)


def main():
    # 1. 无令牌 → 必须拦下并下发结构化确认请求
    r = rpc([toolcall(DISK)])[0]
    p = r.get("payload", {})
    check("1 无令牌拦下", r["type"] == "needs_confirmation", f"实得 {r['type']}")
    check("1a L3 标记", p.get("level") == 3, f"level={p.get('level')}")
    check("1b 要求回显目标值", p.get("echo_required") == "/dev/vda", f"echo_required={p.get('echo_required')}")
    check("1c 有后果说明", bool(p.get("consequence")), f"consequence={(p.get('consequence') or '')[:24]}...")
    check("1d 令牌是 128 位 hex", len(p.get("token", "")) == 32, f"len={len(p.get('token', ''))}")
    check("1e 参数明文回传", p.get("arguments") == DISK, f"args={p.get('arguments')}")
    token = p.get("token", "")

    # 2. 带令牌重发 → 闸门放行（工具体在 Windows 上会失败，但已在闸门之后）
    r = rpc([toolcall(DISK, token)])[0]
    p = r.get("payload", {})
    check("2 令牌放行到工具体", r["type"] == "tool_result", f"实得 {r['type']}")
    out = p.get("output") or ""
    check("2a 拦截来自工具体而非权限", "安装器运行失败" in out or "aether-install" in out, f"output={out[:60]!r}")

    # 3. 重放同一令牌 → 一次性
    r = rpc([toolcall(DISK, token)])[0]
    check("3 重放被拒", r["type"] == "error" and r["payload"].get("code") == 403, f"实得 {r}")
    if r["type"] == "error":
        check("3a 提示已被使用", "已被使用" in r["payload"].get("message", ""), r["payload"].get("message", ""))

    # 4. 新令牌 + 篡改参数 → 绑定校验失败
    r = rpc([toolcall(DISK)])[0]
    tok2 = r.get("payload", {}).get("token", "")
    other = {"disk": "/dev/sda", "confirm": "/dev/sda"}
    r = rpc([toolcall(other, tok2)])[0]
    check("4 换参数被拒", r["type"] == "error" and r["payload"].get("code") == 403, f"实得 {r}")
    if r["type"] == "error":
        check("4a 提示参数不匹配", "参数不匹配" in r["payload"].get("message", ""), r["payload"].get("message", ""))

    # 5. L1 工具无令牌 → 不受确认机制影响
    r = rpc([toolcall({"action": "layout_set", "layout": "two_col"}, tool="desktop")])
    kinds = [x["type"] for x in r]
    check("5 L1 不受确认机制影响", all(k != "needs_confirmation" for k in kinds), f"{kinds}")

    print()
    print("结果：", "全部通过" if not fails else f"失败 {len(fails)} 项: {fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
