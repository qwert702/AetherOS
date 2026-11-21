//! 桌面与帧的状态快照：窗口、桌面、UI 瞬态、设置上下文等纯数据结构。
use super::*;

/// 文件管理器的视图数据。
///
/// 从 `Desktop` 摘出来单独传，否则 `draw_window` 的参数会膨胀成
/// `(entries, selected, dir_error, cwd)` 一串，且每个都只为文件窗口服务。
pub struct FileView<'a> {
    pub cwd: &'a str,
    pub entries: &'a [FsEntry],
    pub selected: Option<usize>,
    /// 读目录失败的原因；有值时状态栏优先显示它
    pub error: Option<&'a str>,
    /// 首个可见条目索引（滚动位置）。0 = 从头开始。
    pub scroll: usize,
    /// 侧栏"位置"列表（(显示名, 路径)）
    pub sidebar: &'a [(String, String)],
}

/// 桌面状态：窗口列表（含 z 序）与当前布局。
pub struct Desktop {
    pub wins: Vec<Win>,
    pub active: usize,
    pub layout: crate::layout::Layout,
    /// 文件管理器当前目录
    pub cwd: String,
    /// 当前目录的条目（`refresh_dir` 之后有效）
    pub entries: Vec<FsEntry>,
    /// 文件管理器中选中的条目索引
    pub selected: Option<usize>,
    /// 文件网格的滚动位置（首个可见条目索引）
    pub scroll: usize,
    /// 侧栏"位置"：（显示名, 真实路径）。只含真实存在的目录。
    pub sidebar: Vec<(String, String)>,
    /// 剪贴板 v1：进程内一段全局文本（终端/文件路径/AI 指令条共用）。
    ///
    /// 跨进程剪贴板要另加 IPC 消息（计划 §2.2）——当前所有"应用"都在合成器进程内，
    /// 一段文本就够用，先把交互跑通。
    pub clipboard: String,
    /// 读取 cwd 时的错误；有值时状态栏显示它而不是条目数
    pub dir_error: Option<String>,
}

/// 窗口内容类型。
///
/// 此前 `draw_window` 靠比较 `title` 字符串来区分"这是终端还是音乐"。
/// 一旦标题要显示动态内容（文件管理器显示当前路径），这个做法立刻失效 ——
/// 所以显式化成一个枚举。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WinKind {
    /// 文件管理器
    Files,
    /// 音乐播放器
    Music,
    /// 终端
    Terminal,
    /// 文本文件预览
    Preview,
    /// Wayland 客户端窗口（3.1 spike）：内容由客户端经 wl_shm 提供
    #[allow(dead_code)] // 生产路径未接；由 sync_wayland 的测试构造。
    Wayland,
    /// **设置中心**（2026-09-29 P3）：左侧分组导航 + 右侧页面内容。
    /// 在此之前点 Dock 的"设置"打开的是文件管理器（标题写着"设置"的假窗口）。
    Settings,
}

pub struct Win {
    pub rect: Rect,
    /// 动画目标；每帧向它缓动，到达后清除
    pub target: Option<Rect>,
    /// 标题栏文字。文件管理器会显示当前路径，故为 `String` 而非 `&'static str`。
    pub title: String,
    /// 内容类型（决定画什么、以及点击怎么处理）
    pub kind: WinKind,
    pub floating: bool,
    /// 最大化前的矩形；`Some` = 当前处于最大化态（再点一次红绿灯恢复）
    pub restore: Option<Rect>,
    /// 预览窗口的内容（仅 `WinKind::Preview` 有值）
    pub preview: Option<PreviewData>,
    /// 终端会话（仅 `WinKind::Terminal` 有值）。
    /// 内容由 `term::Terminal` 持有：Linux 上是真实 PTY，开发机预览是喂进同一解析器的演示脚本。
    pub term: Option<crate::term::Terminal>,
    /// Wayland 客户端窗口的内容（仅 `WinKind::Wayland` 有值）
    pub wayland: Option<WaylandSurface>,
}

