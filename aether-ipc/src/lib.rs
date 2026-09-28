//! aether-ipc — AetherOS 全系统通信协议。
//!
//! 所有组件（aether-shell、aetherd、aether-ops、aether-init）之间
//! 通过这里定义的消息类型通信。传输层当前为 JSON over Unix socket，
//! 未来可平滑升级为二进制编码，消息类型保持不变。

use serde::{Deserialize, Serialize};

/// 客户端 → 系统服务的请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Request {
    /// 存活探测。
    Ping,
    /// 与 AI 中枢对话。
    Chat {
        session_id: String,
        text: String,
    },
    /// 请求 AI 执行一个已授权的工具调用。
    ToolCall {
        session_id: String,
        tool: String,
        arguments: serde_json::Value,
        /// 用户确认令牌：由 aetherd 在 `Response::NeedsConfirmation` 里签发，
        /// UI 在用户点"允许一次"后原样带回。缺省（默认）表示未经确认——
        /// L2+ 工具会被权限闸门拦下。令牌只发给 IPC 客户端，不进 LLM 上下文，
        /// 因此 AI 无法自我授权。
        ///
        /// **只有已注册的 UI 通道**（见 `Request::RegisterUi`）才能兑现令牌；
        /// 未注册连接带令牌一律 403。这是"令牌绑定确认方"的落地点：令牌本身
        /// 不再等价于放行权，必须同时来自可信的确认通道。
        #[serde(default)]
        approval: Option<String>,
    },
    /// 注册为 UI（交互式确认）通道。
    ///
    /// 确认令牌的兑现权与"已注册"绑定：aetherd 在启动时确定一个 UI 密钥
    /// （`AETHER_UI_KEY` 环境变量优先，否则生成随机值并写入
    /// `<审计目录>/ui.key`，Unix 下 chmod 0600）。合成器读取同一来源并注册。
    ///
    /// 这挡不住"能读到密钥的本机进程"，但它把攻击面从"任何能连 7311 的进程"
    /// 收窄到"能读到 0600 密钥文件的进程"，并且让"确认方"成为服务端可断言的事实。
    RegisterUi {
        /// UI 密钥。
        key: String,
    },
    /// 用户在确认卡片上点了"拒绝"：撤销令牌并留痕。
    ///
    /// 没有这条消息时，"拒绝"只存在于合成器的本地状态里，服务端的令牌会
    /// 继续存活到 TTL 到期，审计日志也无法区分"用户拒绝"与"从未回答"。
    ConfirmCancel {
        /// 被拒绝的确认令牌（原样回传）。
        token: String,
    },
    /// 查询系统状态（CPU/内存/磁盘/服务列表等）。
    /// 写入跨进程剪贴板（上限见 aetherd clipboard::MAX_BYTES；超长整体拒绝）。
    ClipboardSet {
        text: String,
    },
    /// 读取跨进程剪贴板。
    ClipboardGet,
    /// 重新加载模型配置（0.6）。
    ///
    /// **只有已注册的 UI 通道**能发起（改配置等于改 AI 的能力边界）。
    /// aetherd 收到后会**退出进程**，由 aether-init 带新配置重新拉起 ——
    /// 不做热替换：让"配置生效"这条路径只有一条，不会出现"改了但只改了一半"。
    ReloadConfig,
    SysInfo {
        scope: SysInfoScope,
    },
    /// 向 init 请求启动/停止/重启一个服务。
    ServiceControl {
        unit: String,
        action: ServiceAction,
    },
}

