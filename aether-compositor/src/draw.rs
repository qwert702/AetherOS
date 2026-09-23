//! Aether 桌面绘制逻辑 —— "Essence" 视觉语言。
//!
//! 设计原则（Apple 级质感，但不是复刻）：
//! - 中性深灰磨砂面板，绝不用高饱和大色块
//! - 发丝级白色描边（hairline）+ 大而柔的多层投影
//! - 连续圆角、克制的单一青色点缀、粗细分明的字重层级
//! - 壁纸是低对比的深色柔光渐变，负责衬托而非抢戏
//!
//! 纯软件光栅化，绘制函数将来同时服务于主机预览后端与 smithay 后端。

use crate::text::{draw_text, strings, TextRenderer};
use self::theme::{color, elevation, font, metric, radius, state};

/// "Essence" 设计令牌 —— 全系统视觉的单一事实来源（对应
/// docs/ui-design-plan.md §3：色板 / 字阶 / 圆角与阴影 / 交互态）。
///
/// 层级模型（从底到顶）：壁纸 → INSET（深底座：菜单栏/Dock/侧栏/终端）
/// → SURFACE_1（窗口）→ SURFACE_2（卡片、浮层）→ SURFACE_3（hover、输入框）。
pub mod theme {
    /// 色板 §3.1：中性层级 + 克制使用的强调色
    /// （极光只留给壁纸、AI 元素、品牌标识）
    pub mod color {
        // —— 中性层级（深空基调，明度阶梯拉开：相邻级差 15-18，杜绝"灰泥"）——
        /// 壁纸渐变顶（深空底色：提亮一档并保持蓝调，Step1 诊断 1）
        pub const BG_TOP: [u8; 3] = [22, 24, 34];
        /// 壁纸渐变底
        pub const BG_BOTTOM: [u8; 3] = [46, 52, 68];
        /// 凹陷区/深底座：菜单栏、Dock 托盘、侧栏、终端底、AI 指令条
        pub const INSET: [u8; 3] = [38, 40, 50];
        /// 窗口/面板主面
        pub const SURFACE_1: [u8; 3] = [54, 57, 68];
        /// 卡片、浮层、侧栏选中
        pub const SURFACE_2: [u8; 3] = [66, 70, 82];
        /// 悬停态、输入框
        pub const SURFACE_3: [u8; 3] = [80, 84, 97];
        /// 强悬停/强调层级（文件卡片 hover 等，Step1 新档）
        pub const SURFACE_4: [u8; 3] = [94, 98, 112];
        /// 发丝描边（玻璃感关键；alpha 在调用点按 6%–16% 使用）
        pub const HAIRLINE: [u8; 3] = [255, 255, 255];
        /// 主文字
        pub const TEXT: [u8; 3] = [240, 241, 246];
        /// 次要文字（提亮：与 TEXT 拉开但不失层级，Step1 诊断 4）
        pub const TEXT_DIM: [u8; 3] = [180, 182, 192];
        /// 占位/禁用文字（提亮到可读下限，Step1 诊断 4）
        pub const TEXT_FAINT: [u8; 3] = [136, 139, 150];

        // —— 强调色（克制使用）——
        /// 主青：选中、焦点、主按钮
        pub const ACCENT: [u8; 3] = [64, 190, 205];
        /// 辅紫：AI 相关元素
        pub const ACCENT_VIOLET: [u8; 3] = [150, 130, 220];
        /// 完成、在线
        pub const SUCCESS: [u8; 3] = [52, 199, 123];
        /// 预警、L2 确认（L2+ 权限确认弹窗，Step 4 消费）
        #[allow(dead_code)]
        pub const WARNING: [u8; 3] = [255, 179, 64];
        /// 危险操作、L3、错误
        pub const DANGER: [u8; 3] = [255, 92, 88];
        /// 信息提示（工具调用卡片等，Step 4 消费）
        #[allow(dead_code)]
        pub const INFO: [u8; 3] = [90, 160, 250];

        // —— 窗控三色（精致化红绿灯：直径 12px、间距 8px、悬停显示符号）——
        pub const CLOSE: [u8; 3] = [255, 95, 86];
        pub const MIN: [u8; 3] = [254, 188, 46];
        pub const ZOOM: [u8; 3] = [39, 201, 63];
    }

    /// 字阶 §3.2：5 档（+ 图标字形档）。11px 是可读下限，不得更小。
    pub mod font {
        /// 按钮文字、标签（下限）
        pub const LABEL: f32 = 11.0;
        /// 辅助说明、状态栏、键帽
        pub const CAPTION: f32 = 12.0;
        /// 终端、代码
        pub const MONO: f32 = 13.0;
        /// 正文、菜单项、窗口标题、按钮
        pub const BODY: f32 = 14.0;
        /// 弹窗标题、面板大标题
        pub const TITLE: f32 = 20.0;
        /// 图标内字形（Dock glyph、AI 徽标字母；非文字层级，不占字阶）
        pub const GLYPH: f32 = 16.0;
    }

    /// 圆角 §3.3：三档
    pub mod radius {
        /// 按钮、输入框、小控件
        pub const SM: f32 = 8.0;
        /// 卡片、面板
        pub const MD: f32 = 12.0;
        /// 窗口、弹窗
        pub const LG: f32 = 16.0;
    }

    /// 阴影档位 §3.3（值即影子总强度；shadow() 按档解释）
    pub mod elevation {
        /// 窗口浮起
        pub const ELEV_1: f32 = 0.25;
        /// 非活动窗口
        pub const ELEV_1_DIM: f32 = 0.14;
        /// 弹窗、浮层（下拉、AI 指令条、Toast、Dock）
        pub const ELEV_2: f32 = 0.4;
        /// 模态（安装向导、权限确认）
        pub const ELEV_3: f32 = 0.55;
    }

    /// 交互态 alpha（悬停/按下/禁用统一在此取值）
    pub mod state {
        /// 悬停高亮叠层
        pub const HOVER: f32 = 0.07;
        /// 悬停高亮叠层（选中项/强调）
        pub const HOVER_STRONG: f32 = 0.14;
        /// 按下压暗
        pub const PRESSED: f32 = 0.12;
        /// 禁用态整体不透明度
        pub const DISABLED: f32 = 0.4;
    }

    /// 布局尺寸（全局骨架常量收口于此；组件局部几何不进本模块）
    pub mod metric {
        /// 顶栏（菜单栏）高度；layout::TOP_BAR 是同一事实的布局视角别名
        pub const MENUBAR_H: i32 = 32;
        /// 窗口标题栏高度
        pub const TITLE_H: i32 = 36;
        /// 窗口标题栏命中区高度（可视高度 - 2，避免吃掉内容区边框）
        pub const TITLE_HIT_H: i32 = 34;
        /// Dock 图标边长
        pub const DOCK_ICON: i32 = 46;
        /// Dock 图标间距/托盘内边距
        pub const DOCK_PAD: i32 = 10;
        /// 窗口间距（§3.3 间距网格）
        pub const GAP: i32 = 14;
        /// 底部 Dock 保留区高度（托盘 + AI 指令条以下留白）
        pub const BOTTOM_DOCK: i32 = 104;
        /// 窗控圆点直径与间距（§3.1：12px / 8px）
        pub const LIGHT_D: i32 = 12;
        pub const LIGHT_GAP: i32 = 8;
    }
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub fn blend_pixel(buf: &mut [u32], idx: usize, rgb: [u8; 3], alpha: f32) {
    if alpha <= 0.0 || idx >= buf.len() {
        return;
    }
    let dst = buf[idx];
    let dr = (dst >> 16) as f32;
    let dg = ((dst >> 8) & 0xff) as f32;
    let db = (dst & 0xff) as f32;
    let r = lerp(dr, rgb[0] as f32, alpha).clamp(0.0, 255.0) as u32;
    let g = lerp(dg, rgb[1] as f32, alpha).clamp(0.0, 255.0) as u32;
    let b = lerp(db, rgb[2] as f32, alpha).clamp(0.0, 255.0) as u32;
    buf[idx] = (r << 16) | (g << 8) | b;
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && x < (self.x + self.w) as f32
            && y >= self.y as f32
            && y < (self.y + self.h) as f32
    }
}

pub fn fill_rect(buf: &mut [u32], w: usize, h: usize, rect: Rect, rgb: [u8; 3], alpha: f32) {
    // 负宽/高是空操作：`as usize` 会把负值回卷成巨值再被钳成整屏填充
    if rect.w <= 0 || rect.h <= 0 {
        return;
    }
    let x0 = rect.x.max(0) as usize;
    let y0 = rect.y.max(0) as usize;
    let x1 = ((rect.x + rect.w) as usize).min(w);
    let y1 = ((rect.y + rect.h) as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(buf, y * w + x, rgb, alpha);
        }
    }
}

