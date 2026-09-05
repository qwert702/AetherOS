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
    },
    /// 查询系统状态（CPU/内存/磁盘/服务列表等）。
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
    /// 桌面行为指令（aetherd → Shell/合成器）：切换布局、开关通知等。
    /// Shell 收到后执行本地动作，这是"AI 操作桌面"的正道。
    Action {
        name: String,
        arguments: serde_json::Value,
    },
    Error {
        code: u32,
        message: String,
    },
}

/// aetherd serve 的默认监听端口。
pub const DEFAULT_PORT: u16 = 7311;

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
}
