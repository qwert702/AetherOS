//! 用户设置的持久化与声明（P3 设置中心）。
//!
//! ## 为什么写在 `/var`
//!
//! 根文件系统是 initramfs（内存），**只有 `/var` 挂到真磁盘**
//! （`aether-init/src/persist.rs`）。所以设置放 `/var/lib/aether/settings.json`；
//! 写在 `/etc` 的文件重启就没了 —— 这是本仓库常被误述的一点。
//!
//! ## 坏设置不能拖垮桌面
//!
//! 文件缺失、JSON 损坏、字段类型不对、越界值：一律回落默认值并继续启动。
//! 这里刻意**不用 serde derive**（构件的 `serde` 特征不在合成器依赖里），
//! 直接走 `serde_json::Value` 手读字段 —— 字段少，且能对每个字段单独容错。

// P3 的设置 UI（点击处理）目前只接在 **Linux 的 fbdev 循环**里，Windows 预览路径只用它的"绘制"半边，
// 所以「读设置 / 写设置 / 夹取」这些函数在非 Linux 目标上没有调用点。这里按目标收敛，避免 dead_code 噪音。
// **等预览路径也接上设置 UI 后应当删掉这行**（那时两边都有调用点）。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use serde_json::{json, Value};

/// 设置文件路径（`/var` 是唯一持久分区）。
pub const SETTINGS_PATH: &str = "/var/lib/aether/settings.json";

/// 时区偏移的允许范围（分钟）：±14 小时，与真实世界时区一致。
const TZ_MIN: i32 = -14 * 60;
const TZ_MAX: i32 = 14 * 60;

/// 用户可改的设置。
///
/// 刻意保持**小而可持久化**：每加一个字段都要能被 `from_json` 容错、被设置界面渲染、
/// 被门禁测到。宁可少，不要放进去实现不了的东西（"能点但没用"是本项目明令禁止的）。
///
/// **为什么新字段都是下标/小整数而不是 `String`**（2026-10-02 功能批次 I）：
/// `Settings` 是 `Copy`（全仓大量按值传递），塞一个 `String` 会引发连锁改动；
/// 而"壁纸""默认应用""语言"本来就该是**可枚举**的（内置集合 + 下标），
/// 用下标既保持 `Copy`，又天然挡住了"设置文件里写任意字符串"这类无效状态。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// 时区偏移（分钟）。默认 +8（产品默认面向中文用户）。
    pub tz_offset_min: i32,
    /// 24 小时制；false = 12 小时制（下午 9:10）。
    pub clock_24h: bool,
    /// 顶栏时钟是否显示秒。
    pub clock_seconds: bool,
    /// 中文输入法是否默认开启。
    pub ime_default: bool,
    /// 深色模式（**默认 false = 明亮/纯白**，与产品主视觉一致）。用户可在设置中心或控制中心切换。
    pub dark_mode: bool,
    /// 高对比度（无障碍 §3.4）。与深色模式**正交**：两种主题都能开。
    pub high_contrast: bool,
    /// 鼠标速度（百分比，100 = 1:1）。
    ///
    /// 合成器原先对相对位移**硬编码 ×2**，在 VMware/真机上太快（用户实测反馈）；
    /// 默认改为 100（1:1），用户可在设置中心「输入」页调。
    pub mouse_speed_pct: i32,
    /// 壁纸：内置壁纸表的下标（0 = 纯色）。越界由解析处夹取。
    pub wallpaper: u8,
    /// 界面缩放百分比，只能是 [`UI_SCALE_STEPS`] 里的值（解析时吸附到最近档）。
    pub ui_scale_pct: u16,
    /// 默认打开方式：内置应用下标（与 `main.rs::APP_TITLES` / `open_app` 一致）。
    pub default_app: u8,
    /// 提示音开关（批次 K 接音频；无音频设备时该项**无效果但可保存**，
    /// 界面必须如实显示"无音频设备"而不是假装静音成功）。
    pub notify_sound: bool,
    /// 界面语言：0 = 中文，1 = English（下标，见 [`LOCALE_COUNT`]）。
    pub locale: u8,
    /// 首次启动向导是否已走完（或跳过）（§5.8）。
    ///
    /// **跳过也算完成**：否则用户点了跳过，下次开机又弹一遍 —— 那是骚扰不是帮助。
    pub setup_done: bool,
    /// 空闲多少分钟后息屏（**0 = 永不**）。取值只允许 `IDLE_BLANK_OPTIONS` 里的档位 ——
    /// 任意分钟数没有意义（用户只会在这几档里选），也避免设置文件被改成 99999。
    pub idle_blank_min: u16,
    /// 性能 HUD（§5.10）：显示**真实**帧时间与内存占用。**默认关** ——
    /// 它是排障工具，不是装饰；常开会让界面多一块与日常使用无关的信息。
    pub perf_hud: bool,
    /// 是否记录通知历史（§3.2）。默认**开** —— 用户没关就不该悄悄不记。
    pub record_notifications: bool,
}

/// 界面缩放的允许档位。
///
/// 只有这三档：软件渲染下任意比例会产生半像素度量（字发虚、边框粗细不一），
/// 而"能选但显示难看"比"只有三档"更伤。
pub const UI_SCALE_STEPS: [u16; 3] = [100, 125, 150];

/// 内置壁纸数量（下标 0..WALLPAPER_COUNT，0 = 纯色）。
pub const WALLPAPER_COUNT: u8 = 6;

/// 支持的界面语言数量（0 中文 / 1 English）。
pub const LOCALE_COUNT: u8 = 2;

/// 默认打开方式的可选数量 —— **直接引用内置应用表**，避免两处各写一个数字。
pub const DEFAULT_APP_COUNT: u8 = crate::APP_TITLES.len() as u8;

/// 息屏档位（分钟）。**0 = 永不**。
///
/// 为什么是固定档位而不是任意值：用户只在这几档里选，而固定档位让
/// "设置文件被手工改成 99999" 这类情况有一个明确的吸附目标（见 `snap_idle_blank`）。
pub const IDLE_BLANK_OPTIONS: [u16; 4] = [0, 5, 10, 30];

/// 把任意分钟数吸附到最近的合法档位。
pub fn snap_idle_blank(min: u16) -> u16 {
    IDLE_BLANK_OPTIONS
        .iter()
        .copied()
        .min_by_key(|o| (*o as i32 - min as i32).abs())
        .unwrap_or(0)
}

/// 把任意值吸附到最近的一档缩放（未知值不会留在设置里）。
pub fn snap_scale(v: u16) -> u16 {
    *UI_SCALE_STEPS
        .iter()
        .min_by_key(|s| s.abs_diff(v))
        .unwrap_or(&100)
}

/// 鼠标速度允许范围。防止设置文件被改成 0（指针彻底不动）或天文数字。
pub const MOUSE_SPEED_MIN: i32 = 25;
pub const MOUSE_SPEED_MAX: i32 = 300;

impl Default for Settings {
    fn default() -> Self {
        Self {
            tz_offset_min: 8 * 60,
            clock_24h: true,
            clock_seconds: false,
            ime_default: false,
            dark_mode: false,
            high_contrast: false,
            mouse_speed_pct: 100,
            wallpaper: 0,
            ui_scale_pct: 100,
            default_app: 0,
            notify_sound: true,
            locale: 0,
            idle_blank_min: 10,
            perf_hud: false,
            record_notifications: true,
            // §5.8：全新安装时向导要弹（旧文件缺这个键会被当成「已完成」，见 from_json_with）
            setup_done: false,
        }
    }
}