/// 抗锯齿圆角矩形（符号距离场覆盖率）。
pub fn rounded_rect(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
    let radius = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    let x0 = (r.x as f32 - 1.0).max(0.0) as usize;
    let y0 = (r.y as f32 - 1.0).max(0.0) as usize;
    let x1 = ((r.x + r.w) as f32 + 1.0).min(w as f32) as usize;
    let y1 = ((r.y + r.h) as f32 + 1.0).min(h as f32) as usize;
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let qx_half = r.w as f32 / 2.0 - radius;
    let qy_half = r.h as f32 / 2.0 - radius;
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = (x as f32 + 0.5 - cx).abs() - qx_half;
            let dy = (y as f32 + 0.5 - cy).abs() - qy_half;
            let outside = dx.max(0.0).hypot(dy.max(0.0)) + (dx.max(dy)).min(0.0);
            let d = outside - radius;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, y * w + x, rgb, alpha * cov);
            }
        }
    }
}

/// 抗锯齿圆角轮廓线（约 2px 描边带）。
pub fn rounded_outline(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
    let x0 = (r.x - 2).max(0) as usize;
    let y0 = (r.y - 2).max(0) as usize;
    let x1 = ((r.x + r.w) as usize + 2).min(w);
    let y1 = ((r.y + r.h) as usize + 2).min(h);
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let qx_half = r.w as f32 / 2.0 - radius;
    let qy_half = r.h as f32 / 2.0 - radius;
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = (x as f32 + 0.5 - cx).abs() - qx_half;
            let dy = (y as f32 + 0.5 - cy).abs() - qy_half;
            let outside = dx.max(0.0).hypot(dy.max(0.0)) + (dx.max(dy)).min(0.0);
            let d = outside - radius;
            // 只保留边框带 [-1.5, 0.5)
            if d < 0.5 && d > -1.5 {
                let cov = (0.5 - d).clamp(0.0, 1.0);
                blend_pixel(buf, y * w + x, rgb, alpha * cov);
            }
        }
    }
}

/// 水平渐变圆角轮廓（焦点环用：左青 → 右紫，AI 元素的极光出口）。
pub fn gradient_outline(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, left: [u8; 3], right: [u8; 3], alpha: f32) {
    let x0 = (r.x - 2).max(0) as usize;
    let y0 = (r.y - 2).max(0) as usize;
    let x1 = ((r.x + r.w) as usize + 2).min(w);
    let y1 = ((r.y + r.h) as usize + 2).min(h);
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let qx_half = r.w as f32 / 2.0 - radius;
    let qy_half = r.h as f32 / 2.0 - radius;
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = (x as f32 + 0.5 - cx).abs() - qx_half;
            let dy = (y as f32 + 0.5 - cy).abs() - qy_half;
            let outside = dx.max(0.0).hypot(dy.max(0.0)) + (dx.max(dy)).min(0.0);
            let d = outside - radius;
            if d < 0.5 && d > -1.5 {
                let cov = (0.5 - d).clamp(0.0, 1.0);
                let t = ((x as f32 - r.x as f32) / r.w.max(1) as f32).clamp(0.0, 1.0);
                let rgb = [
                    lerp(left[0] as f32, right[0] as f32, t) as u8,
                    lerp(left[1] as f32, right[1] as f32, t) as u8,
                    lerp(left[2] as f32, right[2] as f32, t) as u8,
                ];
                blend_pixel(buf, y * w + x, rgb, alpha * cov);
            }
        }
    }
}

/// 大而柔的多层投影（近似大半径高斯）。
/// 层数与半径是性能关键：每层都是一次全区域 SDF 扫描。
/// `strength` 取 theme::elevation 的档位值（值即影子总强度）。
pub fn shadow(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, strength: f32) {
    for i in 0..5 {
        let grow = i as f32 * 5.0;
        let s = Rect {
            x: r.x - grow as i32,
            y: r.y - grow as i32 + 6,
            w: r.w + grow as i32 * 2,
            h: r.h + grow as i32 * 2,
        };
        rounded_rect(buf, w, h, s, radius + grow, [0, 0, 0], strength * (1.0 - i as f32 / 5.0) * 0.80);
    }
}