/// 系统服务 → 客户端的响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Response {
    Pong,
    /// AI 的文本回复（流式场景下会多次下发增量）。
    ChatChunk {
        session_id: String,
        delta: String,
        done: bool,
        /// 本轮回答实际使用的推理通道（"local"/"cloud"；None = 旧客户端/未知，
        /// 顶栏 AI 三态据此渲染）。`#[serde(default)]` 保证向后兼容：
        /// 旧客户端收新包、新客户端收旧包都不崩。
        #[serde(default)]
        channel: Option<String>,
    },
    ToolResult {
        tool: String,
        ok: bool,
        output: String,
    },
    SysInfo(SysReport),
    ServiceAck {
        unit: String,
        ok: bool,
        message: String,
    },
    /// 剪贴板内容（`Request::ClipboardGet` 的应答）。
    ClipboardText {
        text: String,
    },
    /// 剪贴板写入确认（`Request::ClipboardSet` 的应答），`bytes` 为写入字节数。
    ClipboardWritten {
        bytes: usize,
    },
    /// 配置重载已受理（aetherd 即将退出，由 aether-init 用新配置重启它）。
    ConfigReloading,
    /// 桌面行为指令（aetherd → Shell/合成器）：切换布局、开关通知等。
    /// Shell 收到后执行本地动作，这是"AI 操作桌面"的正道。
    Action {
        name: String,
        arguments: serde_json::Value,
    },
    /// L2+ 操作待用户确认：UI 应弹确认卡片，展示参数与后果；
    /// 用户允许后带 `approval` 令牌重发 `Request::ToolCall`。
    NeedsConfirmation {
        tool: String,
        /// 权限等级：2 = L2 敏感写，3 = L3 危险
        level: u8,
        /// 参数明文（原样展示给用户，不做美化）
        arguments: serde_json::Value,
        /// 一句话后果说明
        consequence: String,
        /// L3 需回显确认的**目标值**（如 "/dev/vda"）；None = 只需点确认
        echo_required: Option<String>,
        /// 一次性确认令牌（5 分钟内有效，用后即废）
        token: String,
    },
    /// UI 通道注册成功：此后本连接可以兑现确认令牌。
    UiRegistered,
    /// 用户拒绝确认的回执（令牌已撤销，审计已落 `denied_by_user`）。
    ConfirmCancelled {
        /// 被撤销的令牌
        token: String,
    },
    Error {
        code: u32,
        message: String,
    },
}

/// aetherd serve 的默认监听端口。
pub const DEFAULT_PORT: u16 = 7311;

/// aether-init 服务控制（PID 1）的监听端口。
pub const INIT_PORT: u16 = 7312;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SysInfoScope {
    Cpu,
    Memory,
    Disk,
    Network,
    Services,
    All,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Status,
}

/// 系统状态汇报（M4/M5 由 aetherd 与 aether-ops 填充真实数据）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SysReport {
    pub cpu_percent: Option<f32>,
    pub mem_used_mb: Option<u64>,
    pub mem_total_mb: Option<u64>,
    pub uptime_secs: Option<u64>,
    pub services: Vec<ServiceStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub unit: String,
    pub state: String,
    pub pid: Option<u32>,
    /// 服务定义的重启策略：巡检方据此统一 restart 行为（默认 true 兼容旧端点）
    #[serde(default = "default_true")]
    pub restart: bool,
    /// 关键服务标记：崩溃时告警升级
    #[serde(default)]
    pub essential: bool,
}

fn default_true() -> bool {
    true
}

/// 把一条请求编码为一行 JSON（newline-delimited JSON，socket 帧协议）。
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).expect("aether-ipc: 序列化失败");
    s.push('\n');
    s
}