impl Settings {
    /// 从 JSON 文本解析（用默认时区兜底）。
    ///
    /// **仅测试使用**：生产路径一律走 `load`，它会带上 `TZ` 解析出的兜底时区。
    #[cfg(test)]
    pub fn from_json(text: &str) -> Self {
        Self::from_json_with(text, Settings::default().tz_offset_min)
    }

    /// 同 `from_json`，但时区的兜底值由调用方给（发行版可用 `TZ` 定默认时区）。
    pub fn from_json_with(text: &str, fallback_tz_min: i32) -> Self {
        let mut s = Settings {
            tz_offset_min: fallback_tz_min.clamp(TZ_MIN, TZ_MAX),
            ..Settings::default()
        };
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return s;
        };
        if let Some(n) = v.get("tz_offset_min").and_then(Value::as_i64) {
            s.tz_offset_min = (n as i32).clamp(TZ_MIN, TZ_MAX);
        }
        if let Some(b) = v.get("clock_24h").and_then(Value::as_bool) {
            s.clock_24h = b;
        }
        if let Some(b) = v.get("clock_seconds").and_then(Value::as_bool) {
            s.clock_seconds = b;
        }
        if let Some(b) = v.get("ime_default").and_then(Value::as_bool) {
            s.ime_default = b;
        }
        if let Some(b) = v.get("high_contrast").and_then(Value::as_bool) {
            s.high_contrast = b;
        }
        if let Some(b) = v.get("dark_mode").and_then(Value::as_bool) {
            s.dark_mode = b;
        }
        if let Some(n) = v.get("mouse_speed_pct").and_then(Value::as_i64) {
            s.mouse_speed_pct = (n as i32).clamp(MOUSE_SPEED_MIN, MOUSE_SPEED_MAX);
        }
        // 批次 I 新增字段：**旧文件没有这些键 → 保持默认**（向后兼容），
        // 越界值一律夹取/吸附 —— 设置文件是可以被手工改的，不能信任它的内容。
        if let Some(n) = v.get("wallpaper").and_then(Value::as_u64) {
            s.wallpaper = (n as u8).min(WALLPAPER_COUNT.saturating_sub(1));
        }
        if let Some(n) = v.get("ui_scale_pct").and_then(Value::as_u64) {
            s.ui_scale_pct = snap_scale(n as u16);
        }
        if let Some(n) = v.get("default_app").and_then(Value::as_u64) {
            s.default_app = (n as u8).min(DEFAULT_APP_COUNT.saturating_sub(1));
        }
        if let Some(b) = v.get("notify_sound").and_then(Value::as_bool) {
            s.notify_sound = b;
        }
        if let Some(n) = v.get("locale").and_then(Value::as_u64) {
            s.locale = (n as u8).min(LOCALE_COUNT.saturating_sub(1));
        }
        if let Some(b) = v.get("perf_hud").and_then(Value::as_bool) {
            s.perf_hud = b;
        }
        if let Some(b) = v.get("record_notifications").and_then(Value::as_bool) {
            s.record_notifications = b;
        }
        // §5.8 首次启动向导：**缺键要视为「已完成」**，不能当成 false。
        //
        // 为什么：老设置文件没有这个键，而"有设置文件"本身就说明用户早就用过系统了。
        // 缺键当 false 的话，那些用户升级后开机会突然被弹一个向导 —— 那是骚扰。
        // 只有**文件根本不存在**（全新安装，走 `Settings::default()`）才该弹。
        // 「有设置文件」的准确含义 = **文档里确实有字段**（不是 `null`、不是 `{}`）。
        // 退化的空文档说明这份文件没有内容，那就按全新安装处理（向导该弹）。
        let looks_like_a_settings_file = v.as_object().map(|o| !o.is_empty()).unwrap_or(false);
        if let Some(b) = v.get("setup_done").and_then(Value::as_bool) {
            s.setup_done = b;
        } else {
            s.setup_done = looks_like_a_settings_file;
        }
        if let Some(n) = v.get("idle_blank_min").and_then(Value::as_u64) {
            // 不在档位里的值**吸附到最近的合法档**（而不是原样接受）——
            // 否则手工改过的设置文件会让界面显示一个选不中的值
            s.idle_blank_min = snap_idle_blank(n as u16);
        }
        s
    }

    /// 空闲判定：是否该息屏。
    ///
    /// 纯函数（拿 `Duration` 而不是 `Instant`）：**可单测**，不必等十分钟。
    /// 档位 0 表示永不 —— 那是演示与排障时的选择，不该被"默认值"覆盖掉。
    pub fn should_blank(&self, idle: std::time::Duration) -> bool {
        match self.idle_blank_secs() {
            None => false,
            Some(limit) => idle.as_secs() >= limit,
        }
    }

    /// 息屏阈值（秒）。`None` = 永不。
    pub fn idle_blank_secs(&self) -> Option<u64> {
        match self.idle_blank_min {
            0 => None,
            m => Some(m as u64 * 60),
        }
    }

    /// 序列化（带 schema 版本号，便于以后迁移时判断）。
    ///
    /// 版本号保持 1：批次 I 只是**追加**字段，旧二进制读新文件会忽略未知键、
    /// 新二进制读旧文件走默认值 —— 两个方向都兼容，没有需要迁移的语义变化。
    pub fn to_json(&self) -> String {
        let v = json!({
            "version": 1,
            "tz_offset_min": self.tz_offset_min,
            "clock_24h": self.clock_24h,
            "clock_seconds": self.clock_seconds,
            "ime_default": self.ime_default,
            "dark_mode": self.dark_mode,
            "high_contrast": self.high_contrast,
            "mouse_speed_pct": self.mouse_speed_pct,
            "wallpaper": self.wallpaper,
            "ui_scale_pct": self.ui_scale_pct,
            "default_app": self.default_app,
            "notify_sound": self.notify_sound,
            "locale": self.locale,
            "idle_blank_min": self.idle_blank_min,
            "perf_hud": self.perf_hud,
            "record_notifications": self.record_notifications,
            "setup_done": self.setup_done,
        });
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
    }

    /// 读设置文件；不存在/不可读/损坏都返回默认值。
    ///
    /// `fallback_tz_min` 是**没有设置文件时**的默认时区，由调用方从 `TZ` 解析
    /// （`text::tz_offset_min()`）—— 这样镜像可以按发行配置定默认时区，而不是写死 +8。
    pub fn load(path: &str, fallback_tz_min: i32) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let s = Settings::from_json_with(&text, fallback_tz_min);
                eprintln!("aether-compositor: 设置已加载 {path}");
                s
            }
            Err(e) => {
                eprintln!("aether-compositor: 设置文件不可读（{path}: {e}），使用默认值");
                Settings {
                    tz_offset_min: fallback_tz_min.clamp(TZ_MIN, TZ_MAX),
                    ..Settings::default()
                }
            }
        }
    }

    /// 写设置文件：建父目录 → 写临时文件 → `rename` 覆盖（**原子**）。
    ///
    /// 直接原地写会有一个"文件已截断但还没写完"的窗口 —— 掉电就得到半个 JSON。
    pub fn save(&self, path: &str) -> std::io::Result<()> {
        if let Some(dir) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path)?;
        eprintln!("aether-compositor: 设置已保存 {path}");
        Ok(())
    }

    /// 时区的人类可读写法（`UTC+08:00`）。
    pub fn tz_label(&self) -> String {
        let sign = if self.tz_offset_min < 0 { '-' } else { '+' };
        let a = self.tz_offset_min.abs();
        format!("UTC{sign}{:02}:{:02}", a / 60, a % 60)
    }

    /// 按分钟微调时区（夹取到 ±14:00）。
    pub fn shift_tz(&mut self, delta_min: i32) {
        self.tz_offset_min = (self.tz_offset_min + delta_min).clamp(TZ_MIN, TZ_MAX);
    }
}

