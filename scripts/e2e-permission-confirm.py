"""L2+ 权限确认链路的端到端验证（真实 IPC，不 mock）。

前置：
  1. cargo build -p aetherd --offline
  2. ./target/debug/aetherd serve      # 监听 127.0.0.1:7311
  3. python scripts/e2e-permission-confirm.py

UI 通道密钥：优先 AETHER_UI_KEY 环境变量，否则读 /var/log/aether/ui.key
（aetherd 首次启动时生成）。

验证的语义（对应 docs/ai-permissions.md 与代码审查 P0-4 / P1-8 / P1-9）：
  1. 已注册 UI 通道、无令牌的 L3 ToolCall → NeedsConfirmation（含等级/回显目标/后果/令牌）
  2. 已注册通道带令牌重发                → 放行到工具体（非权限拦截）
  3. 重复使用同一令牌                    → 403（一次性）
  4. 用令牌但篡改参数                    → 403（令牌绑定参数）
  5. 无令牌的 L1 工具                    → 照常执行，不受确认机制影响
  6. **未注册连接带令牌兑现**            → 403（令牌绑定确认方；P1-8 回归断言）
  7. **未注册连接触发 L2+**              → 403（不下发令牌）
  8. **用户拒绝后令牌被撤销**            → ConfirmCancelled，且该令牌不可再兑现（P1-9 回归）
  9. **拒绝后同一操作再次请求**          → 直接拒绝，不再弹确认（Denied 可达）
"""
import json
import os
import socket
import sys

PORT = 7311
DISK = {"disk": "/dev/vda", "confirm": "/dev/vda"}

fails = []


def ui_key():
    k = os.environ.get("AETHER_UI_KEY", "").strip()
    if k:
        return k
    try:
        with open("/var/log/aether/ui.key", encoding="utf-8") as f:
            k = f.read().strip()
        return k or None
    except OSError:
        return None


KEY = ui_key()


def rpc(reqs, register=True):
    """在一条连接上（可选先注册 UI）逐个请求逐行读响应，返回响应 dict 列表。"""
    out = []
    s = socket.create_connection(("127.0.0.1", PORT), timeout=10)
    f = s.makefile("rw", encoding="utf-8", newline="\n")
    if register:
        f.write(json.dumps({"type": "register_ui", "payload": {"key": KEY}}) + "\n")
        f.flush()
        reg = json.loads(f.readline())
        if reg.get("type") != "ui_registered":
            s.close()
            raise SystemExit(f"UI 通道注册失败（{reg}）；请确认 AETHER_UI_KEY 或 /var/log/aether/ui.key")
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
    if not KEY:
        print("找不到 UI 通道密钥：请设置 AETHER_UI_KEY 或确认 aetherd 已生成 /var/log/aether/ui.key")
        return 1

    # 1. 已注册通道、无令牌 → 必须拦下并下发结构化确认请求
    r = rpc([toolcall(DISK)])[0]
    p = r.get("payload", {})
    check("1 无令牌拦下", r["type"] == "needs_confirmation", f"实得 {r['type']}")
    check("1a L3 标记", p.get("level") == 3, f"level={p.get('level')}")
    check("1b 要求回显目标值", p.get("echo_required") == "/dev/vda", f"echo_required={p.get('echo_required')}")
    check("1c 有后果说明", bool(p.get("consequence")), f"consequence={(p.get('consequence') or '')[:24]}...")
    check("1d 令牌是 128 位 hex", len(p.get("token", "")) == 32, f"len={len(p.get('token', ''))}")
    check("1e 参数明文回传", p.get("arguments") == DISK, f"args={p.get('arguments')}")
    token = p.get("token", "")

    # 2. 已注册通道带令牌重发 → 闸门放行（工具体在 Windows 上会失败，但已在闸门之后）
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

    # 6. P1-8 回归：未注册连接不得兑现他人触发的令牌
    r = rpc([toolcall(DISK)])[0]
    tok3 = r.get("payload", {}).get("token", "")
    r = rpc([toolcall(DISK, tok3)], register=False)[0]
    check(
        "6 未注册连接不得兑现令牌",
        r["type"] == "error" and r["payload"].get("code") == 403,
        f"实得 {r}",
    )

    # 7. P1-8 回归：未注册连接不得触发 L2+（令牌根本不下发）
    r = rpc([toolcall(DISK)], register=False)[0]
    check(
        "7 未注册连接不得触发 L2+",
        r["type"] == "error" and r["payload"].get("code") == 403,
        f"实得 {r}",
    )

    # 8. P1-9 回归：拒绝 → 令牌被撤销，且不可再兑现
    r = rpc([toolcall(DISK)])[0]
    tok4 = r.get("payload", {}).get("token", "")
    r = rpc([{"type": "confirm_cancel", "payload": {"token": tok4}}])[0]
    check("8 拒绝回执", r["type"] == "confirm_cancelled", f"实得 {r}")
    r = rpc([toolcall(DISK, tok4)])[0]
    check(
        "8a 被拒绝的令牌不可再兑现",
        r["type"] == "error" and r["payload"].get("code") == 403,
        f"实得 {r}",
    )

    # 9. P1-9 回归：拒绝后同一操作直接拒绝（Denied 可达），不再弹确认
    r = rpc([toolcall(DISK)])[0]
    p = r.get("payload", {})
    denied = r["type"] == "tool_result" and not p.get("ok") and "DENIED" in (p.get("output") or "")
    check("9 拒绝后同操作直接 Denied", denied, f"实得 {r}")

    print()
    print("结果：", "全部通过" if not fails else f"失败 {len(fails)} 项: {fails}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