/// 垂直渐变的圆角方块（Dock 图标底座）：逐像素 SDF + 行渐变色。
fn gradient_tile(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, top: [u8; 3], bottom: [u8; 3], alpha: f32) {
    let _ = h; // 深度用不到屏幕高，仅保留签名一致
    let radius = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let qx_half = r.w as f32 / 2.0 - radius;
    let qy_half = r.h as f32 / 2.0 - radius;
    for y in 0..r.h {
        let t = y as f32 / r.h.max(1) as f32;
        let rgb = [
            lerp(top[0] as f32, bottom[0] as f32, t) as u8,
            lerp(top[1] as f32, bottom[1] as f32, t) as u8,
            lerp(top[2] as f32, bottom[2] as f32, t) as u8,
        ];
        for x in 0..r.w {
            let dx = (r.x + x) as f32 + 0.5 - cx;
            let dy = (r.y + y) as f32 + 0.5 - cy;
            let qx = dx.abs() - qx_half;
            let qy = dy.abs() - qy_half;
            let outside = qx.max(0.0).hypot(qy.max(0.0)) + (qx.max(qy)).min(0.0);
            let d = outside - radius;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, (r.y + y) as usize * w + (r.x + x) as usize, rgb, alpha * cov);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 桌面状态
// ---------------------------------------------------------------------------

/// 桌面状态：窗口列表（含 z 序）与当前布局。
pub struct Desktop {
    pub wins: Vec<Win>,
    pub active: usize,
    pub layout: crate::layout::Layout,
}

pub struct Win {
    pub rect: Rect,
    /// 动画目标；每帧向它缓动，到达后清除
    pub target: Option<Rect>,
    pub title: &'static str,
    pub floating: bool,
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

    /// 状态色：青 / 紫 / 灰
    pub fn color(self) -> [u8; 3] {
        match self {
            AiStatus::Local => color::ACCENT,
            AiStatus::Cloud => color::ACCENT_VIOLET,
            AiStatus::Offline => color::TEXT_FAINT,
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
        if self.level >= 3 { color::DANGER } else { color::WARNING }
    }
}

/// 每帧的 UI 瞬态（由 main.rs 组装）。
pub struct UiState<'a> {
    pub snap: Option<Rect>,
    pub toast: Option<(&'a str, f32)>,
    pub ai_input: &'a str,
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

/// 帧渲染器：持有跨帧缓存（背景层等）与可点击区域登记（供命中测试）。
pub struct Renderer {
    bg: Vec<u32>,
    frames_since_bg: u32,
    bg_w: usize,
    bg_h: usize,
    /// 菜单栏各菜单标签的命中区（每帧更新）
    pub menubar_menus: Vec<Rect>,
    /// 搜索胶囊命中区
    pub search_pill: Rect,
    /// Dock 图标命中区
    pub dock_icons: Vec<Rect>,
    /// 当前展开的下拉菜单：(各项命中区, 文案)
    pub dropdown: Option<(Vec<Rect>, Vec<&'static str>)>,
    /// 安装向导磁盘行的命中区 (矩形, 设备名, 容量MB)
    pub installer_rows: Vec<(Rect, String, u64)>,
    /// "开始安装"按钮命中区
    pub installer_button: Rect,
    /// 权限确认弹窗的按钮命中区
    pub confirm_buttons: Vec<(Rect, ConfirmButton)>,
    /// 权限确认弹窗的回显输入框命中区
    pub confirm_echo: Rect,
}

impl Renderer {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            bg: vec![0; w * h],
            frames_since_bg: u32::MAX,
            bg_w: w,
            bg_h: h,
            menubar_menus: Vec::new(),
            search_pill: Rect { x: 0, y: 0, w: 0, h: 0 },
            dock_icons: Vec::new(),
            dropdown: None,
            installer_rows: Vec::new(),
            installer_button: Rect { x: 0, y: 0, w: 0, h: 0 },
            confirm_buttons: Vec::new(),
            confirm_echo: Rect { x: 0, y: 0, w: 0, h: 0 },
        }
    }

    /// 渲染一帧。背景每 BG_REFRESH_FRAMES 帧才重算一次（柔光漂移是
    /// 4 秒级的变化，逐帧重算纯属浪费），其余帧只是一次内存拷贝。
    pub fn render_frame(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        t: f32,
        desktop: &Desktop,
        ui: &UiState,
        tr: Option<&TextRenderer>,
    ) {
        const BG_REFRESH_FRAMES: u32 = 240;
        if self.frames_since_bg > BG_REFRESH_FRAMES || self.bg_w != w || self.bg_h != h {
            if self.bg.len() != w * h {
                self.bg = vec![0; w * h];
            }
            draw_background(&mut self.bg, w, h, t);
            self.frames_since_bg = 0;
            self.bg_w = w;
            self.bg_h = h;
        }
        self.frames_since_bg += 1;
        buf.copy_from_slice(&self.bg);

        if let Some(z) = ui.snap {
            rounded_rect(buf, w, h, z, radius::LG, color::ACCENT, 0.08);
            rounded_outline(buf, w, h, z, radius::LG, color::ACCENT, 0.5);
        }

        for (i, win) in desktop.wins.iter().enumerate() {
            draw_window(buf, w, h, win.rect, win.title, i == desktop.active, ui.mouse, t, tr);
        }

        self.draw_menubar(buf, w, h, ui, tr);
        if ui.open_menu.is_some() {
            self.draw_dropdown(buf, w, h, ui, tr);
        }
        if let Some((reply, kind, age)) = ui.ai_reply {
            draw_reply(buf, w, h, reply, kind, age, tr);
        }
        self.draw_ai_bar(buf, w, h, ui, t, tr);

        let open_titles: Vec<&str> = desktop.wins.iter().map(|x| x.title).collect();
        self.draw_dock(buf, w, h, &open_titles, ui, tr);

        // 安装向导浮在最上层（Toast 之下）
        if let Some(inst) = &ui.installer {
            self.draw_installer(buf, w, h, inst, ui.mouse, ui.mouse_down, tr);
        }

        // 权限确认是模态：盖在安装向导之上（安装向导的"开始安装"也会走它）
        match &ui.confirm {
            Some(c) => self.draw_confirm(buf, w, h, c, ui.mouse, ui.mouse_down, t, tr),
            None => {
                self.confirm_buttons.clear();
                self.confirm_echo = Rect { x: 0, y: 0, w: 0, h: 0 };
            }
        }

        if let Some((msg, age)) = ui.toast {
            draw_toast(buf, w, h, msg, age, tr);
        }
    }
}

// ---------------------------------------------------------------------------
// 壁纸：低对比柔光渐变
// ---------------------------------------------------------------------------

fn wash(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let dx = (x - cx) / rx;
    let dy = (y - cy) / ry;
    (1.0 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0).powi(2)
}

fn draw_background(buf: &mut [u32], w: usize, h: usize, t: f32) {
    // 漂移放缓（§3.4 动效克制）：4 秒级的变化降到分钟级呼吸
    let drift = t * 0.02;
    // 三道极光柔光：青（左上）、紫（右下）、暖（右上）。
    // Step1 诊断 1：强度/饱和度提升到"肉眼明确可辨但克制"——
    // 中心权重 0.44/0.40 使可见条带出现真实青/紫色相，暗角 0.14 不再闷死。
    let washes: [(f32, f32, f32, f32, [u8; 3], f32); 3] = [
        (
            w as f32 * (0.20 + 0.02 * drift.sin()),
            h as f32 * 0.15,
            w as f32 * 0.58,
            h as f32 * 0.64,
            [66, 178, 186],
            0.44,
        ),
        (
            w as f32 * (0.80 - 0.02 * drift.cos()),
            h as f32 * 0.82,
            w as f32 * 0.64,
            h as f32 * 0.58,
            [148, 128, 214],
            0.45,
        ),
        (
            w as f32 * 0.88,
            h as f32 * 0.12,
            w as f32 * 0.42,
            h as f32 * 0.46,
            [168, 118, 110],
            0.15,
        ),
    ];

    for y in 0..h {
        let vgrad = y as f32 / h as f32;
        for x in 0..w {
            let mut acc = [0f32; 3];
            for c in 0..3 {
                acc[c] = lerp(color::BG_TOP[c] as f32, color::BG_BOTTOM[c] as f32, vgrad);
            }
            for (cx, cy, rx, ry, rgb, s) in &washes {
                let g = wash(x as f32, y as f32, *cx, *cy, *rx, *ry) * s;
                for c in 0..3 {
                    acc[c] += (rgb[c] as f32 - acc[c]) * g;
                }
            }
            // 轻微暗角（从 0.30 降到 0.14：留深空氛围，不再闷死极光）
            let nx = x as f32 / w as f32 - 0.5;
            let ny = y as f32 / h as f32 - 0.5;
            let vig = 1.0 - (nx * nx + ny * ny) * 0.14;
            buf[y * w + x] = ((acc[0] * vig) as u32) << 16
                | ((acc[1] * vig) as u32) << 8
                | (acc[2] * vig) as u32;
        }
    }
}

// ---------------------------------------------------------------------------
// 菜单栏：发丝底 + 品牌 + 菜单（可点击）+ 搜索胶囊 + 电池 + 时钟
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_menubar(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let bar = Rect { x: 0, y: 0, w: w as i32, h: metric::MENUBAR_H };
        fill_rect(buf, w, h, bar, color::INSET, 0.62);
        fill_rect(buf, w, h, Rect { x: 0, y: metric::MENUBAR_H - 1, w: w as i32, h: 1 }, color::HAIRLINE, 0.10);

        let Some(tr) = tr else { return };

        // 品牌：三角徽标（青→紫极光渐变，品牌标识是极光的三个容许出口之一）
        let (bx, by) = (12i32, 8i32);
        for row in 0..16 {
            let half = row / 2;
            let t = row as f32 / 15.0;
            let rgb = [
                lerp(color::ACCENT[0] as f32, color::ACCENT_VIOLET[0] as f32, t) as u8,
                lerp(color::ACCENT[1] as f32, color::ACCENT_VIOLET[1] as f32, t) as u8,
                lerp(color::ACCENT[2] as f32, color::ACCENT_VIOLET[2] as f32, t) as u8,
            ];
            for col in 0..(half + 1) {
                let px = bx + half - col;
                let py = by + 15 - row;
                if px >= 0 && py >= 0 {
                    blend_pixel(buf, py as usize * w + px as usize, rgb, 0.95);
                }
            }
        }
        let brand_x = 32.0;
        tr.draw_bold(buf, w, h, brand_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), strings::BRAND, font::BODY, color::TEXT, 0.98);
        let mut mx = brand_x + tr.measure_bold(strings::BRAND, font::BODY) + 18.0;

        // 菜单标签（登记命中区；悬停/打开用交互态令牌）
        self.menubar_menus.clear();
        for (i, menu) in strings::MENUS.iter().enumerate() {
            let mw = tr.measure(menu, font::BODY);
            let hit = Rect { x: mx as i32 - 8, y: 0, w: mw as i32 + 16, h: metric::MENUBAR_H };
            let hovered = hit.contains(ui.mouse.0, ui.mouse.1);
            let opened = ui.open_menu == Some(i);
            if opened || hovered {
                let a = if opened { state::HOVER_STRONG } else { state::HOVER };
                rounded_rect(buf, w, h, Rect { x: hit.x + 2, y: 4, w: hit.w - 4, h: metric::MENUBAR_H - 8 }, radius::SM, color::HAIRLINE, a);
            }
            draw_text(tr, buf, w, h, mx, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), menu, font::BODY, color::TEXT, if opened { 1.0 } else { 0.88 });
            self.menubar_menus.push(hit);
            mx += mw + 16.0;
        }

        // 右侧：电池、AI 状态、搜索胶囊、时钟
        let clock = crate::text::clock_str();
        let clock_w = tr.measure_bold(&clock, font::BODY);
        let clock_x = w as f32 - 16.0 - clock_w;
        tr.draw_bold(buf, w, h, clock_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), &clock, font::BODY, color::TEXT, 0.95);

        let pill = Rect { x: clock_x as i32 - 208, y: 5, w: 192, h: 22 };
        let pill_hover = pill.contains(ui.mouse.0, ui.mouse.1);
        rounded_rect(buf, w, h, pill, 11.0, color::HAIRLINE, if pill_hover { state::HOVER_STRONG } else { 0.08 });
        rounded_outline(buf, w, h, pill, 11.0, color::HAIRLINE, if pill_hover { 0.22 } else { 0.14 });
        draw_text(tr, buf, w, h, (pill.x + 12) as f32, tr.vcenter(pill.y as f32, pill.h as f32, font::CAPTION), "搜索", font::CAPTION, color::TEXT_DIM, if pill_hover { 0.95 } else { 0.8 });
        let key = Rect { x: pill.x + pill.w - 22, y: 8, w: 16, h: 16 };
        rounded_rect(buf, w, h, key, 4.0, color::HAIRLINE, 0.12);
        draw_text(tr, buf, w, h, (key.x + 4) as f32, tr.vcenter(key.y as f32, key.h as f32, font::LABEL), "K", font::LABEL, color::TEXT_DIM, 0.85);
        self.search_pill = pill;

        // AI 状态指示（§4 三态：本地青 / 云端紫 / 离线灰）
        let ai_label = ui.ai_status.label();
        let ai_w = tr.measure(ai_label, font::CAPTION);
        let ai_x = pill.x as f32 - 16.0 - ai_w;
        rounded_rect(buf, w, h, Rect { x: ai_x as i32 - 12, y: 13, w: 6, h: 6 }, 3.0, ui.ai_status.color(), 0.95);
        draw_text(tr, buf, w, h, ai_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::CAPTION), ai_label, font::CAPTION, color::TEXT_DIM, 0.85);

        // 电池：状态用中性色（在线/电量语义留给文字，避免与强调色抢注意力）
        let batt = Rect { x: ai_x as i32 - 56, y: 10, w: 26, h: 12 };
        rounded_outline(buf, w, h, batt, 3.5, color::TEXT_DIM, 0.5);
        fill_rect(buf, w, h, Rect { x: batt.x + batt.w, y: 13, w: 2, h: 6 }, color::TEXT_DIM, 0.5);
        fill_rect(buf, w, h, Rect { x: batt.x + 2, y: batt.y + 2, w: 16, h: 8 }, color::TEXT, 0.5);
    }

    /// 展开中的下拉菜单（登记各项命中区，悬停高亮）。
    fn draw_dropdown(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let Some(mi) = ui.open_menu else { return };
        let Some(label) = self.menubar_menus.get(mi) else { return };
        let Some(tr) = tr else { return };
        let items = strings::MENU_ITEMS[mi];
        let panel_w = items
            .iter()
            .map(|s| tr.measure(s, font::BODY))
            .fold(120.0f32, f32::max)
            + 44.0;
        let panel = Rect { x: label.x, y: metric::MENUBAR_H + 4, w: panel_w as i32, h: items.len() as i32 * 30 + 8 };
        shadow(buf, w, h, panel, radius::MD, elevation::ELEV_2);
        rounded_rect(buf, w, h, panel, radius::MD, color::SURFACE_2, 0.98);
        rounded_outline(buf, w, h, panel, radius::MD, color::HAIRLINE, 0.12);

        let mut rects = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let ir = Rect { x: panel.x + 4, y: panel.y + 4 + i as i32 * 30, w: panel.w - 8, h: 30 };
            if ir.contains(ui.mouse.0, ui.mouse.1) {
                rounded_rect(buf, w, h, Rect { x: ir.x + 4, y: ir.y + 3, w: ir.w - 8, h: ir.h - 6 }, radius::SM, color::SURFACE_3, 0.9);
            }
            draw_text(tr, buf, w, h, (ir.x + 16) as f32, tr.vcenter(ir.y as f32, ir.h as f32, font::BODY), item, font::BODY, color::TEXT, 0.92);
            rects.push(ir);
        }
        self.dropdown = Some((rects, items.to_vec()));
    }
}

