# aether-ipc 协议草案 v0

传输：Unix domain socket（`/run/aetherd.sock`），newline-delimited JSON。
每个组件同时充当 client 和/或 server；消息类型定义于 `aether-ipc` crate。

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
