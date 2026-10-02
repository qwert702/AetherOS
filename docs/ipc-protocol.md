# aether-ipc 协议草案 v0

> ⚠️ **本文是 M0 的草案，不是现状依据**（2026-10-02 校正）。
>
> 传输有**两条**（aetherd 同时监听，请求处理逻辑共用同一段代码）：
> * **`/run/aetherd.sock`（Unix socket，0600）** —— 客户端默认走这条。
>   对端身份由文件权限保证（`/run` 属 root），这是 `RegisterUi` 需要的保证：
>   TCP 上客户端**无法验证对端**，谁抢到端口谁就能收到 UI 密钥（审计 M-17）。
> * **TCP `127.0.0.1:7311`** —— 保留给开发机预览与 hostfwd 调试；客户端只有在
>   显式设 `AETHER_IPC_TCP=1` 时才会用它，因为那意味着密钥走明文 TCP。
>   aether-init 的控制通道是另一条：`/run/aether-init.sock`（0600，见 `aether-init/src/ipc.rs`）。
>
> 权威定义请看 `aether-ipc/src/lib.rs`：10 个 `Request` 变体、12 个 `Response` 变体；
> 本文下面的表只是 M0 时的**最小子集**（缺 `RegisterUi` / `ConfirmCancel` /
> `Clipboard*` / `ReloadConfig` 等后加的部分）。总览见 `INDEX.md`「组件与入口」。

传输：newline-delimited JSON（NDJSON）。每个组件同时充当 client 和/或 server；
消息类型定义于 `aether-ipc` crate。

## 请求（Request）

| type | payload | 说明 |
|---|---|---|
| `ping` | — | 存活探测 |
| `chat` | `session_id, text` | 与 AI 对话 |
| `tool_call` | `session_id, tool, arguments` | 请求执行已授权工具 |
| `sys_info` | `scope` | 查询 CPU/内存/磁盘/网络/服务 |
| `service_control` | `unit, action` | 控制系统服务 |

## 响应（Response）

| type | payload | 说明 |
|---|---|---|
| `pong` | — | 探测回应 |
| `chat_chunk` | `session_id, delta, done` | AI 回复（流式增量） |
| `tool_result` | `tool, ok, output` | 工具执行结果 |
| `sys_info` | SysReport | 系统状态 |
| `service_ack` | `unit, ok, message` | 服务控制回执 |
| `error` | `code, message` | 错误 |

## 约定

1. 所有 JSON 键名 snake_case；枚举值 snake_case
2. 请求-响应一一对应；`chat_chunk` 是唯一的多播流式响应
3. 协议版本：握手阶段首条消息携带 `{"type":"ping"}`，未来升级时引入 `hello{version}`
4. 任何未知 `type` 必须返回 `error{code:1, message:"unknown type"}`，保证前后兼容