// ---------------------------------------------------------------------------
// 窗口：磨砂深灰 + 发丝描边 + 红绿灯（左）+ 居中标题
// ---------------------------------------------------------------------------

fn draw_window(buf: &mut [u32], w: usize, h: usize, r: Rect, title: &str, active: bool, mouse: (f32, f32), t: f32, tr: Option<&TextRenderer>) {
    shadow(buf, w, h, r, radius::LG, if active { elevation::ELEV_1 } else { elevation::ELEV_1_DIM });
    rounded_rect(buf, w, h, r, radius::LG, color::SURFACE_1, 0.92);
    // 顶部内高光（玻璃厚度感）
    fill_rect(buf, w, h, Rect { x: r.x + 8, y: r.y + 1, w: r.w - 16, h: 1 }, color::HAIRLINE, 0.07);
    rounded_outline(buf, w, h, r, radius::LG, color::HAIRLINE, if active { 0.16 } else { 0.09 });

    // 标题栏分隔发丝线
    fill_rect(buf, w, h, Rect { x: r.x + 1, y: r.y + metric::TITLE_H, w: r.w - 2, h: 1 }, color::HAIRLINE, 0.07);

    // 红绿灯（左）：直径 12px、间距 8px（§3.1），悬停时整组显示符号
    let lights = [color::CLOSE, color::MIN, color::ZOOM];
    let ly = r.y + (metric::TITLE_H - metric::LIGHT_D) / 2;
    let group = Rect {
        x: r.x + 8,
        y: r.y,
        w: metric::LIGHT_D * 3 + metric::LIGHT_GAP * 2 + 12,
        h: metric::TITLE_H,
    };
    let hovered = active && group.contains(mouse.0, mouse.1);
    for (i, c) in lights.iter().enumerate() {
        let lx = r.x + 14 + (i as i32) * (metric::LIGHT_D + metric::LIGHT_GAP);
        let dot = Rect { x: lx, y: ly, w: metric::LIGHT_D, h: metric::LIGHT_D };
        rounded_rect(buf, w, h, dot, metric::LIGHT_D as f32 / 2.0, *c, if active { 0.95 } else { 0.45 });
        if hovered {
            light_symbol(buf, w, h, lx + metric::LIGHT_D / 2, ly + metric::LIGHT_D / 2, i);
        }
    }

    // 居中标题（粗体、层级分明；按字号在标题栏内垂直居中）
    if let Some(tr) = tr {
        let tw = tr.measure_bold(title, font::BODY);
        let tx = r.x as f32 + r.w as f32 / 2.0 - tw / 2.0;
        let ty = tr.vcenter(r.y as f32, metric::TITLE_H as f32, font::BODY);
        tr.draw_bold(buf, w, h, tx, ty, title, font::BODY, color::TEXT, if active { 0.9 } else { 0.4 });
    }

    let content = Rect { x: r.x + 1, y: r.y + metric::TITLE_H + 1, w: r.w - 2, h: r.h - metric::TITLE_H - 2 };
    if title == strings::WIN_TERM {
        draw_term_content(buf, w, h, content, t, tr);
    } else {
        draw_files_content(buf, w, h, content, mouse, tr);
    }
}

/// 窗控符号（悬停时显示）：纯几何绘制，不依赖字体字形覆盖。
fn light_symbol(buf: &mut [u32], w: usize, h: usize, cx: i32, cy: i32, kind: usize) {
    const S: i32 = 3; // 半臂长 → 符号跨度 7px（12px 圆内）
    // 亮色圆点上用近黑符号：对比足，又不抢红绿灯本身的颜色语义
    let rgb = [26, 27, 32];
    let alpha = 0.75;
    match kind {
        // 关闭：×
        0 => {
            for i in -S..=S {
                fill_rect(buf, w, h, Rect { x: cx + i, y: cy + i, w: 1, h: 1 }, rgb, alpha);
                fill_rect(buf, w, h, Rect { x: cx + i, y: cy - i, w: 1, h: 1 }, rgb, alpha);
            }
        }
        // 最小化：—
        1 => fill_rect(buf, w, h, Rect { x: cx - S, y: cy, w: 2 * S + 1, h: 1 }, rgb, alpha),
        // 最大化：＋（软件光栅下加号比双向箭头清晰）
        _ => {
            fill_rect(buf, w, h, Rect { x: cx - S, y: cy, w: 2 * S + 1, h: 1 }, rgb, alpha);
            fill_rect(buf, w, h, Rect { x: cx, y: cy - S, w: 1, h: 2 * S + 1 }, rgb, alpha);
        }
    }
}

fn draw_term_content(buf: &mut [u32], w: usize, h: usize, r: Rect, t: f32, tr: Option<&TextRenderer>) {
    fill_rect(buf, w, h, r, color::INSET, 0.92);
    let Some(tr) = tr else { return };
    let lines = [
        ("aether@localhost ~ $", color::ACCENT, 0.95),
        ("uname -a", color::TEXT, 0.9),
        ("AetherOS 0.1.0 aether-kernel x86_64", color::TEXT_DIM, 0.85),
        ("aether@localhost ~ $", color::ACCENT, 0.95),
    ];
    let mut y = r.y + 12;
    for (line, rgb, a) in lines {
        draw_text(tr, buf, w, h, (r.x + 14) as f32, y as f32, line, font::MONO, rgb, a);
        y += 22;
    }
    if (t * 2.0) as i32 % 2 == 0 {
        fill_rect(buf, w, h, Rect { x: r.x + 14, y: y + 2, w: 8, h: 14 }, color::TEXT, 0.7);
    }
}