/// 设置面板里可交互项的标识。
///
/// 绘制期登记、事件循环消费 —— 与 `widgets::HitTable` 同一思路：判定逻辑只有一份，
/// 且**绘制不修改状态**（看一帧不改设置）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsHit {
    /// 「网络」页：开始编辑 DNS（§2.4）。**只改编辑态**，真正的写入在按下回车后
    /// 经 aetherd 执行 —— 界面不直接改系统配置。
    EditDns,
    /// 切换界面语言（§3.5）。值是 `Settings::locale` 的下标。
    Locale(u8),
    /// 点击「工具权限」页的第 `i` 行：把该工具的策略循环到下一个（§5.3）。
    CycleToolPolicy(usize),
    /// 重新从 aetherd 取一次策略列表（§5.3）。
    RefreshPolicy,
    /// 安装「应用商店」页的第 `i` 项（§5.11）。
    InstallApp(usize),
    /// 重新扫描商店目录（§5.11）。
    RefreshStore,
    /// 立刻对一次时（§3.3）。
    SyncTimeNow,
    /// 切换待写入的时区名候选（§3.3）。
    CycleTimezoneName,
    /// 把待写入的时区名写进系统（§3.3）。
    SetTimezoneNow,
    /// 切换到第 n 页
    Page(usize),
    /// 24 小时制开关
    Clock24h,
    /// 显示秒开关
    ClockSeconds,
    /// 时区偏移微调（参数 = 分钟增量，正负）
    TzShift(i32),
    /// 中文输入法默认开关
    ImeDefault,
    /// 深色模式开关（默认关 = 白色）
    ToggleDark,
    /// 切换高对比度（无障碍 §3.4）。
    ToggleContrast,
    /// 鼠标速度（百分比，由分段控件给值；越界由 `apply_hit` 夹取）
    MouseSpeed(i32),
    /// **动作类**：打开内建应用（序号与 `main.rs::open_app` 一致）
    OpenApp(usize),
    /// **动作类**：重启一个系统服务（参数是 `aether-ops` 的单元名）。
    ///
    /// 这是"设置里真正能**改**系统"的能力（P4.4）：走 `Request::ServiceControl`
    /// 交给 aetherd，再由它转给 init —— **不新增 IPC 变体**（协议里本来就有），
    /// 也不在合成器里直接碰系统（那是 aetherd / init 的职责）。
    ///
    /// 单元名由调用点给出（见 `draw.rs` 的各页），目前有 `network` 与 `aetherd`。
    RestartService(&'static str),
    /// **动作类**：关机 / 重启（L3，走 `system_power` 工具 → 确认卡片 → PID 1）。
    ///
    /// 与 `RestartService` 分开而不是合并：电源是**整机级且不可逆**的，必须过确认卡片；
    /// "重启某个服务"不需要。合并成一个变体就会让确认逻辑退化成"看参数决定要不要确认"，
    /// 而那种判定迟早会漏（本仓库已经因为"按入口判"吃过一次亏，见 H-2）。
    Power(aether_ipc::PowerAction),
    /// **动作类**：注销（= 重启合成器，由 aether-init 按 `restart: true` 拉起）。
    ///
    /// 不需要确认：它是**可恢复**的（桌面几秒后回来），且当前没有账户体系 ——
    /// 假装它是"安全敏感操作"反而不诚实（见 FEATURE-GAPS §2.3）。
    Logout,
    /// 选择壁纸（批次 L-1）。越界由 `apply_hit` 夹取。
    Wallpaper(u8),
    /// 恢复默认外观（批次 L-1）：**保留时区**（它来自系统/发行配置，不是"外观"）。
    ResetDefaults,
    /// 息屏档位（§3.6）：分钟数，0 = 永不。非法值由 `apply_hit` 吸附。
    IdleBlank(u16),
    /// 性能 HUD 开关（§5.10）
    TogglePerfHud,
    /// 是否记录通知历史（§3.2）
    ToggleRecordNotifications,
    /// **动作类**：清空通知历史（§3.2）。不改设置状态，由事件循环执行。
    ClearNotifications,
    /// **动作类**：导出审计日志（§5.2）
    ExportAudit,
}

/// 设置页的声明（左栏与内容区共用同一份，避免两处各写一遍页名）。
pub struct PageDef {
    /// 英文页名（§3.5）。**每页都必须有** —— 有一条测试扫全表，空的会变红。
    pub title_en: &'static str,
    /// 英文分组名。
    pub group_en: &'static str,
    /// 页标题
    pub title: &'static str,
    /// 左栏分组
    pub group: &'static str,
    /// 是否已可用。`false` = 左栏显示为不可用（这些页要等 IPC 侧能力就绪）
    pub enabled: bool,
}

/// 左栏页面清单。
///
/// **不可用的页如实标出**：本项目不允许"点进去发现是空壳"的假页面。
impl PageDef {
    /// 按语言取页名。
    ///
    /// 注意：**分组比较仍用 `group`（原文）**，只在绘制时翻译 ——
    /// 若拿翻译后的文本互相比较，同一分组在两种语言下会因文本不同而重复出标题。
    pub fn title_in(&self, lang: crate::i18n::Lang) -> &'static str {
        crate::i18n::pick(lang, self.title, self.title_en)
    }

    pub fn group_in(&self, lang: crate::i18n::Lang) -> &'static str {
        crate::i18n::pick(lang, self.group, self.group_en)
    }
}

pub const PAGES: &[PageDef] = &[
    PageDef { title: "外观", title_en: "Appearance", group: "个性化", group_en: "Personalization", enabled: true },
    PageDef { title: "时钟与时区", title_en: "Date & Time", group: "系统", group_en: "System", enabled: true },
    PageDef { title: "输入", title_en: "Input", group: "设备", group_en: "Devices", enabled: true },
    // §5.9：快捷键总览。放在「输入」之后 —— 两者都是"怎么操作"这条线上的。
    PageDef { title: "快捷键", title_en: "Shortcuts", group: "设备", group_en: "Devices", enabled: true },
    PageDef { title: "AI", title_en: "AI", group: "智能", group_en: "Intelligence", enabled: true },
    PageDef { title: "关于", title_en: "About", group: "系统", group_en: "System", enabled: true },
    PageDef { title: "网络", title_en: "Network", group: "网络与共享", group_en: "Network & Sharing", enabled: true },
    PageDef { title: "应用", title_en: "Apps", group: "应用", group_en: "Applications", enabled: true },
    // §5.11：应用商店（列出可安装包，点一下就装）。
    PageDef { title: "应用商店", title_en: "App Store", group: "应用", group_en: "Applications", enabled: true },
    PageDef { title: "权限与隐私", title_en: "Privacy & Permissions", group: "隐私和安全性", group_en: "Privacy & Security", enabled: true },
    // §5.3：逐工具的权限策略编辑器。与上一页同组：都属"谁能动我的东西"。
    PageDef { title: "工具权限", title_en: "Tool Permissions", group: "隐私和安全性", group_en: "Privacy & Security", enabled: true },
    // §3.2：通知页（是否记录 + 清空历史）。放在最后一页：它是"回头看"的地方，
    // 不是日常要改的设置。
    PageDef { title: "通知", title_en: "Notifications", group: "系统", group_en: "System", enabled: true },
];

