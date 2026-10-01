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
    /// 鼠标速度（百分比，100 = 1:1）。
    ///
    /// 合成器原先对相对位移**硬编码 ×2**，在 VMware/真机上太快（用户实测反馈）；
    /// 默认改为 100（1:1），用户可在设置中心「输入」页调。
    pub mouse_speed_pct: i32,
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
            mouse_speed_pct: 100,
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
        if let Some(b) = v.get("dark_mode").and_then(Value::as_bool) {
            s.dark_mode = b;
        }
        if let Some(n) = v.get("mouse_speed_pct").and_then(Value::as_i64) {
            s.mouse_speed_pct = (n as i32).clamp(MOUSE_SPEED_MIN, MOUSE_SPEED_MAX);
        }
        s
    }

    /// 序列化（带 schema 版本号，便于以后迁移时判断）。
    pub fn to_json(&self) -> String {
        let v = json!({
            "version": 1,
            "tz_offset_min": self.tz_offset_min,
            "clock_24h": self.clock_24h,
            "clock_seconds": self.clock_seconds,
            "ime_default": self.ime_default,
            "dark_mode": self.dark_mode,
            "mouse_speed_pct": self.mouse_speed_pct,
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
    /// 鼠标速度（百分比，由分段控件给值；越界由 `apply_hit` 夹取）
    MouseSpeed(i32),
    /// **动作类**：打开内建应用（序号与 `main.rs::open_app` 一致）
    OpenApp(usize),
}

/// 设置页的声明（左栏与内容区共用同一份，避免两处各写一遍页名）。
pub struct PageDef {
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
pub const PAGES: &[PageDef] = &[
    PageDef { title: "外观", group: "个性化", enabled: true },
    PageDef { title: "时钟与时区", group: "系统", enabled: true },
    PageDef { title: "输入", group: "设备", enabled: true },
    PageDef { title: "AI", group: "智能", enabled: true },
    PageDef { title: "关于", group: "系统", enabled: true },
    PageDef { title: "网络", group: "网络与共享", enabled: true },
    PageDef { title: "应用", group: "应用", enabled: true },
    PageDef { title: "权限与隐私", group: "隐私和安全性", enabled: false },
];

/// 左栏默认选中的页（第一个可用的页）。
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
        SettingsHit::MouseSpeed(p) => s.mouse_speed_pct = p.clamp(MOUSE_SPEED_MIN, MOUSE_SPEED_MAX),
        // 动作类命中**不改设置状态**：它们由事件循环执行（打开窗口/程序），
        // 放这里只是为了让 `apply_hit` 对枚举保持穷尽（漏一个变体会编译不过）。
        SettingsHit::OpenApp(_) => {}
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
            mouse_speed_pct: 150,
        };
        assert_eq!(Settings::from_json(&s.to_json()), s);
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
            mouse_speed_pct: 60,
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

        // 可用页能切、不可用页拒绝切（否则左栏"待接入"会点出空页）
        let enabled = PAGES.iter().position(|p| p.enabled).unwrap();
        let disabled = PAGES.iter().position(|p| !p.enabled).unwrap();
        apply_hit(&mut s, &mut page, SettingsHit::Page(enabled));
        assert_eq!(page, enabled);
        apply_hit(&mut s, &mut page, SettingsHit::Page(disabled));
        assert_eq!(page, enabled, "不可用页不该被切过去");
        apply_hit(&mut s, &mut page, SettingsHit::Page(9999));
        assert_eq!(page, enabled, "越界页索引必须被忽略");
        let _ = start;
    }
}