fn draw_files_content(buf: &mut [u32], w: usize, h: usize, r: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>) {
    // 侧栏
    let sidebar = Rect { x: r.x, y: r.y, w: 138, h: r.h };
    fill_rect(buf, w, h, sidebar, color::INSET, 0.45);
    fill_rect(buf, w, h, Rect { x: sidebar.x + sidebar.w, y: r.y, w: 1, h: r.h }, color::HAIRLINE, 0.06);
    if let Some(tr) = tr {
        for (i, item) in ["文档", "图片", "音乐", "项目"].iter().enumerate() {
            let y = r.y + 14 + i as i32 * 34;
            let selected = i == 0;
            let row = Rect { x: r.x + 8, y: y - 5, w: 122, h: 28 };
            if selected {
                rounded_rect(buf, w, h, row, radius::SM, color::ACCENT, 0.18);
            } else if row.contains(mouse.0, mouse.1) {
                rounded_rect(buf, w, h, row, radius::SM, color::SURFACE_1, 0.7);
            }
            draw_text(tr, buf, w, h, (r.x + 20) as f32, tr.vcenter((y - 5) as f32, 28.0, font::BODY), item, font::BODY, if selected { color::TEXT } else { color::TEXT_DIM }, if selected { 0.95 } else { 0.75 });
        }
    }
    // 文件卡片网格（悬停提亮一档：SURFACE_2 → SURFACE_3）
    let grid_x = r.x + 152;
    for row in 0..2 {
        for col in 0..4 {
            let card = Rect { x: grid_x + col * 98, y: r.y + 14 + row * 98, w: 86, h: 86 };
            if card.w <= 0 || card.x + card.w > r.x + r.w {
                continue;
            }
            let hovered = card.contains(mouse.0, mouse.1);
            rounded_rect(buf, w, h, card, radius::MD, if hovered { color::SURFACE_4 } else { color::SURFACE_2 }, if hovered { 0.9 } else { 0.62 });
            rounded_outline(buf, w, h, card, radius::MD, color::HAIRLINE, if hovered { 0.16 } else { 0.08 });
            // 缩略图占位：与 Dock 图标同语言的低饱和渐变底 + 白色文件夹符号
            //（Step2：不再用实心蓝灰块，消灭占位廉价感）
            gradient_tile(buf, w, h, Rect { x: card.x + 21, y: card.y + 14, w: 44, h: 34 }, 7.0, [52, 104, 118], [72, 158, 168], 0.9);
            rounded_rect(buf, w, h, Rect { x: card.x + 29, y: card.y + 17, w: 9, h: 4 }, 2.0, color::TEXT, 0.95);
            rounded_rect(buf, w, h, Rect { x: card.x + 30, y: card.y + 21, w: 26, h: 17 }, 3.0, color::TEXT, 0.95);
        }
    }
}

// ---------------------------------------------------------------------------
// AI 指令条：悬浮胶囊（Spotlight 气质），可真实输入
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_ai_bar(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, t: f32, tr: Option<&TextRenderer>) {
    let input = ui.ai_input;
    let mouse = ui.mouse;
    let bar_w = 560i32;
    let bar = Rect { x: w as i32 / 2 - bar_w / 2, y: h as i32 - metric::BOTTOM_DOCK - 52 - 16, w: bar_w, h: 52 };
    let hovered = bar.contains(mouse.0, mouse.1);
    shadow(buf, w, h, bar, radius::MD, elevation::ELEV_2);
    rounded_rect(buf, w, h, bar, 26.0, color::INSET, if hovered { 0.82 } else { 0.75 });
    if ui.ai_focused {
        // 焦点态：青紫渐变环（AI 元素的极光配额）；思考中叠一层呼吸脉动
        let breath = if ui.ai_thinking { 0.55 + 0.45 * (t * 3.0).sin() } else { 1.0 };
        gradient_outline(buf, w, h, bar, 26.0, color::ACCENT, color::ACCENT_VIOLET, 0.75 * breath);
    } else {
        rounded_outline(buf, w, h, bar, 26.0, color::HAIRLINE, if hovered { 0.22 } else { 0.14 });
    }

    let Some(tr) = tr else { return };

    // 渐变圆形徽标（AI 元素：极光的容许出口之一）
    let av = Rect { x: bar.x + 12, y: bar.y + 10, w: 32, h: 32 };
    gradient_tile(buf, w, h, av, 16.0, color::ACCENT, color::ACCENT_VIOLET, 0.95);
    let aw = tr.measure_bold("A", font::GLYPH);
    tr.draw_bold(buf, w, h, av.x as f32 + (av.w as f32 - aw) / 2.0, tr.vcenter(av.y as f32, av.h as f32, font::GLYPH), "A", font::GLYPH, color::TEXT, 0.98);

    let text_x = (bar.x + 58) as f32;
    let text_y = tr.vcenter(bar.y as f32, bar.h as f32, font::BODY);
    if input.is_empty() {
        draw_text(tr, buf, w, h, text_x, text_y, strings::AI_BAR_HINT, font::BODY, color::TEXT_DIM, 0.85);
    } else {
        // 从左截断，保证光标所在的内容始终可见
        let max_w = bar.x as f32 + bar.w as f32 - 96.0 - text_x;
        let mut shown: String = input.to_string();
        while tr.measure(&shown, font::BODY) > max_w && shown.chars().count() > 1 {
            shown.remove(0);
        }
        draw_text(tr, buf, w, h, text_x, text_y, &shown, font::BODY, color::TEXT, 0.95);
    }

    // 输入光标
    let caret_x = text_x + tr.measure(if input.is_empty() { "" } else { input_tail_visible(tr, input, text_x, bar.x as f32 + bar.w as f32 - 96.0) }, font::BODY) + 4.0;
    if ui.ai_thinking {
        // 思考中：三点呼吸，明确"在等我"而不是"在等你打字"
        for i in 0..3 {
            let phase = (t * 3.0 - i as f32 * 0.5).sin() * 0.5 + 0.5;
            let a = 0.25 + 0.6 * phase;
            let cx = caret_x as i32 + i * 8;
            rounded_rect(buf, w, h, Rect { x: cx, y: bar.y + 23, w: 5, h: 5 }, 2.5, color::ACCENT, a);
        }
    } else if ((t_now() * 2.0) as i32) % 2 == 0 {
        fill_rect(buf, w, h, Rect { x: caret_x as i32, y: bar.y + 16, w: 2, h: 20 }, color::ACCENT, 0.9);
    }

    // 右侧快捷键胶囊
    let hint = "Enter";
    let hw = tr.measure(hint, font::LABEL) + 12.0;
    let key = Rect { x: bar.x + bar.w - hw as i32 - 12, y: bar.y + 15, w: hw as i32, h: 22 };
    rounded_rect(buf, w, h, key, 6.0, color::HAIRLINE, 0.10);
    draw_text(tr, buf, w, h, (key.x + 6) as f32, tr.vcenter(key.y as f32, key.h as f32, font::LABEL), hint, font::LABEL, color::TEXT_DIM, 0.8);
    }
}

fn input_tail_visible<'a>(tr: &TextRenderer, input: &'a str, x: f32, max_right: f32) -> &'a str {
    let mut s = input;
    while tr.measure(s, font::BODY) > max_right - x {
        match s.char_indices().nth(1) {
            Some((byte_idx, _)) => s = &s[byte_idx..],
            None => break,
        }
    }
    s
}

fn t_now() -> f32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() as f32 / 1000.0)
        .unwrap_or(0.0)
}

/// 回复气泡：出现在指令条上方。三类样式一眼可分——
/// 用户指令（中性底、偏右）/ AI 回复（青紫环）/ 工具调用（左侧状态条）。
fn draw_reply(buf: &mut [u32], w: usize, h: usize, text: &str, kind: BubbleKind, age: f32, tr: Option<&TextRenderer>) {
    let Some(tr) = tr else { return };
    let alpha = ((6.0 - age) / 0.4).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let max_w = 720.0f32;
    let pad = if kind == BubbleKind::Tool { 30.0 } else { 18.0 };
    let tw = tr.measure(text, font::BODY).min(max_w);
    let bar_w = (tw + 2.0 * pad) as i32;
    let y = h as i32 - metric::BOTTOM_DOCK - 52 - 16 - 56;
    // 用户指令靠右、AI 与工具结果靠左：来源方向即身份
    let x = match kind {
        BubbleKind::User => w as i32 / 2 + 60 - bar_w,
        _ => w as i32 / 2 - 60,
    };
    let r = Rect { x, y, w: bar_w, h: 36 };
    shadow(buf, w, h, r, radius::MD, alpha * elevation::ELEV_2);
    let bg = match kind {
        BubbleKind::User => color::SURFACE_3,
        _ => color::SURFACE_2,
    };
    rounded_rect(buf, w, h, r, 18.0, bg, alpha * 0.94);
    match kind {
        // AI 回复：青紫渐变环（AI 元素）
        BubbleKind::Ai => gradient_outline(buf, w, h, r, 18.0, color::ACCENT, color::ACCENT_VIOLET, alpha * 0.55),
        BubbleKind::User => rounded_outline(buf, w, h, r, 18.0, color::HAIRLINE, alpha * 0.16),
        // 工具调用：左侧状态条（成功绿/失败红），由文案前缀决定
        BubbleKind::Tool => {
            rounded_outline(buf, w, h, r, 18.0, color::HAIRLINE, alpha * 0.12);
            let ok = !text.starts_with('✗');
            let bar_rgb = if ok { color::SUCCESS } else { color::DANGER };
            rounded_rect(buf, w, h, Rect { x: r.x + 10, y: r.y + 9, w: 4, h: r.h - 18 }, 2.0, bar_rgb, alpha * 0.95);
        }
    }
    // 左截断显示
    let mut shown: String = text.to_string();
    while tr.measure(&shown, font::BODY) > max_w && shown.chars().count() > 1 {
        shown.remove(0);
    }
    let tx = match kind {
        BubbleKind::User => tr.measure(&shown, font::BODY) + pad,
        BubbleKind::Tool => tr.measure(&shown, font::BODY) + pad + 6.0,
        BubbleKind::Ai => pad,
    };
    draw_text(tr, buf, w, h, r.x as f32 + tx, tr.vcenter(r.y as f32, r.h as f32, font::BODY), &shown, font::BODY, color::TEXT, alpha);
}

