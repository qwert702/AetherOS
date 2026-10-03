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
    /// 写入一项系统设置（**会改系统配置**，必须经确认）。
    ///
    /// 键是 [`SettingKey`] 白名单（不是任意字符串）：未知键在**解码阶段**就被拒绝 ——
    /// "任何字符串都能当键"的实现里，键名会一路流到拼路径/拼命令的地方，
    /// 那正是路径遍历与配置注入的入口。
    SetSetting {
        key: SettingKey,
        value: String,
    },
}

/// 允许的 DNS 服务器数量上限。
pub const MAX_DNS: usize = 3;

/// 解析 DNS 服务器列表：逗号或空白分隔，1~[`MAX_DNS`] 个 IPv4 地址。
///
/// **为什么放在共享 crate**：写入侧（`aetherd`）要先校验再落盘，界面侧要"边输边判"
/// 给即时反馈。两边各写一份就是**两份会分叉的校验** —— 安全审计 S-2 的教训。
/// 这里只有一份，且 `aether-ipc` 在 musl 检查范围内（两侧的编译都会被查）。
///
/// **纯函数**（收字符串、不碰文件），所以边界能在开发机上全部测到。
/// 拒绝的理由都写进返回值 —— 界面要把它原样显示给用户，不能只说"格式错误"。
pub fn parse_dns_list(input: &str) -> Result<Vec<std::net::Ipv4Addr>, String> {
    let parts: Vec<&str> = input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("DNS 不能为空：请填 1~3 个 IPv4 地址，例如 1.1.1.1, 8.8.8.8".into());
    }
    if parts.len() > MAX_DNS {
        return Err(format!(
            "最多 {MAX_DNS} 个 DNS 服务器，收到了 {} 个",
            parts.len()
        ));
    }
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        // 含冒号的多半是想填 IPv6：明确说明，别让用户对着"格式错误"猜
        if p.contains(':') {
            return Err(format!(
                "暂不支持 IPv6 DNS（{p}）：本版只写 IPv4 的 nameserver 行"
            ));
        }
        match p.parse::<std::net::Ipv4Addr>() {
            Ok(ip) => {
                // 回环地址不是有效的上游 DNS：接受它只会让解析静默失败
                if ip.is_loopback() {
                    return Err(format!("{p} 是回环地址，不能作为上游 DNS"));
                }
                out.push(ip);
            }
            Err(_) => {
                return Err(format!(
                    "「{p}」不是合法的 IPv4 地址（要四段 0~255，且不能有前导零）"
                ))
            }
        }
    }
    Ok(out)
}

/// 可写的系统设置项（**白名单**）。
///
/// 只列真正有消费者、能立即生效的项。静态 IP 暂不列入：它需要 aether-init 侧
/// 开机读取并配置，否则写了没人读 —— 那是假功能，不是"未完成的功能"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKey {
    /// DNS 服务器列表（写入 `/etc/resolv.conf`，立即生效）。
    Dns,
}

impl SettingKey {
    /// 线上表示（与 `serde` 的 rename 保持一致；两侧各写一份必然分叉，故有断言钉住）。
    pub fn as_str(self) -> &'static str {
        match self {
            SettingKey::Dns => "dns",
        }
    }

    /// 从字符串解析（白名单；**未知一律 `None`**）。
    pub fn parse(s: &str) -> Option<SettingKey> {
        match s {
            "dns" => Some(SettingKey::Dns),
            _ => None,
        }
    }
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
    /// 设置写入回执（`Request::SetSetting` 的应答）。
    ///
    /// `ok = false` 时 `message` 说的是**为什么拒绝**（校验不过 / 权限不足 / 写失败），
    /// 而不是空话 —— 设置页要把它原样显示给用户。
    SettingAck {
        key: String,
        ok: bool,
        message: String,
    },
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


// ---------------------------------------------------------------------------
// 对端身份（SO_PEERCRED）
// ---------------------------------------------------------------------------

/// 是否允许该 uid 连接本机 IPC（**纯函数，跨平台可测**）。
///
/// 只允许 root（uid 0）：当前没有用户模型，合成器与 CLI 都以 root 运行。
/// **读不到对端身份时一律拒绝**（fail-closed）—— "不知道对方是谁"不等于"可以放行"。
///
/// 判据**只有这一处**：aetherd 与 aether-init 共用它。两处各写一份迟早会分叉，
/// 而"一个入口严、另一个松"正是这类检查最容易出的问题。
pub fn peer_uid_allowed_with(uid: Option<u32>, trusted_uid: u32) -> bool {
    matches!(uid, Some(u) if u == 0 || u == trusted_uid)
}

/// 以**本进程自身的 uid** 为可信身份（Linux）。
///
/// 语义：root 永远放行；此外只放行"与本进程同一个用户"的连接 ——
/// 服务若以非 root 用户运行，接受同用户连接是自然的，而它也造不出 root-only 的 socket。
/// 这是 aetherd 与 aether-init 统一后的判据（原先两处写法不同，见审计 M-17 残余）。
#[cfg(target_os = "linux")]
pub fn peer_uid_allowed(uid: Option<u32>) -> bool {
    // SAFETY: getuid 不会失败、无副作用、无参数。
    let me = unsafe { libc::getuid() };
    peer_uid_allowed_with(uid, me)
}