/// Wayland 客户端窗口的内容快照（3.1 spike）。
///
/// 每次 commit 时从 `Session` **拷贝**一份 —— 渲染循环是同步的，
/// 不与客户端共享内存指针，避免"客户端改一半、合成器读一半"的撕裂。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaylandSurface {
    /// 来源 surface 的对象 id（同步时的身份）
    pub surface_id: u32,
    pub app_id: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8888（shm.rs::read_pixels 的输出）
    pub pixels: Vec<u8>,
}

/// AI 回复气泡的类别（用户/AI/工具调用三类样式，语义一眼可分）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BubbleKind {
    /// 用户发出的指令（中性底、偏右）
    User,
    /// AI 的自然语言回复（青紫焦点环）
    Ai,
    /// 工具调用结果（带工具名与成败标记）
    Tool,
}

/// AI 通路状态（§4 三态：本地青 / 云端紫 / 离线灰）。
///
/// 三态均已接线：`Cloud` 由 aetherd 在 ChatChunk.channel=="cloud" 时驱动
/// （协议扩展见 ui-design-handover §8.1）；`Offline` 由连接失败驱动。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AiStatus {
    Local,
    Cloud,
    Offline,
}

impl AiStatus {
    pub fn label(self) -> &'static str {
        match self {
            AiStatus::Local => "本地模型在线",
            AiStatus::Cloud => "云端模型",
            AiStatus::Offline => "AI 离线",
        }
    }
    /// English label（§3.5 i18n）。
    pub fn label_en(self) -> &'static str {
        match self {
            AiStatus::Local => "Local",
            AiStatus::Cloud => "Cloud",
            AiStatus::Offline => "Offline",
        }
    }

    /// 状态色：青 / 紫 / 灰
    pub fn color(self) -> [u8; 3] {
        match self {
            AiStatus::Local => color::accent(),
            AiStatus::Cloud => color::accent(),
            AiStatus::Offline => color::text_faint(),
        }
    }
}

/// L2+ 权限确认弹窗的一帧快照。
pub struct ConfirmUi<'a> {
    pub tool: &'a str,
    /// 2 = L2 敏感写，3 = L3 危险
    pub level: u8,
    /// 参数明文（逐项原样展示，不做美化）
    pub arguments: &'a [(String, String)],
    /// 一句话后果说明
    pub consequence: &'a str,
    /// L3 需回显确认的目标文本；None = 只需点确认
    pub echo_required: Option<&'a str>,
    /// 用户已输入的回显文本
    pub echo_input: &'a str,
}

impl ConfirmUi<'_> {
    /// 回显是否已匹配（L2 恒为 true，L3 需原样输入目标）
    pub fn echo_ok(&self) -> bool {
        match self.echo_required {
            Some(target) => self.echo_input.trim() == target,
            None => true,
        }
    }

    /// 风险等级徽章配色：L2 黄 / L3 红
    pub fn badge_color(&self) -> [u8; 3] {
        if self.level >= 3 { color::danger() } else { color::warning() }
    }

    /// 徽章**文字**色。
    ///
    /// 淡色底上必须用更高对比的变体：直接用 `badge_color()` 当文字色时，
    /// 深色模式 L3 只有 2.6:1（亮红字压在亮红淡底上）——徽章存在的意义就是
    /// "一眼看出等级"，读不清就白做了。
    pub fn badge_text_color(&self) -> [u8; 3] {
        if self.level >= 3 { color::danger_text() } else { color::warning_text() }
    }
}