// ---------------------------------------------------------------------------
// Dock：磨砂托盘 + 渐变图标 + 运行指示点
// ---------------------------------------------------------------------------

impl Renderer {
    /// Dock：磨砂托盘 + 中性玻璃图标 + 运行指示点（真实反映已打开窗口）。
    /// Live ISO 会话（ui.show_installer）在末尾附加"安装"图标。
    ///
    /// 图标底座走中性灰阶（§1：极光只留给壁纸、AI 元素、品牌标识），
    /// 区分度由造型与状态（hover 提亮、运行中青色微光）承担，不再用彩色拟物色块。
    fn draw_dock(&mut self, buf: &mut [u32], w: usize, h: usize, open_titles: &[&str], ui: &UiState, _tr: Option<&TextRenderer>) {
        let mut apps: Vec<&str> = vec![
            strings::WIN_FILES,
            strings::WIN_TERM,
            strings::WIN_BROWSER,
            strings::WIN_MUSIC,
            strings::WIN_SETTINGS,
        ];
        if ui.show_installer {
            apps.push(strings::INSTALLER);
        }
        let n = apps.len() as i32;
        let tray_w = n * metric::DOCK_ICON + (n + 1) * metric::DOCK_PAD;
        let tray_h = metric::DOCK_ICON + 2 * metric::DOCK_PAD;
        let tray = Rect { x: w as i32 / 2 - tray_w / 2, y: h as i32 - metric::BOTTOM_DOCK, w: tray_w, h: tray_h };
        shadow(buf, w, h, tray, radius::MD, elevation::ELEV_2);
        rounded_rect(buf, w, h, tray, 20.0, color::INSET, 0.55);
        rounded_outline(buf, w, h, tray, 20.0, color::HAIRLINE, 0.12);
        // 托盘顶部内高光（玻璃厚度）
        fill_rect(buf, w, h, Rect { x: tray.x + 16, y: tray.y + 1, w: tray.w - 32, h: 1 }, color::HAIRLINE, 0.08);

        self.dock_icons.clear();
        for (i, name) in apps.iter().enumerate() {
            let ix = tray.x + metric::DOCK_PAD + i as i32 * (metric::DOCK_ICON + metric::DOCK_PAD);
            let iy = tray.y + metric::DOCK_PAD;
            let tile = Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON };
            self.dock_icons.push(tile);
            let hovered = tile.contains(ui.mouse.0, ui.mouse.1);
            let running = open_titles.contains(name);

            if *name == strings::INSTALLER {
                // 安装是 Live ISO 里唯一需要被一眼找到的动作：琥珀警示色 + 强调描边
                gradient_tile(buf, w, h, tile, radius::MD, [160, 108, 46], [214, 156, 74], if hovered { 1.0 } else { 0.92 });
            } else {
                // Step2 图标体系：每应用专属「低饱和双色渐变底座」（深→浅对角/纵向），
                // 消灭灰剪影廉价感；色相克制（青/蓝/紫系），不与强调色抢戏
                let base = match i {
                    0 => ([52, 104, 118], [72, 158, 168]),   // 文件：青
                    1 => ([58, 96, 132], [86, 118, 178]),    // 终端：靛
                    2 => ([50, 88, 140], [76, 122, 184]),    // 浏览器：蓝
                    3 => ([94, 76, 138], [134, 108, 180]),   // 音乐：紫
                    _ => ([62, 66, 88], [90, 96, 124]),      // 设置：蓝灰
                };
                gradient_tile(buf, w, h, tile, radius::MD, base.0, base.1, 0.96);
                if hovered {
                    rounded_rect(buf, w, h, tile, radius::MD, color::HAIRLINE, 0.12);
                }
                if running {
                    // 运行中：青色微光（状态反馈，不是装饰）
                    rounded_rect(buf, w, h, tile, radius::MD, color::ACCENT, 0.10);
                }
            }
            rounded_outline(buf, w, h, tile, radius::MD, color::HAIRLINE, if hovered { 0.28 } else { 0.16 });

            // 白色几何符号（全部纯 rect 绘制，不依赖字体字形覆盖）
            let sym = color::TEXT;
            let a = 0.95;
            match i {
                // 文件：页签 + 文件夹体
                0 => {
                    rounded_rect(buf, w, h, Rect { x: ix + 9, y: iy + 12, w: 12, h: 5 }, 2.0, sym, a);
                    rounded_rect(buf, w, h, Rect { x: ix + 10, y: iy + 17, w: 26, h: 17 }, 3.0, sym, a);
                }
                // 终端：">_"（45° 双斜臂 + 下划线）
                1 => {
                    for k in 0..6 {
                        fill_rect(buf, w, h, Rect { x: ix + 13 + k, y: iy + 13 + k, w: 2, h: 2 }, sym, a);
                        fill_rect(buf, w, h, Rect { x: ix + 13 + k, y: iy + 25 - k, w: 2, h: 2 }, sym, a);
                    }
                    fill_rect(buf, w, h, Rect { x: ix + 12, y: iy + 30, w: 20, h: 2 }, sym, a);
                }
                // 浏览器：球体 = 圆环 + 经纬
                2 => {
                    rounded_outline(buf, w, h, Rect { x: ix + 8, y: iy + 8, w: 30, h: 30 }, 15.0, sym, a);
                    fill_rect(buf, w, h, Rect { x: ix + 21, y: iy + 12, w: 3, h: 22 }, sym, a);
                    fill_rect(buf, w, h, Rect { x: ix + 12, y: iy + 21, w: 22, h: 3 }, sym, a);
                }
                // 音乐：双音符（两条符干 + 符头 + 横梁）
                3 => {
                    fill_rect(buf, w, h, Rect { x: ix + 17, y: iy + 13, w: 3, h: 19 }, sym, a);
                    fill_rect(buf, w, h, Rect { x: ix + 26, y: iy + 13, w: 3, h: 19 }, sym, a);
                    fill_rect(buf, w, h, Rect { x: ix + 17, y: iy + 13, w: 12, h: 4 }, sym, a);
                    rounded_rect(buf, w, h, Rect { x: ix + 9, y: iy + 27, w: 13, h: 9 }, 4.0, sym, a);
                    rounded_rect(buf, w, h, Rect { x: ix + 18, y: iy + 27, w: 13, h: 9 }, 4.0, sym, a);
                }
                _ if *name == strings::INSTALLER => {
                    // 安装：向下箭头写入磁盘底座
                    fill_rect(buf, w, h, Rect { x: ix + 20, y: iy + 10, w: 6, h: 12 }, sym, a);
                    for r in 0..6i32 {
                        fill_rect(buf, w, h, Rect { x: ix + 14 + r, y: iy + 22 + r, w: 18 - 2 * r, h: 2 }, sym, a);
                    }
                    fill_rect(buf, w, h, Rect { x: ix + 12, y: iy + 33, w: 22, h: 3 }, sym, a);
                }
                // 设置：齿轮 = 外环 + 内环 + 四齿
                _ => {
                    rounded_outline(buf, w, h, Rect { x: ix + 10, y: iy + 10, w: 26, h: 26 }, 13.0, sym, a);
                    rounded_outline(buf, w, h, Rect { x: ix + 16, y: iy + 16, w: 14, h: 14 }, 7.0, sym, a);
                    for (tx, ty) in [(21, 8), (21, 33), (8, 21), (33, 21)] {
                        fill_rect(buf, w, h, Rect { x: ix + tx, y: iy + ty, w: 5, h: 5 }, sym, a);
                    }
                }
            }

            // 运行指示点：真实反映打开的窗口（强调色，品牌点缀）
            if running {
                rounded_rect(buf, w, h, Rect { x: ix + metric::DOCK_ICON / 2 - 1, y: iy + metric::DOCK_ICON + 5, w: 3, h: 3 }, 1.5, color::ACCENT, 0.9);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 软件光标（fbdev 无硬件指针；白箭头 + 黑描边，指向左上）
// ---------------------------------------------------------------------------

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn draw_cursor(buf: &mut [u32], w: usize, h: usize, mx: f32, my: f32) {
    let (cx, cy) = (mx.round() as i32, my.round() as i32);
    let inside = |x: i32, y: i32| x >= 0 && x <= 10 && y >= x && y <= 15 - x / 2;
    for dy in -1..=16 {
        for dx in -1..=11 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                continue;
            }
            let idx = y as usize * w + x as usize;
            if inside(dx, dy) {
                blend_pixel(buf, idx, [255, 255, 255], 0.95);
            } else if inside(dx, dy - 1)
                || inside(dx, dy + 1)
                || inside(dx - 1, dy)
                || inside(dx + 1, dy)
            {
                blend_pixel(buf, idx, [10, 10, 14], 0.9);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 安装向导窗口（Live ISO → Dock"安装"图标打开）
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_installer(&mut self, buf: &mut [u32], w: usize, h: usize, inst: &InstallerUi, mouse: (f32, f32), mouse_down: bool, tr: Option<&TextRenderer>) {
        let win_w = 640i32;
        let win_h = 400i32;
        let win = Rect {
            x: w as i32 / 2 - win_w / 2,
            y: ((h as i32 - win_h) / 2 - 30).max(56),
            w: win_w,
            h: win_h,
        };
        shadow(buf, w, h, win, radius::LG, elevation::ELEV_3);
        rounded_rect(buf, w, h, win, radius::LG, color::SURFACE_2, 0.98);
        rounded_outline(buf, w, h, win, radius::LG, color::HAIRLINE, 0.16);
        let Some(tr) = tr else { return };

        // 标题栏（TITLE 20px 在 36px 栏内垂直居中）
        tr.draw_bold(buf, w, h, (win.x + 18) as f32, tr.vcenter(win.y as f32, metric::TITLE_H as f32, font::TITLE), "安装 AetherOS", font::TITLE, color::TEXT, 0.95);
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + metric::TITLE_H, w: win.w - 2, h: 1 }, color::HAIRLINE, 0.08);

        // 提示行
        draw_text(tr, buf, w, h, (win.x + 18) as f32, (win.y + 52) as f32,
                  "选择目标磁盘并确认。整盘覆盖，目标盘上的数据将丢失。", font::CAPTION, color::TEXT_DIM, 0.85);

        // 磁盘行
        self.installer_rows.clear();
        let row_y0 = win.y + 78;
        for (i, (dev, mb)) in inst.disks.iter().enumerate() {
            let r = Rect { x: win.x + 16, y: row_y0 + i as i32 * 50, w: win.w - 32, h: 44 };
            if inst.selected == i {
                rounded_rect(buf, w, h, r, radius::MD, color::ACCENT, 0.16);
                rounded_outline(buf, w, h, r, radius::MD, color::ACCENT, 0.45);
                rounded_rect(buf, w, h, Rect { x: r.x + 12, y: r.y + 16, w: 12, h: 12 }, 6.0, color::ACCENT, 0.95);
            } else {
                rounded_rect(buf, w, h, r, radius::MD, color::HAIRLINE, 0.05);
                rounded_outline(buf, w, h, r, radius::MD, color::HAIRLINE, 0.10);
                rounded_outline(buf, w, h, Rect { x: r.x + 11, y: r.y + 15, w: 14, h: 14 }, 7.0, color::TEXT_DIM, 0.7);
            }
            tr.draw_bold(buf, w, h, (r.x + 36) as f32, tr.vcenter(r.y as f32, r.h as f32, font::BODY), dev, font::BODY, color::TEXT, 0.92);
            let sz = format!("{mb} MB 可用");
            let sw = tr.measure(&sz, font::CAPTION);
            draw_text(tr, buf, w, h, (r.x + r.w - 16) as f32 - sw, tr.vcenter(r.y as f32, r.h as f32, font::CAPTION), &sz, font::CAPTION, color::TEXT_DIM, 0.8);
            self.installer_rows.push((r, dev.clone(), *mb));
        }

        // 阶段信息行
        let msg_y = (win.y + win_h - 104) as f32;
        match (inst.phase, inst.message) {
            (InstallerPhase::Running, _) => {
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, "正在写入磁盘，请勿关机…", font::BODY, color::ACCENT, 0.95);
            }
            (InstallerPhase::Done, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, &shown, font::CAPTION, color::SUCCESS, 0.95);
            }
            (InstallerPhase::Failed, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, &shown, font::CAPTION, color::DANGER, 0.95);
            }
            _ => {
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, "就绪。也可以选中磁盘后按 Enter 开始。", font::CAPTION, color::TEXT_DIM, 0.75);
            }
        }

        // 主按钮（四状态：默认 / 悬停 / 按下 / 禁用；禁用以状态色而非灰表达语义）
        let button = Rect { x: win.x + win.w / 2 - 160, y: win.y + win_h - 62, w: 320, h: 44 };
        let label = match inst.phase {
            InstallerPhase::Idle => "开始安装（整盘覆盖）",
            InstallerPhase::Running => "安装中…",
            InstallerPhase::Done => "完成 · 重启后从磁盘引导",
            InstallerPhase::Failed => "重试安装",
        };
        let enabled = matches!(inst.phase, InstallerPhase::Idle | InstallerPhase::Failed);
        let hovered = enabled && button.contains(mouse.0, mouse.1);
        let pressed = hovered && mouse_down;
        if enabled {
            rounded_rect(buf, w, h, button, radius::SM, color::ACCENT, 0.95);
            if hovered {
                rounded_rect(buf, w, h, button, radius::SM, color::HAIRLINE, state::HOVER);
            }
            if pressed {
                // 按下是压暗（叠黑），不是降低底色——后者会透出背景显得像变淡
                rounded_rect(buf, w, h, button, radius::SM, [0, 0, 0], state::PRESSED);
            }
        } else if matches!(inst.phase, InstallerPhase::Done) {
            rounded_rect(buf, w, h, button, radius::SM, color::SUCCESS, 0.85);
        } else {
            rounded_rect(buf, w, h, button, radius::SM, color::SURFACE_3, 0.9);
        }
        let (label_rgb, label_a) = if enabled {
            (color::TEXT, 0.98)
        } else if matches!(inst.phase, InstallerPhase::Done) {
            (color::TEXT, 0.95)
        } else {
            (color::TEXT_FAINT, state::DISABLED)
        };
        let lw = tr.measure_bold(label, font::BODY);
        let ly = tr.vcenter(button.y as f32, button.h as f32, font::BODY);
        tr.draw_bold(buf, w, h, (button.x + button.w / 2) as f32 - lw / 2.0, ly, label, font::BODY, label_rgb, label_a);
        self.installer_button = button;
    }
}