/// 读取 Unix socket 对端的 uid（`SO_PEERCRED`）。读不到返回 `None`。
///
/// **不用 `UnixStream::peer_cred()`**：它在 rustc 1.98 上仍是 unstable
/// （`peer_credentials_unix_socket`）—— 这个坑只有 musl 静态检查能发现，
/// 因为 Windows 编译不到这段代码。改用 `libc::getsockopt`（与 aether-init 同一套）。
#[cfg(target_os = "linux")]
pub fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `cred` 是 POD，先清零再交给内核填写；长度按类型给出。
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(cred.uid)
    } else {
        None
    }
}

/// 校验对端身份，**不通过时返回拒绝原因**（便于调用方打印统一格式的日志）。
///
/// 返回 `Err(reason)` 即应断开该连接。**读不到身份不是"跳过检查"**，而是拒绝。
#[cfg(target_os = "linux")]
pub fn check_peer(stream: &std::os::unix::net::UnixStream) -> Result<(), String> {
    let uid = peer_uid(stream);
    if peer_uid_allowed(uid) {
        Ok(())
    } else {
        Err(format!("对端 uid {uid:?} 不是 root（SO_PEERCRED）"))
    }
}

/// 非 Linux 的 unix（如 macOS）：没有 `SO_PEERCRED`（那边是 `LOCAL_PEERCRED`，语义不同）。
///
/// **不假装检查过** —— 明确返回错误（fail-closed）。当前部署目标只有 Linux，
/// 若将来要支持别的 unix，应在这里补对应实现，而不是悄悄放行。
#[cfg(all(unix, not(target_os = "linux")))]
pub fn check_peer(_stream: &std::os::unix::net::UnixStream) -> Result<(), String> {
    Err("本平台没有 SO_PEERCRED，无法校验对端身份（按拒绝处理）".into())
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
    /// 对端身份判据：只放行 root，且**未知身份一律拒绝**。
    ///
    /// 这条测试的意义是把"fail-closed"钉死：改成 `uid != Some(1000)` 那种
    /// 黑名单式写法（常见错误方向）会立刻变红。
    #[test]
    fn peer_uid_must_be_root_or_same_user_and_unknown_is_rejected() {
        // root 永远放行（无论本进程是谁）
        assert!(peer_uid_allowed_with(Some(0), 0));
        assert!(peer_uid_allowed_with(Some(0), 1000), "root 应无条件放行");
        // 本进程自身的 uid 放行（服务以非 root 运行时接受同用户连接）
        assert!(peer_uid_allowed_with(Some(1000), 1000));
        // 其它 uid 一律拒绝
        assert!(!peer_uid_allowed_with(Some(1000), 0), "普通用户不得连 root 的 IPC");
        assert!(!peer_uid_allowed_with(Some(1), 1000));
        // **fail-closed**：读不到身份必须拒绝（最关键的一条）
        assert!(!peer_uid_allowed_with(None, 0));
        assert!(!peer_uid_allowed_with(None, 1000));
    }

    #[test]
    fn setting_key_whitelist_rejects_unknown_and_matches_wire_form() {
        // 白名单：只有已知键能解析
        assert_eq!(SettingKey::parse("dns"), Some(SettingKey::Dns));
        assert_eq!(SettingKey::parse(""), None);
        assert_eq!(SettingKey::parse("DNS"), None, "大小写不敏感会扩大攻击面，明确要求小写");
        // 危险形状一律拒绝（这些是"任意字符串当键"实现里最容易出事的形式）
        for evil in ["../etc/passwd", "dns\n", "dns ", " dns", "dns;reboot", "aetherd.sock"] {
            assert_eq!(SettingKey::parse(evil), None, "不该接受 {evil:?}");
        }
        // 线上表示必须与 as_str 一致（两侧各写一份必然分叉）
        let json = serde_json::to_string(&SettingKey::Dns).unwrap();
        assert_eq!(json, format!("\"{}\"", SettingKey::Dns.as_str()));
        // 未知键在**解码阶段**就被拒（不是等到用的时候才检查）
        assert!(serde_json::from_str::<SettingKey>("\"nope\"").is_err());
    }

    #[test]
    fn dns_validator_is_shared_and_strict() {
        assert_eq!(parse_dns_list("1.1.1.1").unwrap().len(), 1);
        assert_eq!(parse_dns_list("1.1.1.1, 8.8.8.8").unwrap().len(), 2);
        // 顺序即优先级，不能被改动
        let v = parse_dns_list("9.9.9.9, 1.1.1.1").unwrap();
        assert_eq!(v[0].to_string(), "9.9.9.9");
        // 拒绝路径（每条理由都要非空 —— 界面直接显示它）
        for (bad, why) in [
            ("", "空"),
            ("1.1.1.1, 8.8.8.8, 9.9.9.9, 4.4.4.4", "超 3 个"),
            ("::1", "IPv6"),
            ("1.1.1", "段数不足"),
            ("999.1.1.1", "越界"),
            ("01.2.3.4", "前导零"),
            ("1.1.1.1; reboot", "注入形状"),
            ("127.0.0.1", "回环"),
            ("localhost", "域名不是地址"),
        ] {
            let r = parse_dns_list(bad);
            assert!(r.is_err(), "{bad:?}（{why}）应被拒绝");
            assert!(!r.unwrap_err().is_empty(), "{bad:?} 的理由不能为空");
        }
    }

}