/// 左栏默认选中的页（第一个可用的页）。
/// 「网络」设置页里 DNS 输入框的状态（§2.4 网络写入的界面侧）。
///
/// **为什么单独一个类型**：输入框要用到光标、还要"边输边判"给即时反馈。
/// 把光标逻辑与校验都塞进 `Settings` 会让那个结构变成杂物间；
/// 分成一个小组件后，它的行为可以脱离界面单独测（本文件末尾就是这些用例）。
///
/// 光标复用 [`crate::textview::TextCursor`] —— **不重写**：插入/退格/删除/多字节
/// 字符定位都已经有实现和测试，再写一份就是两份会分叉的编辑逻辑。
#[derive(Debug, Clone, Default)]
pub struct DnsField {
    pub text: String,
    pub cursor: crate::textview::TextCursor,
    /// 是否处于编辑态（点「编辑」后为 true，回车提交或按 Esc 后为 false）。
    pub editing: bool,
    /// 上一次提交的结果（成功说明 / 拒绝理由），显示在输入框下方。
    pub feedback: Option<String>,
}

impl DnsField {
    /// 允许的最大输入长度（按**字符**计）。
    ///
    /// 为什么要上限：这是从键盘逐字符进来的字段，没有上限时按住键能把它撑到任意长，
    /// 而它每次按键都要跑一遍校验并重画 —— 那是自找的卡顿。
    /// 3 个 IPv4 加分隔符最多 50 字符左右，给 64 足够宽松。
    pub const MAX_CHARS: usize = 64;

    /// 进入编辑态，并把当前值放进输入框（用户从现状开始改，而不是从空白开始）。
    pub fn begin(&mut self, current: &str) {
        self.text = current.to_string();
        self.cursor = crate::textview::TextCursor::default();
        self.cursor.end(self.text.chars().count());
        self.editing = true;
        self.feedback = None;
    }

    /// 退出编辑态（放弃未提交的改动）。返回是否本来在编辑。
    pub fn cancel(&mut self) -> bool {
        let was = self.editing;
        self.editing = false;
        self.text.clear();
        self.feedback = None;
        was
    }

    /// 插入一个字符。超过上限则**忽略**（而不是截断已有内容 —— 悄悄吃掉用户输入更糟）。
    pub fn insert(&mut self, ch: char) -> bool {
        if self.text.chars().count() >= Self::MAX_CHARS {
            return false;
        }
        self.cursor.insert(&mut self.text, ch);
        self.feedback = None;
        true
    }

    pub fn backspace(&mut self) -> bool {
        self.feedback = None;
        self.cursor.backspace(&mut self.text)
    }

    /// 提交前校验（**用共享实现**，与写入侧同一份判据）。
    ///
    /// 界面**不重复实现**校验规则：两侧各写一份必然分叉，而分叉的后果是
    /// "界面说可以、服务端拒绝"或更糟的反过来。
    pub fn validate(&self) -> Result<Vec<std::net::Ipv4Addr>, String> {
        aether_ipc::parse_dns_list(&self.text)
    }

    /// 记录提交结果并退出编辑态（成功时保留文本，失败时**保留原文**让用户接着改）。
    pub fn finish(&mut self, result: Result<String, String>) {
        match result {
            Ok(msg) => {
                self.feedback = Some(msg);
                self.editing = false;
            }
            Err(why) => {
                // 失败时**不退出编辑态**：用户正要改，把他踢出去等于让他重来一遍
                self.feedback = Some(why);
            }
        }
    }
}

pub fn first_enabled_page() -> usize {
    PAGES.iter().position(|p| p.enabled).unwrap_or(0)
}