// ---------------------------------------------------------------------------
// L2+ 权限确认弹窗（模态；安装向导的"开始安装"也走这里）
// ---------------------------------------------------------------------------

/// 工具 ID → 用户可读的操作名（确认卡片标题）。
fn tool_label(tool: &str) -> &str {
    match tool {
        "install_disk" => "整盘安装到磁盘",
        "desktop" => "桌面操作",
        "read_file" => "读取文件",
        "sys_probe" => "运行系统探针",
        "sys_info" => "查询系统状态",
        other => other,
    }
}

impl Renderer {
    fn draw_confirm(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        c: &ConfirmUi,
        mouse: (f32, f32),
        mouse_down: bool,
        t: f32,
        tr: Option<&TextRenderer>,
    ) {
        self.confirm_buttons.clear();
        self.confirm_echo = Rect { x: 0, y: 0, w: 0, h: 0 };

        // 高度按内容自适应：标题区 + 参数行 + 后果说明 + 可选回显区 + 按钮区
        let args: Vec<(String, String)> = c
            .arguments
            .iter()
            .map(|(k, v)| {
                let shown = if v.chars().count() > 34 {
                    let head: String = v.chars().take(24).collect();
                    format!("{head}...")
                } else {
                    v.clone()
                };
                (k.clone(), shown)
            })
            .collect();
        let echo_block = if c.echo_required.is_some() { 96 } else { 0 };
        let win_w = 560i32;
        let win_h = 104 + args.len() as i32 * 22 + 54 + echo_block + 68;
        let win = Rect {
            x: w as i32 / 2 - win_w / 2,
            y: ((h as i32 - win_h) / 2 - 10).max(48),
            w: win_w,
            h: win_h,
        };

        // 模态遮罩：压暗背景，明确"必须先回答这个"
        fill_rect(buf, w, h, Rect { x: 0, y: 0, w: w as i32, h: h as i32 }, [0, 0, 0], 0.35);
        shadow(buf, w, h, win, radius::LG, elevation::ELEV_3);
        rounded_rect(buf, w, h, win, radius::LG, color::SURFACE_2, 0.99);
        rounded_outline(buf, w, h, win, radius::LG, color::HAIRLINE, 0.18);
        let Some(tr) = tr else { return };

        // 标题行：等级徽章（L2 黄 / L3 红）+ 操作名
        let badge_rgb = c.badge_color();
        let badge_text = format!("L{} {}", c.level, if c.level >= 3 { "危险" } else { "敏感写" });
        let bw = tr.measure_bold(&badge_text, font::LABEL) + 18.0;
        let badge = Rect { x: win.x + 20, y: win.y + 20, w: bw as i32, h: 22 };
        rounded_rect(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.22);
        rounded_outline(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.7);
        draw_text(tr, buf, w, h, (badge.x + 9) as f32, tr.vcenter(badge.y as f32, badge.h as f32, font::LABEL), &badge_text, font::LABEL, badge_rgb, 1.0);
        tr.draw_bold(
            buf, w, h,
            (badge.x + badge.w + 12) as f32,
            tr.vcenter(win.y as f32 + 18.0, 26.0, 17.0),
            tool_label(c.tool), 17.0, color::TEXT, 0.96,
        );
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + 58, w: win.w - 2, h: 1 }, color::HAIRLINE, 0.08);

        // 参数明文（逐项原样展示：确认的前提是看清对象）
        let mut y = win.y + 74;
        draw_text(tr, buf, w, h, (win.x + 20) as f32, tr.vcenter(y as f32, 20.0, font::CAPTION), "工具", font::CAPTION, color::TEXT_FAINT, 0.9);
        draw_text(tr, buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, 20.0, font::BODY), c.tool, font::BODY, color::TEXT_DIM, 0.95);
        y += 22;
        for (k, v) in &args {
            draw_text(tr, buf, w, h, (win.x + 20) as f32, tr.vcenter(y as f32, 20.0, font::CAPTION), k, font::CAPTION, color::TEXT_FAINT, 0.9);
            tr.draw_bold(buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, 20.0, font::BODY), v, font::BODY, color::TEXT, 0.96);
            y += 22;
        }

        // 后果说明（风险色）+ 警示三角（几何绘制，不依赖字体字形）
        y += 10;
        let cx = win.x + 27;
        let cy = y + 10;
        for row in 0..9i32 {
            let half = row / 2;
            fill_rect(buf, w, h, Rect { x: cx - half, y: cy - 8 + row, w: half * 2 + 1, h: 1 }, badge_rgb, 0.95);
        }
        let mut shown: String = c.consequence.to_string();
        while tr.measure(&shown, font::CAPTION) > (win.w - 64) as f32 && shown.chars().count() > 6 {
            shown.pop();
        }
        draw_text(tr, buf, w, h, (win.x + 42) as f32, tr.vcenter(y as f32, 20.0, font::CAPTION), &shown, font::CAPTION, badge_rgb, 0.95);
        y += 34;

        // L3 回显确认：必须原样输入目标，防"手滑点确认"
        if let Some(target) = c.echo_required {
            let tip = format!("L3 危险操作：请原样输入 {target} 以确认");
            draw_text(tr, buf, w, h, (win.x + 20) as f32, y as f32, &tip, font::CAPTION, color::TEXT_DIM, 0.9);
            y += 22;
            let field = Rect { x: win.x + 20, y, w: win.w - 40, h: 38 };
            let ok = c.echo_ok();
            rounded_rect(buf, w, h, field, radius::SM, color::INSET, 0.9);
            rounded_outline(buf, w, h, field, radius::SM, if ok { color::SUCCESS } else { color::HAIRLINE }, if ok { 0.75 } else { 0.18 });
            let fty = tr.vcenter(field.y as f32, field.h as f32, font::BODY);
            if c.echo_input.is_empty() {
                let hint = format!("输入 {target}");
                draw_text(tr, buf, w, h, (field.x + 12) as f32, fty, &hint, font::BODY, color::TEXT_FAINT, 0.8);
            } else {
                draw_text(tr, buf, w, h, (field.x + 12) as f32, fty, c.echo_input, font::BODY, color::TEXT, 0.96);
            }
            let tw = tr.measure(c.echo_input, font::BODY);
            if ((t * 2.0) as i32) % 2 == 0 {
                fill_rect(buf, w, h, Rect { x: (field.x + 12) as i32 + tw as i32, y: field.y + 10, w: 2, h: 18 }, color::ACCENT, 0.9);
            }
            self.confirm_echo = field;
            y += 50;
        }

        // 按钮：拒绝（danger 描边）/ 允许一次（accent 实心，回显未匹配时禁用）。
        // 不做"永久允许"：安全决策，计划明确不引入。
        let bw2 = 150i32;
        let bh = 42i32;
        let by = y.max(win.y + win_h - 62);
        let deny = Rect { x: win.x + win.w - 20 - bw2 * 2 - 12, y: by, w: bw2, h: bh };
        let allow = Rect { x: win.x + win.w - 20 - bw2, y: by, w: bw2, h: bh };

        let deny_hover = deny.contains(mouse.0, mouse.1);
        let deny_fill = if deny_hover && mouse_down { 0.22 } else if deny_hover { 0.14 } else { 0.06 };
        rounded_rect(buf, w, h, deny, radius::SM, color::DANGER, deny_fill);
        rounded_outline(buf, w, h, deny, radius::SM, color::DANGER, if deny_hover { 0.85 } else { 0.6 });
        let dw = tr.measure_bold("拒绝", font::BODY);
        tr.draw_bold(buf, w, h, (deny.x + deny.w / 2) as f32 - dw / 2.0, tr.vcenter(deny.y as f32, deny.h as f32, font::BODY), "拒绝", font::BODY, color::DANGER, 0.98);

        let ready = c.echo_ok();
        let allow_hover = ready && allow.contains(mouse.0, mouse.1);
        if ready {
            rounded_rect(buf, w, h, allow, radius::SM, color::ACCENT, 0.95);
            if allow_hover {
                rounded_rect(buf, w, h, allow, radius::SM, color::HAIRLINE, state::HOVER);
                if mouse_down {
                    rounded_rect(buf, w, h, allow, radius::SM, [0, 0, 0], state::PRESSED);
                }
            }
        } else {
            rounded_rect(buf, w, h, allow, radius::SM, color::SURFACE_3, 0.9);
        }
        let (lrgb, la) = if ready { (color::TEXT, 0.98) } else { (color::TEXT_FAINT, state::DISABLED) };
        let lw = tr.measure_bold("允许一次", font::BODY);
        tr.draw_bold(buf, w, h, (allow.x + allow.w / 2) as f32 - lw / 2.0, tr.vcenter(allow.y as f32, allow.h as f32, font::BODY), "允许一次", font::BODY, lrgb, la);

        self.confirm_buttons.push((allow, ConfirmButton::Allow));
        self.confirm_buttons.push((deny, ConfirmButton::Deny));
    }
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