/// 从一行 JSON 解码一条消息。
pub fn decode<'a, T: Deserialize<'a>>(line: &'a str) -> anyhow::Result<T> {
    Ok(serde_json::from_str(line.trim())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request::Chat {
            session_id: "s1".into(),
            text: "把窗口排成两列".into(),
        };
        let line = encode(&req);
        assert!(line.ends_with('\n'));
        let back: Request = decode(&line).unwrap();
        assert_eq!(
            serde_json::to_value(&req).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn response_decodes() {
        let r: Response = decode(r#"{"type":"pong","payload":null}"#).unwrap();
        assert!(matches!(r, Response::Pong));
    }

    #[test]
    fn chat_chunk_roundtrip_with_channel() {
        let r = Response::ChatChunk {
            session_id: "s1".into(),
            delta: "好的".into(),
            done: true,
            channel: Some("local".into()),
        };
        let back: Response = decode(&encode(&r)).unwrap();
        match back {
            Response::ChatChunk { channel, delta, done, .. } => {
                assert_eq!(channel.as_deref(), Some("local"));
                assert_eq!(delta, "好的");
                assert!(done);
            }
            other => panic!("应为 ChatChunk，实得 {other:?}"),
        }
    }

    #[test]
    fn chat_chunk_without_channel_decodes_to_none() {
        // 向后兼容：旧服务端下发的无 channel 包也能解析
        let r: Response = decode(r#"{"type":"chat_chunk","payload":{"session_id":"s1","delta":"hi","done":false}}"#).unwrap();
        match r {
            Response::ChatChunk { channel, .. } => assert_eq!(channel, None),
            other => panic!("应为 ChatChunk，实得 {other:?}"),
        }
    }

    #[test]
    fn ui_registration_and_cancel_roundtrip() {
        // 令牌绑定确认方（P1-8）与拒绝通路（P1-9）的协议面
        let r = Request::RegisterUi { key: "deadbeef".into() };
        match decode::<Request>(&encode(&r)).unwrap() {
            Request::RegisterUi { key } => assert_eq!(key, "deadbeef"),
            other => panic!("应为 RegisterUi，实得 {other:?}"),
        }
        let c = Request::ConfirmCancel { token: "abc".into() };
        match decode::<Request>(&encode(&c)).unwrap() {
            Request::ConfirmCancel { token } => assert_eq!(token, "abc"),
            other => panic!("应为 ConfirmCancel，实得 {other:?}"),
        }
        let u = Response::UiRegistered;
        assert!(matches!(decode::<Response>(&encode(&u)).unwrap(), Response::UiRegistered));
        let cc = Response::ConfirmCancelled { token: "abc".into() };
        match decode::<Response>(&encode(&cc)).unwrap() {
            Response::ConfirmCancelled { token } => assert_eq!(token, "abc"),
            other => panic!("应为 ConfirmCancelled，实得 {other:?}"),
        }
    }

    /// 剪贴板请求/应答的编解码往返。
    ///
    /// 单独成测试而不是塞进上面的 UI 注册用例：失败时的报错才会指向正确的地方
    /// （第四轮审查 P3-5 —— 之前它被插在 `ui_registration_and_cancel_roundtrip`
    /// 里，名字与内容不符）。
    #[test]
    fn clipboard_roundtrip() {
        let req = Request::ClipboardSet { text: "跨进程\n内容".into() };
        match decode::<Request>(&encode(&req)).unwrap() {
            Request::ClipboardSet { text } => assert_eq!(text, "跨进程\n内容"),
            other => panic!("应为 ClipboardSet，实得 {other:?}"),
        }
        match decode::<Request>(&encode(&Request::ClipboardGet)).unwrap() {
            Request::ClipboardGet => {}
            other => panic!("应为 ClipboardGet，实得 {other:?}"),
        }
        let resp = Response::ClipboardText { text: "abc".into() };
        match decode::<Response>(&encode(&resp)).unwrap() {
            Response::ClipboardText { text } => assert_eq!(text, "abc"),
            other => panic!("应为 ClipboardText，实得 {other:?}"),
        }
        let resp = Response::ClipboardWritten { bytes: 42 };
        match decode::<Response>(&encode(&resp)).unwrap() {
            Response::ClipboardWritten { bytes } => assert_eq!(bytes, 42),
            other => panic!("应为 ClipboardWritten，实得 {other:?}"),
        }
    }

    #[test]
    fn tool_call_without_approval_decodes_to_none() {
        // 向后兼容：不带 approval 字段的 ToolCall 仍可解析（默认 None）
        let r: Request = decode(
            r#"{"type":"tool_call","payload":{"session_id":"s","tool":"read_file","arguments":{"path":"/tmp/a"}}}"#,
        )
        .unwrap();
        match r {
            Request::ToolCall { approval, tool, .. } => {
                assert_eq!(approval, None);
                assert_eq!(tool, "read_file");
            }
            other => panic!("应为 ToolCall，实得 {other:?}"),
        }
    }
}