/// 应用一次设置点击。
///
/// 放在这里而不是 main.rs：这是**纯逻辑**，跟设置一起被单测覆盖更划算 ——
/// 设置中心的交互不该只能靠手动点着试。
///
/// 两条规则：**不可用的页拒绝切换**（否则左栏那几行"待接入"会点出空页），
/// **数值改动一律走夹取**（时区不能跑到 UTC+99）。
pub fn apply_hit(s: &mut Settings, page: &mut usize, hit: SettingsHit) {
    match hit {
        SettingsHit::Page(i) => {
            if PAGES.get(i).map(|p| p.enabled).unwrap_or(false) {
                *page = i;
            }
        }
        SettingsHit::Clock24h => s.clock_24h = !s.clock_24h,
        SettingsHit::ClockSeconds => s.clock_seconds = !s.clock_seconds,
        SettingsHit::TzShift(d) => s.shift_tz(d),
        SettingsHit::ImeDefault => s.ime_default = !s.ime_default,
        SettingsHit::ToggleDark => s.dark_mode = !s.dark_mode,
        SettingsHit::ToggleContrast => s.high_contrast = !s.high_contrast,
        SettingsHit::Locale(i) => s.locale = i.min(LOCALE_COUNT.saturating_sub(1)),
        SettingsHit::MouseSpeed(p) => s.mouse_speed_pct = p.clamp(MOUSE_SPEED_MIN, MOUSE_SPEED_MAX),
        // 动作类命中**不改设置状态**：它们由事件循环执行（打开窗口/程序/电源动作），
        // 放这里只是为了让 `apply_hit` 对枚举保持穷尽（漏一个变体会编译不过）。
        SettingsHit::OpenApp(_)
        | SettingsHit::RestartService(_)
        | SettingsHit::Power(_)
        | SettingsHit::Logout
        // 「修改 DNS」同样**不改设置状态**：编辑态住在 `Renderer::dns`
        // （界面状态，不是要落盘的设置），由 main.rs 的命中分派处理。
        | SettingsHit::EditDns
        // 「工具权限」的行点击与刷新：要发 IPC 与 aetherd 来回，由事件循环处理。
        | SettingsHit::CycleToolPolicy(_)
        | SettingsHit::RefreshPolicy
        // 「应用商店」的安装与重扫同理：要发 IPC，由事件循环处理。
        | SettingsHit::InstallApp(_)
        | SettingsHit::RefreshStore
        // §3.3 的三条同理：要发 IPC，由事件循环处理。
        | SettingsHit::SyncTimeNow
        | SettingsHit::CycleTimezoneName
        | SettingsHit::SetTimezoneNow => {}
        // 壁纸是**状态类**改动：立即写进 settings（调用方随后落盘 + 重建背景缓存）
        SettingsHit::Wallpaper(i) => {
            s.wallpaper = i.min(WALLPAPER_COUNT.saturating_sub(1));
        }
        // 恢复默认：重置除时区外的全部字段。
        // **为什么保留时区**：它来自系统/发行配置（`TZ`），不是"外观偏好" ——
        // 把用户所在时区重置成 +8 属于"顺手改了一个他没要求改的东西"。
        SettingsHit::ResetDefaults => {
            let tz = s.tz_offset_min;
            *s = Settings::default();
            s.tz_offset_min = tz;
        }
        SettingsHit::IdleBlank(min) => {
            s.idle_blank_min = snap_idle_blank(min);
        }
        SettingsHit::TogglePerfHud => s.perf_hud = !s.perf_hud,
        SettingsHit::ToggleRecordNotifications => {
            s.record_notifications = !s.record_notifications
        }
        // 动作类：清空历史/导出审计由事件循环执行（`apply_hit` 不改状态）
        SettingsHit::ClearNotifications | SettingsHit::ExportAudit => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_sane() {
        let s = Settings::default();
        assert_eq!(s.tz_offset_min, 8 * 60);
        assert!(s.clock_24h);
        assert_eq!(s.tz_label(), "UTC+08:00");
        let neg = Settings { tz_offset_min: -330, ..Settings::default() };
        assert_eq!(neg.tz_label(), "UTC-05:30");
    }

    #[test]
    fn json_round_trip() {
        let s = Settings {
            tz_offset_min: -330,
            clock_24h: false,
            clock_seconds: true,
            ime_default: true,
            dark_mode: true,
            // 无障碍字段也用**非默认值**往返一次：默认值会让"没写进 JSON"这类 bug 溜过去
            high_contrast: true,
            mouse_speed_pct: 150,
            // 批次 I 新字段也一起过一遍往返（漏了就会在"存了没生效"这类 bug 里翻车）
            wallpaper: 3,
            ui_scale_pct: 125,
            default_app: 2,
            notify_sound: false,
            locale: 1,
            idle_blank_min: 10,
            perf_hud: false,
            record_notifications: true,
            // §5.8：全新安装时向导要弹（旧文件缺这个键会被当成「已完成」，见 from_json_with）
            setup_done: false,
        };
        assert_eq!(Settings::from_json(&s.to_json()), s);
    }

    /// 批次 I 回归：**旧设置文件必须能读**（新字段走默认），新字段越界必须被夹取。
    ///
    /// 设置文件是可被手工改的（甚至可能是上个版本写的），所以两类容错都要有：
    /// 缺键 → 默认；越界/类型错 → 夹取或回落。缺任何一类都会让用户遇到
    /// "改了设置，重启后界面崩了/比例是 137% 这种鬼值"。
    #[test]
    fn new_fields_are_backward_compatible_and_clamped() {
        // 旧文件（只有批次 I 之前的键）→ 新字段取默认，旧字段照旧生效
        let old = r#"{"version":1,"tz_offset_min":-330,"dark_mode":true,"mouse_speed_pct":150}"#;
        let s = Settings::from_json(old);
        assert_eq!(s.tz_offset_min, -330, "旧字段仍要生效");
        assert!(s.dark_mode);
        assert_eq!(s.wallpaper, Settings::default().wallpaper, "缺键应取默认");
        assert_eq!(s.ui_scale_pct, 100);
        assert!(s.notify_sound);
        assert_eq!(s.locale, 0);

        // 越界夹取：壁纸/默认应用/语言按下标上限，缩放吸附到最近档
        assert_eq!(Settings::from_json(r#"{"wallpaper": 99}"#).wallpaper, WALLPAPER_COUNT - 1);
        assert_eq!(Settings::from_json(r#"{"default_app": 99}"#).default_app, DEFAULT_APP_COUNT - 1);
        assert_eq!(Settings::from_json(r#"{"locale": 9}"#).locale, LOCALE_COUNT - 1);
        assert_eq!(Settings::from_json(r#"{"ui_scale_pct": 137}"#).ui_scale_pct, 125, "吸附到最近档");
        assert_eq!(Settings::from_json(r#"{"ui_scale_pct": 1}"#).ui_scale_pct, 100);
        assert_eq!(Settings::from_json(r#"{"ui_scale_pct": 9999}"#).ui_scale_pct, 150);
        // 类型不对 → 回落默认（而不是 panic 或取到 0）
        assert_eq!(Settings::from_json(r#"{"wallpaper": "花"}"#).wallpaper, 0);
        assert_eq!(Settings::from_json(r#"{"notify_sound": "是"}"#).notify_sound, true);
    }

    /// 缩放的允许档位只有三档，且 `snap_scale` 必须覆盖整条数轴（不产生越界值）。
    #[test]
    fn ui_scale_snaps_to_declared_steps_only() {
        for v in [0u16, 1, 99, 100, 101, 112, 113, 124, 125, 126, 137, 149, 150, 151, 60000] {
            let got = snap_scale(v);
            assert!(
                UI_SCALE_STEPS.contains(&got),
                "snap_scale({v}) = {got} 不在允许档位里"
            );
        }
        // 最近档语义：112 更接近 100，113 更接近 125
        assert_eq!(snap_scale(112), 100);
        assert_eq!(snap_scale(113), 125);
    }

    #[test]
    fn corrupt_or_partial_json_falls_back_per_field() {
        // 完全损坏 → 全默认
        assert_eq!(Settings::from_json("{ 这不是 json"), Settings::default());
        assert_eq!(Settings::from_json(""), Settings::default());
        assert_eq!(Settings::from_json("null"), Settings::default());
        // 部分字段 + 类型错误 → 只取能用的，其余默认
        let s = Settings::from_json(r#"{"clock_24h": false, "tz_offset_min": "八点"}"#);
        assert!(!s.clock_24h, "合法字段要生效");
        assert_eq!(s.tz_offset_min, 8 * 60, "类型不对的字段回落默认");
        // 越界时区要夹取（不能出现 UTC+99:00）
        let s = Settings::from_json(r#"{"tz_offset_min": 99999}"#);
        assert_eq!(s.tz_offset_min, TZ_MAX);
        let s = Settings::from_json(r#"{"tz_offset_min": -99999}"#);
        assert_eq!(s.tz_offset_min, TZ_MIN);
        // 鼠标速度：类型不对回落默认，越界夹取（写成 0 会让指针彻底不动）
        assert_eq!(Settings::from_json(r#"{"mouse_speed_pct": "快"}"#).mouse_speed_pct, 100);
        assert_eq!(Settings::from_json(r#"{"mouse_speed_pct": 0}"#).mouse_speed_pct, MOUSE_SPEED_MIN);
        assert_eq!(
            Settings::from_json(r#"{"mouse_speed_pct": 99999}"#).mouse_speed_pct,
            MOUSE_SPEED_MAX
        );
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("aether-settings-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let path = path.to_str().unwrap();
        let s = Settings {
            tz_offset_min: 330,
            clock_24h: false,
            clock_seconds: true,
            ime_default: false,
            dark_mode: true,
            high_contrast: true,
            mouse_speed_pct: 60,
            // 非默认值：真正验证"写进去→读出来"包含批次 I 的新字段
            wallpaper: 4,
            ui_scale_pct: 150,
            default_app: 1,
            notify_sound: false,
            locale: 1,
            idle_blank_min: 10,
            perf_hud: false,
            record_notifications: true,
            // §5.8：全新安装时向导要弹（旧文件缺这个键会被当成「已完成」，见 from_json_with）
            setup_done: false,
        };
        s.save(path).expect("写设置应当成功（父目录会自动建）");
        assert_eq!(Settings::load(path, 8 * 60), s, "写进去什么，读出来就该是什么");
        // 临时文件不应残留（原子写）
        assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());
        std::fs::remove_dir_all(&dir).ok();
        // 不存在的路径 → 默认值（时区用调用方给的兜底），不 panic
        let none = Settings::load("/nonexistent/dir/settings.json", 8 * 60);
        assert_eq!(none, Settings::default());
        assert_eq!(
            Settings::load("/nonexistent/dir/settings.json", -300).tz_offset_min,
            -300,
            "没有设置文件时应采用调用方给的默认时区（来自 TZ）"
        );
    }

    #[test]
    fn pages_are_consistent() {
        assert!(!PAGES.is_empty());
        // 标题唯一：左栏重复页名会让用户点错
        for (i, a) in PAGES.iter().enumerate() {
            for b in &PAGES[i + 1..] {
                assert_ne!(a.title, b.title, "页标题重复：{}", a.title);
            }
        }
        // 至少要有一个可用页，且首选项必须真的是可用的
        assert!(PAGES.iter().any(|p| p.enabled), "至少一页要能用");
        assert!(PAGES[first_enabled_page()].enabled);
    }

    /// 点击 → 状态变化：开关要翻转、页面要切换、时区要夹取。
    #[test]
    fn click_applies_and_clamps() {
        let mut s = Settings::default();
        let mut page = first_enabled_page();
        let start = page;

        apply_hit(&mut s, &mut page, SettingsHit::Clock24h);
        assert!(!s.clock_24h, "第一次点击应关掉 24 小时制");
        apply_hit(&mut s, &mut page, SettingsHit::Clock24h);
        assert!(s.clock_24h, "再点一次应还原");

        apply_hit(&mut s, &mut page, SettingsHit::ClockSeconds);
        assert!(s.clock_seconds);
        apply_hit(&mut s, &mut page, SettingsHit::ImeDefault);
        assert!(s.ime_default);

        // 主题开关：默认必须是**关**（= 白色/明亮，产品主视觉），点一下才变深色
        assert!(!Settings::default().dark_mode, "默认必须是明亮主题");
        apply_hit(&mut s, &mut page, SettingsHit::ToggleDark);
        assert!(s.dark_mode, "第一次点击应切到深色");
        apply_hit(&mut s, &mut page, SettingsHit::ToggleDark);
        assert!(!s.dark_mode, "再点一次应回到明亮");

        // 鼠标速度：默认必须是 1:1，越界值要夹取
        assert_eq!(Settings::default().mouse_speed_pct, 100, "默认必须是 1:1（原来是 ×2）");
        apply_hit(&mut s, &mut page, SettingsHit::MouseSpeed(60));
        assert_eq!(s.mouse_speed_pct, 60);
        apply_hit(&mut s, &mut page, SettingsHit::MouseSpeed(0));
        assert_eq!(s.mouse_speed_pct, MOUSE_SPEED_MIN, "过小的速度要夹到下限");
        apply_hit(&mut s, &mut page, SettingsHit::MouseSpeed(9999));
        assert_eq!(s.mouse_speed_pct, MOUSE_SPEED_MAX, "过大的速度要夹到上限");

        apply_hit(&mut s, &mut page, SettingsHit::TzShift(-30));
        assert_eq!(s.tz_offset_min, 8 * 60 - 30);        for _ in 0..100 {
            apply_hit(&mut s, &mut page, SettingsHit::TzShift(-30));
        }
        assert_eq!(s.tz_offset_min, TZ_MIN, "时区必须夹在 -14:00");
        for _ in 0..200 {
            apply_hit(&mut s, &mut page, SettingsHit::TzShift(30));
        }
        assert_eq!(s.tz_offset_min, TZ_MAX, "时区必须夹在 +14:00");

        // 可用页能切；不可用页拒绝切（否则左栏"待接入"会点出空页）。
        //
        // 2026-10-02：网络 / 应用 / 权限与隐私三页全部接入后，**已经没有任何不可用页**。
        // 所以这条规则改成**有条件断言**：有不可用页才验，没有就跳过 ——
        // 规则本身仍由 `apply_hit` 保证（将来若再加占位页，这里会自动重新生效）。
        let enabled = PAGES.iter().position(|p| p.enabled).expect("至少要有一个可用页");
        apply_hit(&mut s, &mut page, SettingsHit::Page(enabled));
        assert_eq!(page, enabled);
        if let Some(disabled) = PAGES.iter().position(|p| !p.enabled) {
            apply_hit(&mut s, &mut page, SettingsHit::Page(disabled));
            assert_eq!(page, enabled, "不可用页不该被切过去");
        }
        apply_hit(&mut s, &mut page, SettingsHit::Page(9999));
        assert_eq!(page, enabled, "越界页索引必须被忽略");
        let _ = start;
    }
}

#[cfg(test)]
mod restart_action_tests {
    use super::*;

    /// 动作类命中**只**产生副作用（发 IPC），不得改动任何设置状态。
    ///
    /// 这条测的是"点了重启不会顺手改配置"——设置项与当前页都必须原样不动。
    #[test]
    fn restart_hit_is_an_action_not_state() {
        let mut st = Settings::default();
        let before = st.clone();
        let mut page = 3usize;
        for unit in ["network", "aetherd"] {
            apply_hit(&mut st, &mut page, SettingsHit::RestartService(unit));
        }
        assert_eq!(page, 3, "动作类命中不得切换设置页");
        assert_eq!(st.tz_offset_min, before.tz_offset_min);
        assert_eq!(st.clock_24h, before.clock_24h);
        assert_eq!(st.dark_mode, before.dark_mode);
        assert_eq!(st.mouse_speed_pct, before.mouse_speed_pct);
    }

    /// 批次 J-2：电源与注销同样是**动作类**命中 —— 不得改动设置状态或当前页。
    ///
    /// 这条比 `restart_hit_...` 更重要一点：电源动作如果顺手把设置改了（或把页面
    /// 跳走了），用户点"关机"却看到界面变化，会以为点错了。
    #[test]
    fn power_and_logout_hits_are_actions_not_state() {
        let mut st = Settings::default();
        let before = st.clone();
        let mut page = 2usize;
        for hit in [
            SettingsHit::Power(aether_ipc::PowerAction::PowerOff),
            SettingsHit::Power(aether_ipc::PowerAction::Reboot),
            SettingsHit::Logout,
        ] {
            apply_hit(&mut st, &mut page, hit);
        }
        assert_eq!(page, 2, "动作类命中不得切换设置页");
        assert_eq!(st, before, "动作类命中不得改动任何设置值");
    }

    /// 批次 L-1：壁纸选择必须**夹取**（设置文件/命中参数都可能越界），且真的写进状态。
    #[test]
    fn wallpaper_hit_clamps_and_applies() {
        let mut st = Settings::default();
        let mut page = 0usize;
        apply_hit(&mut st, &mut page, SettingsHit::Wallpaper(3));
        assert_eq!(st.wallpaper, 3, "合法值应写入");
        apply_hit(&mut st, &mut page, SettingsHit::Wallpaper(200));
        assert_eq!(
            st.wallpaper,
            WALLPAPER_COUNT - 1,
            "越界应夹取到最后一档，而不是溢出或 panic"
        );
        assert_eq!(page, 0, "选壁纸不该切页");
    }

    /// 批次 L-1：**恢复默认真的重置**，但**保留时区**。
    ///
    /// 时区来自系统/发行配置（`TZ`），不是"外观偏好" —— 顺手把它重置成 +8
    /// 属于"改了一个用户没要求改的东西"，是这类按钮最容易犯的错。
    #[test]
    fn reset_defaults_resets_all_but_timezone() {
        let mut st = Settings {
            tz_offset_min: -330, // UTC-05:30（用户所在时区）
            dark_mode: true,
            wallpaper: 4,
            ui_scale_pct: 150,
            default_app: 2,
            notify_sound: false,
            locale: 1,
            idle_blank_min: 10,
            perf_hud: false,
            record_notifications: true,
            ..Settings::default()
        };
        let mut page = 1usize;
        apply_hit(&mut st, &mut page, SettingsHit::ResetDefaults);
        assert_eq!(st.tz_offset_min, -330, "时区必须保留");
        let want = Settings::default();
        assert_eq!(st.dark_mode, want.dark_mode);
        assert_eq!(st.wallpaper, want.wallpaper);
        assert_eq!(st.ui_scale_pct, want.ui_scale_pct);
        assert_eq!(st.default_app, want.default_app);
        assert_eq!(st.notify_sound, want.notify_sound);
        assert_eq!(st.locale, want.locale);
        assert_eq!(page, 1, "恢复默认不该切页");
    }

    /// 批次 L-1：色板表必须是**真的不同**，且索引 0 = 跟随主题（默认行为不变）。
    #[test]
    fn wallpaper_palette_is_distinct_and_index0_follows_theme() {
        // 索引 0 必须与主题色一致：否则"默认壁纸"会被静默改掉（等于给所有人换了壁纸）
        let (top, bottom) = crate::draw::wallpaper_colors(0, false);
        assert_eq!((top, bottom), crate::draw::wallpaper_colors(0, true), "0 = 跟随主题");
        // 各档之间必须可区分（否则"能换"是假的）
        let mut seen = std::collections::HashSet::new();
        for i in 0..WALLPAPER_COUNT {
            let (t, b) = crate::draw::wallpaper_colors(i, false);
            assert!(seen.insert((t, b)), "第 {i} 档与前面某档配色相同（换了个寂寞）");
        }
        // 越界必须回退到默认，而不是 panic
        assert_eq!(
            crate::draw::wallpaper_colors(99, false),
            crate::draw::wallpaper_colors(0, false)
        );
        // 深色模式下同一档必须给出不同（更暗）的配色，否则深色主题下色板会刺眼
        let (light_top, _) = crate::draw::wallpaper_colors(1, false);
        let (dark_top, _) = crate::draw::wallpaper_colors(1, true);
        let lum = |c: [u8; 3]| c[0] as u32 + c[1] as u32 + c[2] as u32;
        assert!(lum(dark_top) < lum(light_top), "深色档应更暗");
    }
    /// §3.6：息屏判定是**纯函数** —— 不必等十分钟就能验。
    ///
    /// 三条边界必须钉住：0 = 永不（**不能被"默认值"覆盖**）、阈值前不息、阈值后息。
    #[test]
    fn idle_blank_respects_never_and_threshold() {
        use std::time::Duration;
        let never = Settings { idle_blank_min: 0, ..Settings::default() };
        assert_eq!(never.idle_blank_secs(), None);
        // 永不档：无论空闲多久都不息屏（演示/排障时正是要这个）
        assert!(!never.should_blank(Duration::from_secs(60 * 60 * 24)));

        let five = Settings { idle_blank_min: 5, ..Settings::default() };
        assert_eq!(five.idle_blank_secs(), Some(300));
        assert!(!five.should_blank(Duration::from_secs(299)), "差一秒不该息屏");
        assert!(five.should_blank(Duration::from_secs(300)), "到点就该息屏");
        assert!(five.should_blank(Duration::from_secs(301)));
    }

    /// §3.6：非法档位必须**吸附到最近合法档**，而不是原样接受。
    ///
    /// 否则手工改过的设置文件会让界面显示一个选不中的值（"高亮没有落在任何按钮上"）。
    #[test]
    fn idle_blank_snaps_to_declared_steps() {
        assert_eq!(snap_idle_blank(0), 0);
        assert_eq!(snap_idle_blank(4), 5, "4 离 5 更近");
        assert_eq!(snap_idle_blank(7), 5, "7 离 5 比离 10 近");
        assert_eq!(snap_idle_blank(8), 10, "8 离 10 更近");
        assert_eq!(snap_idle_blank(25), 30);
        assert_eq!(snap_idle_blank(9999), 30, "超大值吸附到最大档，不溢出");
        // 每个档位本身必须是不动点
        for o in IDLE_BLANK_OPTIONS {
            assert_eq!(snap_idle_blank(o), o, "{o} 应是不动点");
        }

        let mut st = Settings::default();
        let mut page = 0usize;
        apply_hit(&mut st, &mut page, SettingsHit::IdleBlank(9999));
        assert_eq!(st.idle_blank_min, 30, "命中里的非法值也要吸附");
        apply_hit(&mut st, &mut page, SettingsHit::IdleBlank(0));
        assert_eq!(st.idle_blank_min, 0, "0（永不）必须能选中");
        assert_eq!(page, 0, "改息屏不该切页");
    }

    /// §3.6：旧设置文件（没有这个键）必须拿到默认值，且新文件能往返。
    #[test]
    fn idle_blank_is_backward_compatible() {
        // 旧文件：没有 idle_blank_min → 用默认（10 分钟）
        let old = r#"{"version":1,"dark_mode":true}"#;
        let s = Settings::from_json(old);
        assert_eq!(s.idle_blank_min, Settings::default().idle_blank_min);
        assert!(s.dark_mode, "旧文件的其它字段仍要读进来");

        // 新文件往返
        let s = Settings { idle_blank_min: 30, ..Settings::default() };
        let back = Settings::from_json(&s.to_json());
        assert_eq!(back.idle_blank_min, 30);

        // 手工改成非法值 → 吸附（而不是让界面显示一个选不中的档）
        let weird = Settings::from_json(r#"{"idle_blank_min": 77}"#);
        assert_eq!(weird.idle_blank_min, snap_idle_blank(77));
        // 类型不对（字符串）→ 走默认，不 panic
        let bad = Settings::from_json(r#"{"idle_blank_min": "十分钟"}"#);
        assert_eq!(bad.idle_blank_min, Settings::default().idle_blank_min);
    }

    /// §5.10：性能 HUD 默认**关**（排障工具，不是常显装饰），且能往返持久化。
    #[test]
    fn perf_hud_defaults_off_and_round_trips() {
        assert!(!Settings::default().perf_hud, "HUD 必须默认关");
        let mut st = Settings::default();
        let mut page = 0usize;
        apply_hit(&mut st, &mut page, SettingsHit::TogglePerfHud);
        assert!(st.perf_hud, "点一下应打开");
        apply_hit(&mut st, &mut page, SettingsHit::TogglePerfHud);
        assert!(!st.perf_hud, "再点应关闭");
        assert_eq!(page, 0, "开关不该切页");

        let on = Settings { perf_hud: true, ..Settings::default() };
        assert!(Settings::from_json(&on.to_json()).perf_hud, "开启状态要能存下来");
        // 旧文件缺该键 → 默认关（不会因为升级就冒出一块 HUD）
        assert!(!Settings::from_json(r#"{"dark_mode":true}"#).perf_hud);
        // 类型不对 → 默认，不 panic
        assert!(!Settings::from_json(r#"{"perf_hud":"开"}"#).perf_hud);
    }

    /// N1-d：通知记录开关默认**开**、能往返、旧文件走默认。
    #[test]
    fn record_notifications_defaults_on_and_round_trips() {
        assert!(Settings::default().record_notifications, "默认应记录");
        let mut st = Settings::default();
        let mut page = 0usize;
        apply_hit(&mut st, &mut page, SettingsHit::ToggleRecordNotifications);
        assert!(!st.record_notifications, "点一下应关闭");
        apply_hit(&mut st, &mut page, SettingsHit::ToggleRecordNotifications);
        assert!(st.record_notifications);
        // 清空是**动作类**：不该改动任何设置状态（否则"点清空"会顺手改开关）
        let before = st.clone();
        apply_hit(&mut st, &mut page, SettingsHit::ClearNotifications);
        assert_eq!(st, before, "清空历史不得改动设置状态");

        let off = Settings { record_notifications: false, ..Settings::default() };
        assert!(!Settings::from_json(&off.to_json()).record_notifications, "关闭状态要能存下来");
        assert!(Settings::from_json(r#"{"dark_mode":true}"#).record_notifications, "旧文件走默认（开）");
    }

    /// §2.4 界面侧：DNS 输入字段的行为（脱离界面可测）。
    #[test]
    fn dns_field_edits_like_a_text_box() {
        let mut f = DnsField::default();
        assert!(!f.editing);
        f.begin("1.1.1.1");
        assert!(f.editing);
        // begin 把光标放在末尾（用户从现状接着改）
        assert_eq!(f.cursor.pos(), "1.1.1.1".chars().count());
        // 继续输入
        for c in ", 8.8.8.8".chars() {
            assert!(f.insert(c));
        }
        assert_eq!(f.text, "1.1.1.1, 8.8.8.8");
        assert!(f.validate().is_ok());
        // 退格
        assert!(f.backspace());
        assert_eq!(f.text, "1.1.1.1, 8.8.8.");
        assert!(f.validate().is_err(), "退格后应当不合法");
        // 取消：清空并退出
        assert!(f.cancel());
        assert!(!f.editing && f.text.is_empty());
        assert!(!f.cancel(), "已退出时再取消应返回 false");
    }

    /// 上限是**忽略**新输入，而不是截断已有内容（悄悄吃掉用户输入更糟）。
    #[test]
    fn dns_field_caps_length_without_eating_input() {
        let mut f = DnsField::default();
        f.begin("");
        for _ in 0..DnsField::MAX_CHARS {
            assert!(f.insert('a'));
        }
        let before = f.text.clone();
        assert!(!f.insert('b'), "超过上限应被忽略");
        assert_eq!(f.text, before, "不应改动已有内容");
        assert_eq!(f.text.chars().count(), DnsField::MAX_CHARS);
    }

    /// 提交失败时**留在编辑态**并把理由显示出来（用户接着改，不用重来）。
    #[test]
    fn dns_field_keeps_editing_on_rejection() {
        let mut f = DnsField::default();
        f.begin("999.1.1.1");
        f.finish(Err("越界".into()));
        assert!(f.editing, "被拒时应仍在编辑态");
        assert_eq!(f.feedback.as_deref(), Some("越界"));
        assert_eq!(f.text, "999.1.1.1", "被拒时不得清空用户输入");
        // 成功则退出编辑态
        f.begin("1.1.1.1");
        f.finish(Ok("DNS 已设为 1.1.1.1".into()));
        assert!(!f.editing);
        assert!(f.feedback.unwrap().contains("1.1.1.1"));
    }

    /// 校验必须走**共享实现**（与写入侧同一份判据），不在这里重写规则。
    #[test]
    fn dns_field_validation_matches_the_writer() {
        let mut f = DnsField::default();
        for (input, ok) in [
            ("1.1.1.1", true),
            ("1.1.1.1, 8.8.8.8, 9.9.9.9", true),
            ("", false),
            ("01.2.3.4", false),
            ("::1", false),
            ("1.1.1.1; reboot", false),
        ] {
            f.begin(input);
            assert_eq!(
                f.validate().is_ok(),
                ok,
                "{input:?} 的判定应与写入侧一致（共享 parse_dns_list）"
            );
        }
    }

    /// 多字节字符下光标不能切坏字符串（复用 TextCursor 的意义所在）。
    #[test]
    fn dns_field_handles_multibyte_input_safely() {
        let mut f = DnsField::default();
        f.begin("中文");
        // begin 把光标放在**末尾**（pos = 2）
        assert_eq!(f.cursor.pos(), 2);
        f.insert('好');
        assert_eq!(f.text, "中文好");
        assert_eq!(f.cursor.pos(), 3);
        // 退格删掉光标前那个字符，即刚插入的「好」
        assert!(f.backspace(), "退格应删掉光标前的字符");
        assert_eq!(f.text, "中文", "删的是最后插入的「好」");
        assert_eq!(f.cursor.pos(), 2, "光标应随之前移一格");
        // 再退格一次轮到「文」——多字节定位不能切坏字符串
        assert!(f.backspace());
        assert_eq!(f.text, "中");
        assert_eq!(f.cursor.pos(), 1);
        // 关键：不能 panic、不能产生非法 UTF-8（String 保证，但插入位置算错会 panic）
        assert_eq!(f.validate().is_err(), true);
    }

    /// §3.4：高对比度开关要能改、能存、能读回来（与深色模式同一条路径）。
    #[test]
    fn high_contrast_toggles_and_round_trips() {
        let mut s = Settings::default();
        assert!(!s.high_contrast, "默认必须是关（不改动默认观感）");
        let mut page = 0usize;
        apply_hit(&mut s, &mut page, SettingsHit::ToggleContrast);
        assert!(s.high_contrast, "第一次点击应开启");
        let json = s.to_json();
        assert!(json.contains("high_contrast"), "必须落盘：{json}");
        assert!(Settings::from_json(&json).high_contrast, "读回来必须还是开");
        apply_hit(&mut s, &mut page, SettingsHit::ToggleContrast);
        assert!(!s.high_contrast, "再点一次应关闭");
        // 旧文件没有这个字段 → 走默认（关），不能因此读失败
        assert!(!Settings::from_json(r#"{"dark_mode":true}"#).high_contrast);
    }

    /// §3.5：**每一页都必须有非空英文名**。
    ///
    /// 这是防"翻了一半"的断言：新增页面时忘了写英文名，界面上会是一片空白，
    /// 而这在中文环境下根本看不出来（写这条测试的作者也是中文用户）。
    #[test]
    fn every_page_has_an_english_title_and_group() {
        use crate::i18n::Lang;
        for (i, p) in PAGES.iter().enumerate() {
            assert!(
                !p.title_en.trim().is_empty(),
                "第 {i} 页（{}）缺少英文页名",
                p.title
            );
            assert!(
                !p.group_en.trim().is_empty(),
                "第 {i} 页（{}）缺少英文分组名",
                p.title
            );
            // 英文名不该和中文名一模一样（那多半是没翻、直接复制过来的）
            if p.title.chars().any(|c| !c.is_ascii()) {
                assert_ne!(p.title, p.title_en, "第 {i} 页的英文名与中文名相同");
            }
            // 两种语言都能取到
            assert_eq!(p.title_in(Lang::Zh), p.title);
            assert_eq!(p.title_in(Lang::En), p.title_en);
        }
    }

    /// §3.5：语言开关要走真实的 `apply_hit` 路径，并且**越界会被夹取**。
    #[test]
    fn locale_hit_clamps_to_known_languages() {
        let mut s = Settings::default();
        let mut page = 0usize;
        assert_eq!(s.locale, 0, "默认中文");
        apply_hit(&mut s, &mut page, SettingsHit::Locale(1));
        assert_eq!(s.locale, 1);
        apply_hit(&mut s, &mut page, SettingsHit::Locale(99));
        assert_eq!(s.locale, LOCALE_COUNT - 1, "越界要夹到最后一档，而不是越界");
    }

    /// §5.8：向导开关要能存下来；**老文件（没有这个键）必须视为已完成**。
    #[test]
    fn setup_done_round_trips_and_defaults_to_done_for_old_files() {
        assert!(!Settings::default().setup_done, "全新安装时应当弹出向导");
        let done = Settings { setup_done: true, ..Settings::default() };
        assert!(Settings::from_json(&done.to_json()).setup_done);
        // **旧文件没有 setup_done 键 → 视为已完成**（否则升级后会被弹向导骚扰）
        assert!(
            Settings::from_json(r#"{"dark_mode":true,"locale":1}"#).setup_done,
            "缺键必须当成已完成（老用户不该被弹向导）"
        );
        // 显式写成 false 时要尊重（例如想重看一遍向导）
        assert!(!Settings::from_json(r#"{"setup_done":false}"#).setup_done, "显式 false 要生效");
        // 键存在但不是布尔（文件被手改坏）→ 按"有字段"处理（不弹向导），不接受奇怪状态
        assert!(Settings::from_json(r#"{"setup_done":"yes"}"#).setup_done);
        // 退化文档（没有字段）**不算**"用过系统"：向导该弹
        assert!(!Settings::from_json("null").setup_done, "null 不该被当成设置文件");
        assert!(!Settings::from_json("{}").setup_done, "空对象不该被当成设置文件");
    }

}