/// 每帧的 UI 瞬态（由 main.rs 组装）。
pub struct UiState<'a> {
    pub snap: Option<Rect>,
    pub toast: Option<(&'a str, f32)>,
    pub ai_input: &'a str,
    /// 输入光标位置（第几个**字符**之前）。2.4：支持在中间编辑，不再只能追加。
    pub ai_cursor: usize,
    /// 中文输入法状态（2.3）：候选框据此渲染
    pub ime: &'a crate::ime::Ime,
    /// 指令条是否处于焦点（画青紫渐变环）
    pub ai_focused: bool,
    /// 是否在等待 AI 回复（呼吸动效）
    pub ai_thinking: bool,
    pub ai_status: AiStatus,
    /// 回复气泡：(文本, 类别, 已显示时长)
    pub ai_reply: Option<(&'a str, BubbleKind, f32)>,
    pub mouse: (f32, f32),
    /// 左键是否按下（组件的按下态用）
    pub mouse_down: bool,
    /// 音频状态文案（批次 K-2）：**如实**显示"无音频设备/未检测"，不画假滑块
    pub audio_label: &'a str,
    /// 当前音量（0..=100）。仅在 `audio_enabled` 为真时有意义
    pub audio_volume: u8,
    /// 音频控件是否可用（= `AudioState::Ready`）。false 时**不登记命中区**（点不动）
    pub audio_enabled: bool,
    pub open_menu: Option<usize>,
    /// 是否显示"安装"Dock 图标（仅 Live ISO 会话）
    pub show_installer: bool,
    /// 安装向导窗口（None = 关闭）
    pub installer: Option<InstallerUi<'a>>,
    /// L2+ 权限确认弹窗（None = 无待确认操作）
    pub confirm: Option<ConfirmUi<'a>>,
}

/// 安装向导的阶段（渲染用；状态机在 main.rs）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerPhase {
    Idle,
    Running,
    Done,
    Failed,
}

/// 安装向导窗口的一帧快照。
pub struct InstallerUi<'a> {
    /// (设备, 容量MB) 候选列表
    pub disks: &'a [(String, u64)],
    pub selected: usize,
    pub phase: InstallerPhase,
    /// Done/Failed 时附带的结果说明
    pub message: Option<&'a str>,
}

/// 权限确认弹窗的按钮语义。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfirmButton {
    /// 允许一次（无"永久允许"：安全决策，不做）
    Allow,
    Deny,
}

/// Dock 里**内建**图标的数量（文件/终端/浏览器/音乐/设置）。
///
/// 已装应用排在这之后、安装向导之前。命中测试要靠它算区间，所以必须是常量而不是
/// 各处写字面量 —— `main.rs` 里有一条断言把 `APP_TITLES.len()` 钉在这个值上。
pub const DOCK_BUILTINS: usize = 5;

/// 控制中心里可交互项的标识（绘制期登记、事件循环消费）。
///
/// 与 `settings::SettingsHit` 同一思路：绘制不修改状态，判定只有一份。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlHit {
    /// 切换中文输入法（真实动作：`ime.toggle()`，由主循环执行）
    ToggleIme,
    /// 切换 24 小时制（写 `settings` 并落盘）
    ToggleClock24h,
    /// 打开设置中心
    OpenSettings,
    /// 打开终端
    OpenTerminal,
    /// 深色模式开关（默认关 = 白色；最常用的一项，所以在面板里排第一）
    ToggleDark,
    /// **动作类**：关机（L3，需确认卡片 + 回显 `poweroff`）
    PowerOff,
    /// **动作类**：重启（L3，需确认卡片 + 回显 `reboot`）
    Reboot,
    /// **动作类**：注销（= 重启合成器，由 aether-init 拉起；不需要确认）
    Logout,
    /// **动作类**：音量轨道（批次 K-2）。点击位置决定音量 ——
    /// 命中区是**整条轨道**，具体百分比由点击 x 与轨道宽度算出（见 `main.rs`）。
    VolumeTrack,
    /// **动作类**：打开通知中心（§3.2）
    Notifications,
    /// **动作类**：新增一个虚拟桌面（§5.4）
    AddSpace,
    /// **动作类**：移除当前虚拟桌面（§5.4）。**里面有窗口时会拒绝**并说明原因。
    RemoveSpace,
}