fn draw_toast(buf: &mut [u32], w: usize, h: usize, msg: &str, age: f32, tr: Option<&TextRenderer>) {
    let Some(tr) = tr else { return };
    let alpha = ((1.6 - age) / 0.4).clamp(0.0, 1.0) * 0.95;
    if alpha <= 0.0 {
        return;
    }
    let tw = tr.measure(msg, font::BODY) + 40.0;
    let r = Rect {
        x: w as i32 / 2 - tw as i32 / 2,
        y: h as i32 - metric::BOTTOM_DOCK - 52 - 16 - 60,
        w: tw as i32,
        h: 40,
    };
    shadow(buf, w, h, r, radius::SM, alpha * elevation::ELEV_2);
    rounded_rect(buf, w, h, r, 20.0, color::SURFACE_2, alpha * 0.92);
    rounded_outline(buf, w, h, r, 20.0, color::HAIRLINE, alpha * 0.15);
    fill_rect(buf, w, h, Rect { x: r.x + 18, y: r.y + 17, w: 6, h: 6 }, color::ACCENT, alpha);
    draw_text(tr, buf, w, h, (r.x + 34) as f32, tr.vcenter(r.y as f32, r.h as f32, font::BODY), msg, font::BODY, color::TEXT, alpha);
}

// ---------------------------------------------------------------------------
// BMP 导出（自检/截图模式）
// ---------------------------------------------------------------------------

pub fn write_bmp(path: &str, buf: &[u32], w: usize, h: usize) -> anyhow::Result<()> {
    use anyhow::Context;
    let row_pad = (4 - (w * 3) % 4) % 4;
    let data_size = (w * 3 + row_pad) * h;
    let mut out = Vec::with_capacity(54 + data_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + data_size) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(data_size as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        for x in 0..w {
            let p = buf[y * w + x];
            out.push((p & 0xff) as u8);
            out.push(((p >> 8) & 0xff) as u8);
            out.push(((p >> 16) & 0xff) as u8);
        }
        for _ in 0..row_pad {
            out.push(0);
        }
    }
    std::fs::write(path, out).with_context(|| format!("write {path}"))?;
    Ok(())
}
