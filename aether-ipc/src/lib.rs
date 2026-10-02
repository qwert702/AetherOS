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
    /// `/run/aether/ui.key`，Unix 下 chmod 0600）。合成器读取同一来源并注册。
    ///
    /// 路径在 2026-10-02 安全审计（H-1）中从 `/var/log/aether/` 迁出：旧位置
    /// 在 `read_file` 的读取白名单内，任意本机进程可用 AI 工具把密钥取走。
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
    /// 电源动作（**不可逆**）：关机 / 重启。
    ///
    /// 与 `ServiceControl` 分开而不是复用（"保留单元名 power"）的理由：电源动作的
    /// 语义、鉴权与后果都和"重启某个服务"不同 —— 它是**整机级且不可逆**的，
    /// 混在服务控制里会让"服务名白名单"这种保护失效（power 不是服务）。
    ///
    /// 谁可以发：只有已注册的 UI 通道，且**必须**经确认卡片（见 `aetherd` 的
    /// `system_power` 工具，L3）。执行在 `aether-init`（PID 1）里：
    /// 停服务 → `sync()` → `reboot(2)`。
    SystemPower {
        action: PowerAction,
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
    /// 电源动作受理回执（`Request::SystemPower` 的应答）。
    ///
    /// 注意语义：`accepted = true` 只表示**受理**，不表示"已经关机了" ——
    /// 关机过程中进程会消失，不可能再回一条"完成"。调用方据此提示用户
    /// "正在关机"，而不是"已关机"。
    PowerAck {
        action: PowerAction,
        accepted: bool,
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

/// aether-init 服务控制通道的 Unix socket 路径（Linux，0600 root-only）。
///
/// 同 `UNIX_SOCKET_PATH` 的理由：**连接侧与监听侧必须一致**。此前这个字面量
/// 在三个 crate 里各写了一份（`aether-init/src/ipc.rs`、`aetherd/src/main.rs`、
/// `aether-ops`），任何一处改动都会变成"连不上"这种最难查的故障。
/// 2026-10-02 功能批次 I 收拢到这里（协议 crate 是"跨组件契约"的家）。
///
/// 权限由 aether-init 在 bind 后设为 0600，且 `peer_uid` 只接受 root/同 uid ——
/// 因此"谁能改系统服务"由内核保证，不依赖调用方自觉。
pub const INIT_SOCKET_PATH: &str = "/run/aether-init.sock";

/// aetherd 的 Unix socket 路径（**默认通道**，0600）。
///
/// 放在协议 crate 里而不是各组件各写一遍：aetherd（监听侧）与合成器（连接侧）
/// 必须用同一条路径，两处字面量一旦漂开就变成"连不上"这种最难查的故障。
/// 0600 由 aetherd 在 bind 后设置；`/run` 属 root，因此非 root 进程既建不了也连不上 ——
/// 这正是 `RegisterUi` 需要的"对端身份"保证（2026-10-02 审计 M-17）。
pub const UNIX_SOCKET_PATH: &str = "/run/aetherd.sock";

/// UI 通道密钥的落盘位置（aetherd 生成、合成器读取，0600）。
///
/// 同 `UNIX_SOCKET_PATH`：两侧必须一致，所以定义在这里。
/// **它不在 AI 的读取白名单内**（`aetherd::tools::READ_DENY_SUBPATHS` 明确拒绝 `ui.key`）——
/// 2026-10-02 审计 H-1：放在 `/var/log/aether/` 时，`read_file` 能把密钥交给任意本机进程。
pub const UI_KEY_PATH: &str = "/run/aether/ui.key";

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

/// 电源动作。
///
/// **没有 `Logout`**：注销 = 重启合成器（`compositor.json` 的 `restart:true` 会让
/// aether-init 拉起它），那是合成器自己的事，不需要经过 PID 1 —— 放进这里
/// 会让人以为"注销"也要整机权限。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    /// 关机（`reboot(RB_POWER_OFF)`）
    PowerOff,
    /// 重启（`reboot(RB_AUTOBOOT)`）
    Reboot,
}

impl PowerAction {
    /// 稳定的字符串标识：**用于回显确认与审计**（两种用途都要求跨版本稳定，
    /// 所以不用 `Debug` 格式 —— 那个会随代码改动而变）。
    pub fn as_str(&self) -> &'static str {
        match self {
            PowerAction::PowerOff => "poweroff",
            PowerAction::Reboot => "reboot",
        }
    }

    /// 从字符串解析（大小写不敏感）。未知值返回 None —— 调用方负责报错。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "poweroff" | "power_off" | "shutdown" => Some(PowerAction::PowerOff),
            "reboot" | "restart" => Some(PowerAction::Reboot),
            _ => None,
        }
    }
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

    /// 电源动作的协议往返 + `as_str`/`parse` 必须互逆。
    ///
    /// `as_str` 用于**回显确认与审计**：如果它和 `parse` 不互逆，就会出现
    /// "卡片让你输入 poweroff、实际却不认"这种自相矛盾的交互。
    #[test]
    fn power_action_roundtrip_and_string_contract() {
        for a in [PowerAction::PowerOff, PowerAction::Reboot] {
            let r = Request::SystemPower { action: a };
            match decode::<Request>(&encode(&r)).unwrap() {
                Request::SystemPower { action } => assert_eq!(action, a),
                other => panic!("应为 SystemPower，实得 {other:?}"),
            }
            assert_eq!(
                PowerAction::parse(a.as_str()),
                Some(a),
                "as_str 与 parse 必须互逆：{a:?}"
            );
        }
        // 大小写与常见别名（用户手输的可能带空格/大写）
        assert_eq!(PowerAction::parse(" PowerOff "), Some(PowerAction::PowerOff));
        assert_eq!(PowerAction::parse("REBOOT"), Some(PowerAction::Reboot));
        // 未知值必须返回 None（调用方据此报错，不能默默当成关机）
        assert_eq!(PowerAction::parse("sleep"), None);
        assert_eq!(PowerAction::parse(""), None);
        // 序列化形态稳定（审计与回显都依赖它）
        assert_eq!(
            serde_json::to_string(&PowerAction::PowerOff).unwrap(),
            "\"power_off\""
        );
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
