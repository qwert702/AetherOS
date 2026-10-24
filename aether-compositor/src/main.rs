//! aether-compositor — AetherOS 自研 Wayland 合成器。
//!
//! 当前阶段：主机渲染预览模式（M1–M4 联调形态）。
//! - 极光壁纸 + 磨砂窗口（拖拽/置顶/边缘吸附）+ 四种布局
//! - 底部 AI 指令条可真实输入：Enter 发送到 aetherd serve（127.0.0.1:7311），
//!   收到 ChatChunk 显示回复、收到 Action 执行桌面行为（切换布局等）
//! - `--shot [1-4]` 渲染指定布局的单帧供自检
//!
//! 目标形态（M1 后期，Linux VM）：基于 smithay 的真正 Wayland 合成器。

mod draw;
// 通知历史（§3.2）：纯逻辑模块，接线在后续提交里做
mod notifications;
// 音频（提示音 + 音量）：纯逻辑可单测，实际播放走 alsa-utils 子进程（见模块说明）
mod audio;
#[cfg(target_os = "linux")]
mod fbdev;
#[cfg(target_os = "linux")]
mod fbdev_app;
mod app_main;
mod app_support;
use app_support::*;

fn main() -> anyhow::Result<()> {
    app_main::main()
}
// 输入事件层：事件类型与键位翻译跨平台（可在开发机上单测），仅采集后端限 Linux
mod input;
// 中文输入法（2.3）：拼音 → 候选 → 上屏，词表自建
mod ime;
#[cfg(target_os = "linux")]
mod pty;
mod layout;
mod text;
// 终端：胶合层跨平台（Linux 走真 PTY，开发机喂真实 ANSI 脚本走同一解析路径）
mod term;
// 文本视图共享状态：光标 / 滚动 / 导航键映射（2.4 统一文本交互）
mod textview;
// VT/ANSI 解析器：纯逻辑跨平台（终端正确性靠它的单测保证）
mod vt;

/// 控件原语（P1）：按钮/开关/滑杆/分段/列表行/滚动条/徽标 + 共享命中表。
///
/// 2026-09-29 新增，P3（设置中心）接入了一部分。**当前事实、以及"为什么还保留
/// dead_code"的理由写在 `widgets.rs` 的模块头注释里**（那里是唯一真相，
/// 免得同一件事在两处各说一套）：已被大量使用，但滑杆/徽标/滚动条还没有生产调用点，
/// 且 `WHEEL_LINES`/`scroll_apply` 会在 Windows 上误报"未使用"。
mod widgets;

/// 用户设置的持久化与声明（P3 设置中心）：`/var/lib/aether/settings.json`。
mod settings;
mod shortcuts;
mod palette;
mod policyview;
mod spaces;
mod setup;
mod store;
mod timeview;
mod snapview;
mod updateview;
mod auth;
mod i18n;
mod lock;
mod sha512crypt;
/// 网络状态（P4.2 设置中心「网络」页）：读内核真值（/sys/class/net、/proc/net/*），不新增 IPC。
mod net;
/// 审计日志（P4.3 设置中心「权限与隐私」页）：读 aetherd 的审计日志，只读展示，不新增 IPC。
mod audit;
// Wayland 协议实现（3.1 spike）：wire/object 是纯逻辑，跨平台可测
mod wayland;

use aether_ipc::{Request, Response};
use draw::{Desktop, Rect, Win};
use draw::theme::metric;
// 通知正文的结构化双语类型（§3.5 遗留项 #43）：见 `toast_msg` 助手的说明
use i18n::ToastMsg;
#[cfg(target_os = "linux")]
use draw::{InstallerPhase, InstallerUi};
#[cfg(not(target_os = "linux"))]
use draw::UiState;
use layout::Layout;
#[allow(unused_imports)] // 供 main.rs 内的测试模块使用；bin 构建不一定直接引用。
use app_main::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// aetherd 的 Unix socket 路径（定义在协议 crate 里，与 aetherd 监听侧共用）。
///
/// 为什么 IPC 要优先走它（2026-10-02 审计 M-17）：`register_ui` 的前提是"对端确实是
/// aetherd"，而 TCP 的连接方**无法验证对端**（谁能绑上 7311，谁就能收到 UI 密钥）。
/// Unix socket 由文件权限兜底：`/run` 属 root，socket 0600 —— 非 root 进程既建不了、
/// 也连不上。
#[cfg(unix)]
const AETHERD_SOCKET: &str = aether_ipc::UNIX_SOCKET_PATH;

/// 显式改用 TCP 连接 aetherd（默认关闭）。
///
/// 只有一个正当用途：**开发机上用 hostfwd 直连 guest 的 7311** 调试。
/// 打开它意味着"UI 密钥会经 TCP 明文发送"，所以必须显式设环境变量才会生效。
/// 非 unix 平台没有 Unix socket，连接本来就只能走 TCP，这个开关没有意义
/// （因此那边标 dead_code，避免"未使用常量"警告）。
#[cfg_attr(not(unix), allow(dead_code))]
const AETHER_IPC_TCP_ENV: &str = "AETHER_IPC_TCP";

/// 与 aetherd 的连接：优先 Unix socket，其次（仅显式允许时）TCP。
///
/// 实现 `Read + Write` 并转发 `try_clone`/`set_read_timeout`，因此上层的
/// `BufReader<IpcStream>` 与既有调用形式不变。
enum IpcStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(TcpStream),
}

impl IpcStream {
    /// 连接 aetherd。`connect_timeout` 只对 TCP 生效（Unix socket 是本机连接，立即返回）。
    fn connect(connect_timeout: Duration) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            // 显式要求走 TCP 时才跳过 socket（调试用）
            if std::env::var(AETHER_IPC_TCP_ENV).map(|v| v == "1").unwrap_or(false) {
                return Self::connect_tcp(connect_timeout);
            }
            match std::os::unix::net::UnixStream::connect(AETHERD_SOCKET) {
                Ok(s) => return Ok(IpcStream::Unix(s)),
                // **不回退到 TCP**：socket 不在 = aetherd 没在跑。此时回退 TCP 只会
                // 把密钥交给"抢到他端口"的进程，正是 M-17 要堵的路。
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        if crate::i18n::is_en() {
                            format!("Cannot reach aetherd ({AETHERD_SOCKET}: {e})")
                        } else {
                            format!("连接 aetherd 失败（{AETHERD_SOCKET}: {e}）")
                        },
                    ))
                }
            }
        }
        #[cfg(not(unix))]
        {
            Self::connect_tcp(connect_timeout)
        }
    }

    fn connect_tcp(connect_timeout: Duration) -> std::io::Result<Self> {
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::DEFAULT_PORT).into();
        Ok(IpcStream::Tcp(TcpStream::connect_timeout(&addr, connect_timeout)?))
    }

    fn try_clone(&self) -> std::io::Result<Self> {
        Ok(match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => IpcStream::Unix(s.try_clone()?),
            IpcStream::Tcp(s) => IpcStream::Tcp(s.try_clone()?),
        })
    }

    fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.set_read_timeout(d),
            IpcStream::Tcp(s) => s.set_read_timeout(d),
        }
    }
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.read(buf),
            IpcStream::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.write(buf),
            IpcStream::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.flush(),
            IpcStream::Tcp(s) => s.flush(),
        }
    }
}




#[cfg(test)]
mod tests;