/// 绘制设置页所需的"外部世界"。
///
/// **为什么要有它**：这些参数一路从 `Renderer` 传到页面分支，散着传已经到 13 个 ——
/// 每加一个页面就要改签名 + 全部调用点（含 6 处测试）。收成一个结构后，
/// 加一个页面只改结构的字段与页面分支，不动任何签名。
///
/// 全是 `Copy`（引用 + `Settings`），所以按值传没有代价。
#[derive(Clone, Copy)]
pub struct PageCtx<'a> {
    pub mouse: (f32, f32),
    pub tr: Option<&'a TextRenderer>,
    pub settings: crate::settings::Settings,
    pub dns: &'a crate::settings::DnsField,
    /// §2.3 改口令表单（绘制只读；编辑态由事件循环维护）
    pub pass: &'a crate::settings::PassForm,
    /// §2.3 账户列表（只读）
    pub users: &'a [aether_ipc::UserInfo],
    pub users_loaded: bool,
    /// §3.7 外设（只读）
    pub peripherals: &'a [aether_ipc::PeripheralWire],
    pub peripherals_loaded: bool,
    /// §5.2 时间线的搜索词（由事件循环维护，绘制只读）
    pub audit_query: &'a str,
    pub policy: &'a crate::policyview::PolicyView,
    pub store: &'a crate::store::StoreView,
    pub time: &'a crate::timeview::TimeView,
    pub snapshots: &'a crate::snapview::SnapView,
    pub update: &'a crate::updateview::UpdateView,
    pub displays: &'a [aether_ipc::DisplayWire],
    pub driven_index: u32,
    /// 是否已经读过一次（区分「正在读取」与「确实没有输出」）
    pub displays_loaded: bool,
    /// 可移动设备（§3.7，只读列表 + 一行状态）
    pub removables: &'a [aether_ipc::RemovableWire],
    pub removables_loaded: bool,
    /// 正在对哪个设备做操作（期间挡点击）
    pub removable_pending: Option<&'a str>,
    /// 上一次操作的结果（保留到下一次操作）
    pub removable_message: Option<&'a str>,
}

/// 测试用：与 `test_ctx` 相同，但可以注入一个**非空**的商店视图
/// （验证"已安装/可安装"两种行各自登记了正确的命中）。
#[cfg(test)]
pub fn test_ctx_with_store<'a>(
    tr: Option<&'a TextRenderer>,
    store: &'a crate::store::StoreView,
) -> PageCtx<'a> {
    let mut ctx = test_ctx(tr);
    ctx.store = store;
    ctx
}

/// 测试用的上下文：四个视图对象借用**静态默认值**（测试不关心它们，但必须活得够久）。
#[cfg(test)]
pub fn test_ctx(tr: Option<&TextRenderer>) -> PageCtx<'_> {
    use std::sync::OnceLock;
    static DNS: OnceLock<crate::settings::DnsField> = OnceLock::new();
    static PASS: OnceLock<crate::settings::PassForm> = OnceLock::new();
    static POLICY: OnceLock<crate::policyview::PolicyView> = OnceLock::new();
    static STORE: OnceLock<crate::store::StoreView> = OnceLock::new();
    static TIME: OnceLock<crate::timeview::TimeView> = OnceLock::new();
    static SNAPS: OnceLock<crate::snapview::SnapView> = OnceLock::new();
    static UPD: OnceLock<crate::updateview::UpdateView> = OnceLock::new();
    PageCtx {
        mouse: (0.0, 0.0),
        tr,
        settings: crate::settings::Settings::default(),
        audit_query: "",
            dns: DNS.get_or_init(Default::default),
    pass: PASS.get_or_init(Default::default),
    users: &[],
    users_loaded: false,
    peripherals: &[],
    peripherals_loaded: false,
        policy: POLICY.get_or_init(Default::default),
        store: STORE.get_or_init(Default::default),
        time: TIME.get_or_init(Default::default),
        snapshots: SNAPS.get_or_init(Default::default),
        update: UPD.get_or_init(Default::default),
        displays: &[],
        driven_index: 0,
        displays_loaded: false,
        removables: &[],
        removables_loaded: false,
        removable_pending: None,
        removable_message: None,
    }
}

