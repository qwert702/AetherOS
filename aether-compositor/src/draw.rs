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
/// 双模：**明亮（默认，产品主视觉）** 与 深空（备选，`--theme dark`）。
/// 颜色一律走函数取值，调用点形如 `color::surface_1()`；模式在进程启动时定一次。
///
/// 层级模型（从底到顶）：壁纸 → INSET（底座：菜单栏/Dock/侧栏/终端）
/// → SURFACE_1（窗口）→ SURFACE_2（内容"纸面"）→ SURFACE_3（输入框/浮层）。
///
/// 两种模式的**明度方向一致**：内容面比窗口体更亮（"抬起来"），
/// 底座比窗口体更暗（"沉下去"）。深色模式里"更亮"是加白，明亮模式里是加黑，
/// 方向不变、语义不变——这样所有调用点不必区分模式。
pub mod theme {
    /// 色板 §3.1：中性层级 + 克制使用的强调色
    pub mod color {
        use std::sync::atomic::{AtomicU8, Ordering};

        /// 主题模式。启动时设定一次，绘制期间只读。
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Mode {
            /// 明亮通透（默认）
            Light = 0,
            /// 深空
            Dark = 1,
        }

        static MODE: AtomicU8 = AtomicU8::new(Mode::Light as u8);

        pub fn set_mode(m: Mode) {
            MODE.store(m as u8, Ordering::Relaxed);
        }

        #[inline]
        pub fn is_light() -> bool {
            MODE.load(Ordering::Relaxed) == Mode::Light as u8
        }

        #[inline]
        fn pick(light: [u8; 3], dark: [u8; 3]) -> [u8; 3] {
            if is_light() {
                light
            } else {
                dark
            }
        }

        // —— 中性层级 ——
        /// 壁纸渐变顶
        pub fn bg_top() -> [u8; 3] {
            pick([244, 247, 253], [22, 24, 34])
        }
        /// 壁纸渐变底
        pub fn bg_bottom() -> [u8; 3] {
            pick([218, 228, 246], [46, 52, 68])
        }
        /// 底座（菜单栏、侧栏、终端底、状态条、音乐面板）。
        /// 浅色下**必须比窗口体暗**，否则底座与窗口同色，所有区域糊成一片。
        pub fn inset() -> [u8; 3] {
            pick([226, 229, 236], [30, 32, 41])
        }
        /// 窗口/面板主面
        pub fn surface_1() -> [u8; 3] {
            pick([242, 244, 248], [46, 49, 60])
        }
        /// 内容"纸面"、卡片、浮层、侧栏选中
        pub fn surface_2() -> [u8; 3] {
            pick([255, 255, 255], [64, 68, 80])
        }
        /// 输入框、悬停中的控件
        pub fn surface_3() -> [u8; 3] {
            pick([232, 235, 241], [80, 84, 97])
        }
        /// 对比线：描边、分隔线、悬停叠层。
        /// 深色模式是白线，明亮模式是黑线——**所有调用点的 alpha 无需改动**。
        pub fn hairline() -> [u8; 3] {
            pick([16, 18, 24], [255, 255, 255])
        }
        /// 主文字
        pub fn text() -> [u8; 3] {
            pick([26, 28, 34], [240, 241, 246])
        }
        /// 次要文字
        pub fn text_dim() -> [u8; 3] {
            pick([94, 98, 108], [180, 182, 192])
        }
        /// 占位/禁用文字
        pub fn text_faint() -> [u8; 3] {
            pick([146, 150, 160], [136, 139, 150])
        }

        /// 顶部内高光：两种模式都是白（明亮模式下表现为"玻璃上缘"）。
        pub const HIGHLIGHT: [u8; 3] = [255, 255, 255];
        /// 模态遮罩（压暗背景；明亮模式用较轻的黑）
        pub fn scrim() -> [u8; 3] {
            pick([28, 32, 42], [0, 0, 0])
        }
        /// 模态遮罩不透明度
        pub fn scrim_alpha() -> f32 {
            if is_light() {
                0.24
            } else {
                0.35
            }
        }

        /// 悬浮玻璃层底色（AI 指令条、Dock 托盘、Toast 这类"浮在桌面上"的控件）。
        ///
        /// 明亮模式**必须是白**：用底座色（浅灰）会读成一块灰板，
        /// 整个界面立刻廉价。深色模式则是比窗口更暗的底座。
        pub fn glass() -> [u8; 3] {
            pick([255, 255, 255], [30, 32, 41])
        }

        /// 悬浮玻璃层不透明度（明亮模式要提高，否则粉彩壁纸透上来发脏）
        pub fn glass_alpha(hovered: bool) -> f32 {
            if is_light() {
                if hovered {
                    0.98
                } else {
                    0.95
                }
            } else if hovered {
                0.82
            } else {
                0.75
            }
        }

        // —— 强调色（明亮模式需在白底上达到可读对比度，故整体加深）——
        /// 主青：选中、焦点、主按钮
        pub fn accent() -> [u8; 3] {
            pick([11, 132, 150], [64, 190, 205])
        }
        /// 辅紫：AI 相关元素
        pub fn accent_violet() -> [u8; 3] {
            pick([110, 90, 205], [150, 130, 220])
        }
        /// 完成、在线
        pub fn success() -> [u8; 3] {
            pick([22, 142, 90], [52, 199, 123])
        }
        /// 预警、L2 确认
        pub fn warning() -> [u8; 3] {
            pick([186, 120, 16], [255, 179, 64])
        }
        /// 危险操作、L3、错误
        pub fn danger() -> [u8; 3] {
            pick([208, 56, 52], [255, 92, 88])
        }
        /// 主操作按钮（"允许一次"）底色：强调色加深。
        ///
        /// 直接用 `accent()` 做底 + `text()` 做字，实测对比度**深色 2.07:1、
        /// 明亮 4.11:1**，都低于 WCAG AA 正文 4.5:1 —— 用户在最需要看清的
        /// "放行"按钮上读不清字。本 token 配白字把它提到 5.3:1 / 6.4:1。
        pub fn accent_strong() -> [u8; 3] {
            pick([9, 105, 120], [34, 118, 132])
        }
        /// 危险色**作为文字**时的取值：深色下比 `danger()` 更亮、明亮下更深，
        /// 才能在窗口底上达到 AA 4.5:1（"拒绝"按钮用；`danger()` 本身
        /// 在深色底上只有 3.2:1，做正文色不达标）。
        pub fn danger_text() -> [u8; 3] {
            pick([176, 32, 30], [255, 163, 159])
        }
        /// 预警色**作为文字**时的取值（L2 徽章、预警文案用）。
        /// 理由同 `danger_text()`：`warning()` 在明亮底上只有 3.7:1。
        pub fn warning_text() -> [u8; 3] {
            pick([146, 92, 8], [255, 205, 130])
        }
        /// 信息提示（工具调用卡片等）。
        ///
        /// 当前无调用点：工具结果气泡改用左侧状态条（SUCCESS/DANGER）表达成败，
        /// 信息色没有落地位置。作为色板语义完整性保留，显式标注以免掩盖
        /// 将来真正的死代码。
        #[allow(dead_code)]
        pub fn info() -> [u8; 3] {
            pick([28, 106, 214], [90, 160, 250])
        }

        // —— 窗控三色（红绿灯两种模式一致，这是"系统级"识别色）——
        pub fn close() -> [u8; 3] {
            [255, 95, 86]
        }
        pub fn min() -> [u8; 3] {
            [254, 188, 46]
        }
        pub fn zoom() -> [u8; 3] {
            [39, 201, 63]
        }

        // —— 极光带（壁纸）：明亮模式是低饱和粉彩，深空模式是发光带 ——
        pub fn aurora_cyan() -> [u8; 3] {
            pick([120, 206, 234], [72, 200, 218])
        }
        pub fn aurora_violet() -> [u8; 3] {
            pick([166, 152, 238], [152, 130, 224])
        }
        pub fn aurora_blue() -> [u8; 3] {
            pick([150, 174, 240], [136, 126, 226])
        }
        /// 极光带强度系数（明亮模式下带需要更"淡"才不脏）
        pub fn aurora_gain() -> f32 {
            if is_light() {
                0.95
            } else {
                1.0
            }
        }
        /// 粉彩点缀（仅明亮壁纸使用）
        pub fn aurora_pink() -> [u8; 3] {
            pick([246, 198, 226], [216, 106, 176])
        }
        /// 暗角系数（明亮模式只留极轻的一圈，避免发灰）
        pub fn vignette() -> f32 {
            if is_light() {
                0.03
            } else {
                0.20
            }
        }
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

    /// 阴影档位 §3.3（值即影子强度；`shadow()` 按档解释）。
    ///
    /// 明亮模式的影子必须**更轻**：`shadow()` 是 5 层叠加，边缘处合成后接近
    /// 强度的 2 倍，白底上给 0.25 会压出一圈脏黑边。
    pub mod elevation {
        use super::color::is_light;

        #[inline]
        fn pick(light: f32, dark: f32) -> f32 {
            if is_light() {
                light
            } else {
                dark
            }
        }

        /// 窗口浮起
        pub fn elev_1() -> f32 {
            pick(0.17, 0.25)
        }
        /// 非活动窗口
        pub fn elev_1_dim() -> f32 {
            pick(0.11, 0.14)
        }
        /// 弹窗、浮层（下拉、AI 指令条、Toast、Dock）
        pub fn elev_2() -> f32 {
            pick(0.21, 0.4)
        }
        /// 模态（安装向导、权限确认）
        pub fn elev_3() -> f32 {
            pick(0.27, 0.55)
        }
    }

    /// 交互态 alpha（悬停/按下/禁用统一在此取值）
    pub mod state {
        use super::color::is_light;

        #[inline]
        fn pick(light: f32, dark: f32) -> f32 {
            if is_light() {
                light
            } else {
                dark
            }
        }

        /// 悬停高亮叠层（颜色随模式：深色加白、明亮加黑）
        pub fn hover() -> f32 {
            pick(0.055, 0.07)
        }
        /// 悬停高亮叠层（选中项/强调）
        pub fn hover_strong() -> f32 {
            pick(0.10, 0.14)
        }
        /// 按下压暗
        pub fn pressed() -> f32 {
            pick(0.08, 0.12)
        }
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
        /// 窗控圆点直径与间距（§3.1：10px / 7px，Step4 从 12/8 收细——12px 在 36px 标题栏里偏大）
        pub const LIGHT_D: i32 = 10;
        pub const LIGHT_GAP: i32 = 7;
    }
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 两色混合（混色、提亮、压暗都走它，避免各处手写通道运算）。
#[inline]
pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [
        lerp(a[0] as f32, b[0] as f32, t) as u8,
        lerp(a[1] as f32, b[1] as f32, t) as u8,
        lerp(a[2] as f32, b[2] as f32, t) as u8,
    ]
}

/// 按像素宽度截断并加省略号（卡片标签、状态栏等单行文本用）。
pub fn ellipsize(tr: &TextRenderer, text: &str, px: f32, max_w: f32) -> String {
    if tr.measure(text, px) <= max_w {
        return text.to_string();
    }
    let mut s = String::new();
    for ch in text.chars() {
        let mut probe = s.clone();
        probe.push(ch);
        probe.push('…');
        if tr.measure(&probe, px) > max_w {
            break;
        }
        s.push(ch);
    }
    s.push('…');
    s
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

/// 纯黑叠加的快速路径：`dst * (1 - alpha)`。
///
/// 投影只是"把下面的像素压暗"，不需要通用混色的三次 lerp 与 clamp。
/// `shadow()` 每帧要跑 5 层 × 全窗口面积，这条路径是它的主要成本之一。
#[inline]
fn darken_pixel(buf: &mut [u32], idx: usize, alpha: f32) {
    if alpha <= 0.0 || idx >= buf.len() {
        return;
    }
    let k = 1.0 - alpha;
    let d = buf[idx];
    let r = (((d >> 16) & 0xff) as f32 * k) as u32;
    let g = (((d >> 8) & 0xff) as f32 * k) as u32;
    let b = ((d & 0xff) as f32 * k) as u32;
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
    // `.max(0)` 不可省：`rect.x + rect.w` 是 i32 运算，若为负则 `as usize` 回卷成
    // 巨值，`.min(w)` 会把它钳成 w —— 结果是**误填整行**（P3-28）。
    let x1 = ((rect.x + rect.w).max(0) as usize).min(w);
    let y1 = ((rect.y + rect.h).max(0) as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(buf, y * w + x, rgb, alpha);
        }
    }
}

/// 圆角矩形的有符号距离（负 = 内部）。所有圆角绘制与裁切共用它，
/// 保证"描边 / 填充 / 裁切"三者的形状严格一致。
///
/// 性能要点：只有**角部**（dx>0 且 dy>0）才需要开方。边部与内部
/// （至少一个分量为非正）直接用分量相加即可 —— 这是恒等变形，不是近似。
/// 圆角矩形的角部面积占比极小，条件化后省掉绝大部分 sqrt（软件光栅化下
/// sqrt 是最贵的一步，`draw_window` 的开销几乎全在这里）。
#[inline]
fn sdf_round_rect(r: Rect, radius: f32, x: f32, y: f32) -> f32 {
    let radius = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let qx_half = r.w as f32 / 2.0 - radius;
    let qy_half = r.h as f32 / 2.0 - radius;
    let dx = (x - cx).abs() - qx_half;
    let dy = (y - cy).abs() - qy_half;
    let ox = dx.max(0.0);
    let oy = dy.max(0.0);
    let outside = if ox > 0.0 && oy > 0.0 {
        (ox * ox + oy * oy).sqrt()
    } else {
        ox + oy
    };
    outside + dx.max(dy).min(0.0) - radius
}

/// 圆角矩形某一行上"完全在形状内部"的 x 区间（不含圆角抗锯齿带）。
///
/// 圆角矩形里绝大多数像素离圆角很远，逐像素求 SDF 是浪费。先用解析式
/// 把每行切成"两端圆角带 + 中间直填带"，中间部分可以直接混色。
#[inline]
fn row_inner_span(r: Rect, radius: f32, y: f32) -> (i32, i32) {
    let radius = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    let top = r.y as f32;
    let bot = (r.y + r.h) as f32;
    let dy = (y - top).min(bot - y);
    if dy >= radius {
        return (r.x, r.x + r.w);
    }
    if dy < 0.0 {
        // 该行整行都在形状之外（扫描范围含 ±1 像素的抗锯齿余量）：
        // 必须返回**空区间**。否则 `radius² - k²` 会被 max(0) 截断成 0，
        // 算出一个"看似合法"的直填带，把形状外的像素整行刷上颜色 ——
        // 表现为胶囊/圆角矩形边缘多出一整行（实测：AI 指令条底边 512 像素被误填）。
        let mid = r.x + r.w / 2;
        return (mid, mid);
    }
    let k = radius - dy;
    // 直填带要求像素覆盖率**恰好为 1**，即 d ≤ -0.5，也就是
    // sqrt(ox² + k²) ≤ radius - 0.5 —— 不是 d ≤ 0（那只是形状边界）。
    // 早先按 d ≤ 0 推边界，圆角边缘会被当成满覆盖，实测覆盖率只有 0.90，
    // 表现是圆角变实、与逐像素版本对不上。
    let r_eff = (radius - 0.5).max(0.0);
    let inner = if k >= r_eff {
        0.0
    } else {
        (r_eff * r_eff - k * k).sqrt()
    };
    // +1 像素余量：ceil 后仍可能有浮点临界像素，宁可交给 SDF 逐像素判
    let i = (radius - inner).ceil() as i32 + 1;
    (r.x + i, r.x + r.w - i)
}

/// 抗锯齿圆角矩形（符号距离场覆盖率）。
pub fn rounded_rect(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
    let x0 = (r.x as f32 - 1.0).max(0.0) as usize;
    let y0 = (r.y as f32 - 1.0).max(0.0) as usize;
    let x1 = ((r.x + r.w) as f32 + 1.0).min(w as f32) as usize;
    let y1 = ((r.y + r.h) as f32 + 1.0).min(h as f32) as usize;
    let radius = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    for y in y0..y1 {
        let ay = y as f32 + 0.5;
        let row = y * w;
        // 每行切成"两端圆角带 + 中间直填带"：中间部分不需要 SDF
        let (ix0, ix1) = row_inner_span(r, radius, ay);
        let a = (ix0.max(x0 as i32)).max(0) as usize;
        let b = (ix1.min(x1 as i32)).max(0) as usize;
        for x in a..b {
            blend_pixel(buf, row + x, rgb, alpha);
        }
        for x in x0..a {
            let d = sdf_round_rect(r, radius, x as f32 + 0.5, ay);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, row + x, rgb, alpha * cov);
            }
        }
        for x in b..x1 {
            let d = sdf_round_rect(r, radius, x as f32 + 0.5, ay);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, row + x, rgb, alpha * cov);
            }
        }
    }
}

/// 在 `shape` 的圆角形状内填充 `area`。
///
/// 用途：窗口内容区（侧栏、内容面、状态条）必须贴合窗口自身的圆角，
/// 否则方角会把窗口底角"切"成直角，圆角窗口立刻露馅。
fn fill_clipped(buf: &mut [u32], w: usize, h: usize, area: Rect, shape: Rect, shape_radius: f32, rgb: [u8; 3], alpha: f32) {
    if area.w <= 0 || area.h <= 0 {
        return;
    }
    let x0 = area.x.max(0) as usize;
    let y0 = area.y.max(0) as usize;
    let x1 = ((area.x + area.w) as usize).min(w);
    let y1 = ((area.y + area.h) as usize).min(h);
    let radius = shape_radius.min(shape.w as f32 / 2.0).min(shape.h as f32 / 2.0);
    for y in y0..y1 {
        let ay = y as f32 + 0.5;
        let row = y * w;
        // 同 rounded_rect：只有两端圆角带需要逐像素判裁切
        let (ix0, ix1) = row_inner_span(shape, radius, ay);
        let a = (ix0.max(x0 as i32)).max(0) as usize;
        let b = (ix1.min(x1 as i32)).max(0) as usize;
        for x in a..b {
            blend_pixel(buf, row + x, rgb, alpha);
        }
        for x in x0..a {
            let d = sdf_round_rect(shape, radius, x as f32 + 0.5, ay);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, row + x, rgb, alpha * cov);
            }
        }
        for x in b..x1 {
            let d = sdf_round_rect(shape, radius, x as f32 + 0.5, ay);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, row + x, rgb, alpha * cov);
            }
        }
    }
}


/// 抗锯齿圆角轮廓线（约 2px 描边带）。
///
/// 只扫描**可能命中描边带**的像素：上下边带整行扫，中间行只扫两端的圆角区。
/// 原实现在整个包围盒上逐像素求 SDF，而真正落进 [-1.5, 0.5) 的不足 1% ——
/// 一个 620x500 的窗口描边要白跑 31 万次距离计算。
pub fn rounded_outline(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
    let x0 = (r.x - 2).max(0) as usize;
    let y0 = (r.y - 2).max(0) as usize;
    let x1 = ((r.x + r.w) as usize + 2).min(w);
    let y1 = ((r.y + r.h) as usize + 2).min(h);
    let radius = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let qx_half = r.w as f32 / 2.0 - radius;
    let qy_half = r.h as f32 / 2.0 - radius;
    let top = r.y as f32;
    let bot = (r.y + r.h) as f32;

    macro_rules! scan {
        ($x:expr, $y:expr) => {{
            let dx = ($x as f32 + 0.5 - cx).abs() - qx_half;
            let dy = ($y as f32 + 0.5 - cy).abs() - qy_half;
            let ox = dx.max(0.0);
            let oy = dy.max(0.0);
            let outside = if ox > 0.0 && oy > 0.0 {
                (ox * ox + oy * oy).sqrt()
            } else {
                ox + oy
            };
            let d = outside + dx.max(dy).min(0.0) - radius;
            // 只保留边框带 [-1.5, 0.5)
            if d < 0.5 && d > -1.5 {
                let cov = (0.5 - d).clamp(0.0, 1.0);
                blend_pixel(buf, $y as usize * w + $x as usize, rgb, alpha * cov);
            }
        }};
    }

    for y in y0..y1 {
        let ay = y as f32 + 0.5;
        if ay - top < 3.0 || bot - ay < 3.0 {
            // 上下边带：整行都可能命中
            for x in x0..x1 {
                scan!(x, y);
            }
        } else {
            // 中间行：只有两端圆角带
            let (ix0, ix1) = row_inner_span(r, radius, ay);
            // 扫描边界必须比内部区间**更宽**：直填带只保证 d ≤ -0.5，
            // 而描边带要 d > -1.5 —— 紧贴直填带外沿的像素仍在描边带内。
            // 早先写成 ix0-2 / ix1+2（向内收），把这一圈描边整段漏掉，
            // 表现为圆角内侧缺一小段线（实测 140 像素）。
            let mid0 = ((ix0 + 2).max(x0 as i32).max(0)) as usize;
            let mid1 = ((ix1 - 2).min(x1 as i32).max(0)) as usize;
            for x in x0..mid0.min(x1) {
                scan!(x, y);
            }
            for x in mid1.max(x0)..x1 {
                scan!(x, y);
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
    let radius = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let qx_half = r.w as f32 / 2.0 - radius;
    let qy_half = r.h as f32 / 2.0 - radius;
    let top = r.y as f32;
    let bot = (r.y + r.h) as f32;
    let span = r.w.max(1) as f32;

    macro_rules! scan {
        ($x:expr, $y:expr) => {{
            let dx = ($x as f32 + 0.5 - cx).abs() - qx_half;
            let dy = ($y as f32 + 0.5 - cy).abs() - qy_half;
            let ox = dx.max(0.0);
            let oy = dy.max(0.0);
            let outside = if ox > 0.0 && oy > 0.0 {
                (ox * ox + oy * oy).sqrt()
            } else {
                ox + oy
            };
            let d = outside + dx.max(dy).min(0.0) - radius;
            if d < 0.5 && d > -1.5 {
                let cov = (0.5 - d).clamp(0.0, 1.0);
                let t = (($x as f32 - r.x as f32) / span).clamp(0.0, 1.0);
                let rgb = [
                    lerp(left[0] as f32, right[0] as f32, t) as u8,
                    lerp(left[1] as f32, right[1] as f32, t) as u8,
                    lerp(left[2] as f32, right[2] as f32, t) as u8,
                ];
                blend_pixel(buf, $y as usize * w + $x as usize, rgb, alpha * cov);
            }
        }};
    }

    for y in y0..y1 {
        let ay = y as f32 + 0.5;
        if ay - top < 3.0 || bot - ay < 3.0 {
            for x in x0..x1 {
                scan!(x, y);
            }
        } else {
            let (ix0, ix1) = row_inner_span(r, radius, ay);
            // 扫描边界必须比内部区间**更宽**：直填带只保证 d ≤ -0.5，
            // 而描边带要 d > -1.5 —— 紧贴直填带外沿的像素仍在描边带内。
            // 早先写成 ix0-2 / ix1+2（向内收），把这一圈描边整段漏掉，
            // 表现为圆角内侧缺一小段线（实测 140 像素）。
            let mid0 = ((ix0 + 2).max(x0 as i32).max(0)) as usize;
            let mid1 = ((ix1 - 2).min(x1 as i32).max(0)) as usize;
            for x in x0..mid0.min(x1) {
                scan!(x, y);
            }
            for x in mid1.max(x0)..x1 {
                scan!(x, y);
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
        let a0 = strength * (1.0 - i as f32 / 5.0) * 0.80;
        let rad = radius + grow;
        let x0 = (s.x as f32 - 1.0).max(0.0) as usize;
        let y0 = (s.y as f32 - 1.0).max(0.0) as usize;
        let x1 = ((s.x + s.w) as f32 + 1.0).min(w as f32) as usize;
        let y1 = ((s.y + s.h) as f32 + 1.0).min(h as f32) as usize;
        for y in y0..y1 {
            let row = y * w;
            for x in x0..x1 {
                let d = sdf_round_rect(s, rad, x as f32 + 0.5, y as f32 + 0.5);
                // 完全在内部（d < -1）就是满强度，跳过 clamp
                let a = if d < -1.0 { a0 } else { a0 * (0.5 - d).clamp(0.0, 1.0) };
                darken_pixel(buf, row + x, a);
            }
        }
    }
}

/// 多段垂直渐变的圆角矩形：逐像素 SDF + 逐个色标插值。
///
/// 窗口正是靠它一次成型——标题栏一条"硬变"色标 + 主体平色 + 底部微暗，
/// 全程同一遍 SDF，不存在二次叠加导致的接缝。
fn gradient_stops(buf: &mut [u32], w: usize, r: Rect, radius: f32, stops: &[(f32, [u8; 3])], alpha: f32) {
    if r.w <= 0 || r.h <= 0 || stops.is_empty() {
        return;
    }
    let clip_x0 = r.x.max(0) as usize;
    let clip_x1 = ((r.x + r.w).max(0) as usize).min(w);
    if clip_x1 <= clip_x0 {
        return;
    }
    let rows = buf.len() / w.max(1);
    for y in 0..r.h {
        let ay = r.y + y;
        if ay < 0 || ay as usize >= rows {
            continue;
        }
        let t = (y as f32 + 0.5) / r.h as f32;
        let rgb = sample_stops(stops, t);
        let row = ay as usize * w;
        // 每行切成"两端圆角带 + 中间直填带"：中间部分不做 SDF
        let (ix0, ix1) = row_inner_span(r, radius, ay as f32 + 0.5);
        let a = (ix0.max(clip_x0 as i32)).max(0) as usize;
        let b = (ix1.min(clip_x1 as i32)).max(0) as usize;
        for x in a..b {
            blend_pixel(buf, row + x, rgb, alpha);
        }
        for x in clip_x0..a {
            let d = sdf_round_rect(r, radius, x as f32 + 0.5, ay as f32 + 0.5);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, row + x, rgb, alpha * cov);
            }
        }
        for x in b..clip_x1 {
            let d = sdf_round_rect(r, radius, x as f32 + 0.5, ay as f32 + 0.5);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, row + x, rgb, alpha * cov);
            }
        }
    }
}

/// 按垂直位置 t（0..1）在多段色标上取色；同一 t 上放两个不同色即为硬变。
fn sample_stops(stops: &[(f32, [u8; 3])], t: f32) -> [u8; 3] {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if t <= t1 {
            let span = t1 - t0;
            let k = if span.abs() < 1e-6 { 1.0 } else { ((t - t0) / span).clamp(0.0, 1.0) };
            return mix(c0, c1, k);
        }
    }
    stops[stops.len() - 1].1
}

/// 双色垂直渐变圆角块（Dock 图标底座、AI 徽标等）。
fn gradient_tile(buf: &mut [u32], w: usize, r: Rect, radius: f32, top: [u8; 3], bottom: [u8; 3], alpha: f32) {
    gradient_stops(buf, w, r, radius, &[(0.0, top), (1.0, bottom)], alpha);
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
            AiStatus::Local => color::accent(),
            AiStatus::Cloud => color::accent_violet(),
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
    /// 壁纸 + 全部窗口投影的合成层。
    ///
    /// 投影只取决于窗口几何与激活态，鼠标移动时完全不变；而它每帧要跑
    /// 5 层 × 全窗口面积的 SDF，是 `draw_window` 的主要成本。把它烘焙进
    /// 这一层后，稳态每帧只剩一次内存拷贝。
    shadow_layer: Vec<u32>,
    /// 烘焙层对应的窗口几何指纹（变了才重算）
    shadow_key: u64,
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
            shadow_layer: Vec::new(),
            shadow_key: 0,
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

    /// 性能计时开关（`AETHER_RENDER_TIMING=1` 启用；环境变量只读一次）。
    fn timing_enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var("AETHER_RENDER_TIMING").is_ok())
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
        let timing = Self::timing_enabled();
        // 计时关闭时 tmark 只写不读（宏体不展开），显式放行
        #[allow(unused_assignments)]
        let mut tmark = std::time::Instant::now();
        macro_rules! mark {
            ($name:literal) => {
                if timing {
                    eprintln!(
                        "[render] {:<10} {:>7.2} ms",
                        $name,
                        tmark.elapsed().as_secs_f64() * 1000.0
                    );
                    tmark = std::time::Instant::now();
                }
            };
        }

        // 壁纸漂移是**分钟级**的变化，但生成一次要 ~0.4s（逐像素极光带 + 颗粒），
        // 放在帧内就是一次肉眼可见的掉帧。240 帧在 10fps 下是 24 秒、在 30fps 下
        // 只剩 8 秒 —— 帧率提上来之后必须把间隔一起提上去，否则"优化帧率"
        // 反而让周期性卡顿变密。1800 帧 @30fps ≈ 60 秒。
        const BG_REFRESH_FRAMES: u32 = 1800;
        if self.frames_since_bg > BG_REFRESH_FRAMES || self.bg_w != w || self.bg_h != h {
            if self.bg.len() != w * h {
                self.bg = vec![0; w * h];
            }
            draw_background(&mut self.bg, w, h, t);
            self.frames_since_bg = 0;
            self.bg_w = w;
            self.bg_h = h;
            self.shadow_key = 0; // 壁纸变了，烘焙层必须重算
        }
        mark!("bg_gen");
        self.frames_since_bg += 1;

        // 投影烘焙：只取决于窗口几何与激活态，鼠标移动时完全不变。
        // 未变化时这一层直接复用，稳态每帧只剩一次内存拷贝。
        let skey = desktop_key(desktop);
        if self.shadow_layer.len() != w * h || self.shadow_key != skey {
            if self.shadow_layer.len() != w * h {
                self.shadow_layer = vec![0; w * h];
            }
            self.shadow_layer.copy_from_slice(&self.bg);
            for (i, win) in desktop.wins.iter().enumerate() {
                let active = i == desktop.active;
                shadow(
                    &mut self.shadow_layer,
                    w,
                    h,
                    win.rect,
                    radius::LG,
                    if active { elevation::elev_1() } else { elevation::elev_1_dim() },
                );
            }
            self.shadow_key = skey;
        }
        buf.copy_from_slice(&self.shadow_layer);
        mark!("bg_copy");

        if let Some(z) = ui.snap {
            rounded_rect(buf, w, h, z, radius::LG, color::accent(), 0.08);
            rounded_outline(buf, w, h, z, radius::LG, color::accent(), 0.5);
        }
        mark!("snap");

        for (i, win) in desktop.wins.iter().enumerate() {
            draw_window(buf, w, h, win.rect, win.title, i == desktop.active, ui.mouse, t, tr);
        }
        mark!("windows");

        self.draw_menubar(buf, w, h, ui, tr);
        mark!("menubar");
        if ui.open_menu.is_some() {
            self.draw_dropdown(buf, w, h, ui, tr);
        }
        if let Some((reply, kind, age)) = ui.ai_reply {
            draw_reply(buf, w, h, reply, kind, age, tr);
        }
        self.draw_ai_bar(buf, w, h, ui, t, tr);
        mark!("ai");

        let open_titles: Vec<&str> = desktop.wins.iter().map(|x| x.title).collect();
        self.draw_dock(buf, w, h, &open_titles, ui, tr);
        mark!("dock");

        // 安装向导浮在最上层（Toast 之下）
        if let Some(inst) = &ui.installer {
            self.draw_installer(buf, w, h, inst, ui.mouse, ui.mouse_down, tr);
        }
        mark!("installer");

        // 权限确认是模态：盖在安装向导之上（安装向导的"开始安装"也会走它）
        match &ui.confirm {
            Some(c) => self.draw_confirm(buf, w, h, c, ui.mouse, ui.mouse_down, t, tr),
            None => {
                self.confirm_buttons.clear();
                self.confirm_echo = Rect { x: 0, y: 0, w: 0, h: 0 };
            }
        }
        mark!("confirm");

        if let Some((msg, age)) = ui.toast {
            draw_toast(buf, w, h, msg, age, tr);
        }
        mark!("toast");
        // 计时关闭时最后一次赋值不会被读到，显式消费以免 dead-code 警告
        let _ = tmark;
    }
}

// ---------------------------------------------------------------------------
// 壁纸：低对比柔光渐变
// ---------------------------------------------------------------------------

/// 极光带的横截面：Lorentzian 平方（廉价除法，形态与高斯接近，
/// 但尾部更宽——正是极光边缘自然消散的样子）。
#[inline]
fn band_profile(d: f32) -> f32 {
    let q = 1.0 / (1.0 + d * d);
    q * q
}

/// 确定性颗粒噪点（±2/255 量级）。
/// 作用不是"做旧"，而是消除软件光栅渐变必然出现的色带断层——
/// 没有它，极光在深色底上会出现一圈圈等高线，质感立刻崩。
#[inline]
fn grain(x: usize, y: usize) -> f32 {
    let mut n = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    n ^= n >> 15;
    n = n.wrapping_mul(0x2545_F491);
    n ^= n >> 13;
    ((n & 0x3ff) as f32 / 1023.0 - 0.5) * 4.2
}

/// 一条极光带：翘曲中心线 + 窄横截面 + 沿 x 的柔和包络。
struct AuroraBand {
    rgb: [u8; 3],
    /// 基准中心（屏高比例）
    my: f32,
    /// 翘曲振幅（屏高比例）
    amp: f32,
    /// 沿 x 的波长（单位：屏宽）
    wave: f32,
    phase: f32,
    /// 横截面半宽（屏高比例）——越小带越锐
    sigma: f32,
    inten: f32,
    /// 包络重心与宽度（屏宽比例）
    ex: f32,
    ew: f32,
}

/// 柔光团衰减（1 - 归一化距离）²：明亮壁纸用。
#[inline]
fn wash(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let dx = (x - cx) / rx;
    let dy = (y - cy) / ry;
    (1.0 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0).powi(2)
}

/// 明亮壁纸：近白底 + 四角粉彩柔光团。
///
/// 为什么不用极光带：光带结构在深色底上才读得出来；浅色底上宽光带只会糊成
/// 一片"奶雾"，窄光带又会变成脏色块。浅色的高级感来自**近白底 + 角落极淡的
/// 多色柔光**（Apple 的浅色壁纸正是这套语言）。
fn draw_light_wallpaper(buf: &mut [u32], w: usize, h: usize, t: f32) {
    let (wf, hf) = (w as f32, h as f32);
    let drift = t * 0.02;
    // (色, 中心 x/y 比例, 半径 x/y 比例, 强度)
    let blobs: [([u8; 3], f32, f32, f32, f32, f32); 4] = [
        (color::aurora_cyan(), 0.16, 0.86, 0.60, 0.60, 0.60),
        (color::aurora_violet(), 0.86, 0.84, 0.55, 0.55, 0.55),
        (color::aurora_blue(), 0.06, 0.10, 0.45, 0.45, 0.40),
        (color::aurora_pink(), 0.70, 0.99, 0.45, 0.40, 0.30),
    ];
    let (top, bottom) = (color::bg_top(), color::bg_bottom());
    let vig_k = color::vignette();

    for y in 0..h {
        let vgrad = y as f32 / hf;
        let base = [
            lerp(top[0] as f32, bottom[0] as f32, vgrad),
            lerp(top[1] as f32, bottom[1] as f32, vgrad),
            lerp(top[2] as f32, bottom[2] as f32, vgrad),
        ];
        for x in 0..w {
            let mut acc = base;
            for (rgb, cx, cy, rx, ry, s) in &blobs {
                // 极缓漂移：分钟级呼吸，肉眼几乎察觉不到
                let cx = wf * (cx + 0.008 * (drift + cy * 6.0).sin());
                let cy = hf * (cy + 0.006 * (drift * 0.8 + rx * 9.0).cos());
                let g = wash(x as f32, y as f32, cx, cy, wf * rx, hf * ry) * s;
                if g > 0.002 {
                    for c in 0..3 {
                        acc[c] += (rgb[c] as f32 - acc[c]) * g;
                    }
                }
            }
            let nx = x as f32 / wf - 0.5;
            let ny = y as f32 / hf - 0.5;
            let vig = 1.0 - (nx * nx + ny * ny) * vig_k;
            let n = grain(x, y);
            let px = [
                (acc[0] * vig + n).clamp(0.0, 255.0) as u32,
                (acc[1] * vig + n).clamp(0.0, 255.0) as u32,
                (acc[2] * vig + n).clamp(0.0, 255.0) as u32,
            ];
            buf[y * w + x] = (px[0] << 16) | (px[1] << 8) | px[2];
        }
    }
}

/// 壁纸：深空底 + 结构化极光带 + 颗粒（深色模式），
/// 明亮模式走 [`draw_light_wallpaper`]。
///
/// 与"几个大半径柔光平摊"的区别：柔光平摊出来是一块发灰的脏渐变，
/// 而极光必须是**有走向的光带**——中心线随 x 缓慢翘曲、横截面很窄、
/// 沿 x 有强弱包络。三条带共用同一套漂移时钟，整体像缓慢流动。
fn draw_background(buf: &mut [u32], w: usize, h: usize, t: f32) {
    if color::is_light() {
        draw_light_wallpaper(buf, w, h, t);
        return;
    }
    let (wf, hf) = (w as f32, h as f32);
    // 漂移放缓（§3.4 动效克制）：分钟级呼吸
    let drift = t * 0.03;

    // 青（上，多数被窗口遮住，只在边缘透出）/ 紫（中）/ 蓝紫（下，窗口下沿之外
    // 的主要可见区——壁纸的构图重心必须放在"真正露出来的地方"）。
    // 不在边缘放窄带：屏幕上只露出一窄条时，窄带会读成"色块"而不是极光。
    // 色相与强度随模式（明亮模式是低饱和粉彩，深空模式是发光带）。
    let gain = color::aurora_gain();
    let bands = [
        AuroraBand { rgb: color::aurora_cyan(), my: 0.20, amp: 0.075, wave: 1.30, phase: 0.4, sigma: 0.046, inten: 0.70 * gain, ex: 0.42, ew: 0.44 },
        AuroraBand { rgb: color::aurora_violet(), my: 0.42, amp: 0.100, wave: 0.92, phase: 2.3, sigma: 0.064, inten: 0.55 * gain, ex: 0.60, ew: 0.56 },
        AuroraBand { rgb: color::aurora_blue(), my: 0.78, amp: 0.050, wave: 1.15, phase: 4.1, sigma: 0.062, inten: 0.44 * gain, ex: 0.52, ew: 0.70 },
    ];
    let nb = bands.len();

    // 逐 x 预计算中心线与包络（每像素只剩一次除法的横截面求值）
    let mut centers = vec![0f32; w * nb];
    let mut envelopes = vec![0f32; w * nb];
    for x in 0..w {
        let xn = x as f32 / wf;
        for (k, b) in bands.iter().enumerate() {
            // 双谐波翘曲：主波 + 约 1/3 振幅的次谐波，避免"标准正弦"的机械感
            let ang = (xn * b.wave + b.phase + drift * (1.0 + k as f32 * 0.35)) * std::f32::consts::TAU;
            let warp = ang.sin() + 0.34 * (ang * 2.13 + 1.7).sin();
            centers[k * w + x] = (b.my + b.amp * warp) * hf;
            let u = (xn - b.ex) / b.ew;
            envelopes[k * w + x] = (-u * u).clamp(-9.0, 0.0).exp();
        }
    }

    for y in 0..h {
        let vgrad = y as f32 / hf;
        let ny = vgrad - 0.5;
        let base = [
            lerp(color::bg_top()[0] as f32, color::bg_bottom()[0] as f32, vgrad),
            lerp(color::bg_top()[1] as f32, color::bg_bottom()[1] as f32, vgrad),
            lerp(color::bg_top()[2] as f32, color::bg_bottom()[2] as f32, vgrad),
        ];
        for x in 0..w {
            let mut acc = base;
            for k in 0..nb {
                let b = &bands[k];
                let d = (y as f32 - centers[k * w + x]) / (b.sigma * hf);
                let g = band_profile(d) * envelopes[k * w + x] * b.inten;
                if g > 0.002 {
                    for c in 0..3 {
                        acc[c] += (b.rgb[c] as f32 - acc[c]) * g;
                    }
                }
            }
            // 暗角：留住氛围（明亮模式只留极轻的一圈，重了立刻发灰）
            let nx = x as f32 / wf - 0.5;
            let vig = 1.0 - (nx * nx + ny * ny) * color::vignette();
            let n = grain(x, y);
            let px = [
                (acc[0] * vig + n).clamp(0.0, 255.0) as u32,
                (acc[1] * vig + n).clamp(0.0, 255.0) as u32,
                (acc[2] * vig + n).clamp(0.0, 255.0) as u32,
            ];
            buf[y * w + x] = (px[0] << 16) | (px[1] << 8) | px[2];
        }
    }
}

// ---------------------------------------------------------------------------
// 菜单栏：发丝底 + 品牌 + 菜单（可点击）+ 搜索胶囊 + 电池 + 时钟
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_menubar(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let bar = Rect { x: 0, y: 0, w: w as i32, h: metric::MENUBAR_H };
        fill_rect(buf, w, h, bar, color::inset(), 0.62);
        fill_rect(buf, w, h, Rect { x: 0, y: metric::MENUBAR_H - 1, w: w as i32, h: 1 }, color::hairline(), 0.10);

        let Some(tr) = tr else { return };

        // 品牌：三角徽标（青→紫极光渐变，品牌标识是极光的三个容许出口之一）
        let (bx, by) = (12i32, 8i32);
        for row in 0..16 {
            let half = row / 2;
            let t = row as f32 / 15.0;
            let rgb = [
                lerp(color::accent()[0] as f32, color::accent_violet()[0] as f32, t) as u8,
                lerp(color::accent()[1] as f32, color::accent_violet()[1] as f32, t) as u8,
                lerp(color::accent()[2] as f32, color::accent_violet()[2] as f32, t) as u8,
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
        tr.draw_bold(buf, w, h, brand_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), strings::BRAND, font::BODY, color::text(), 0.98);
        let mut mx = brand_x + tr.measure_bold(strings::BRAND, font::BODY) + 18.0;

        // 菜单标签（登记命中区；悬停/打开用交互态令牌）
        self.menubar_menus.clear();
        for (i, menu) in strings::MENUS.iter().enumerate() {
            let mw = tr.measure(menu, font::BODY);
            let hit = Rect { x: mx as i32 - 8, y: 0, w: mw as i32 + 16, h: metric::MENUBAR_H };
            let hovered = hit.contains(ui.mouse.0, ui.mouse.1);
            let opened = ui.open_menu == Some(i);
            if opened || hovered {
                let a = if opened { state::hover_strong() } else { state::hover() };
                rounded_rect(buf, w, h, Rect { x: hit.x + 2, y: 4, w: hit.w - 4, h: metric::MENUBAR_H - 8 }, radius::SM, color::hairline(), a);
            }
            draw_text(tr, buf, w, h, mx, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), menu, font::BODY, color::text(), if opened { 1.0 } else { 0.92 });
            self.menubar_menus.push(hit);
            mx += mw + 16.0;
        }

        // 右侧：电池、AI 状态、搜索胶囊、时钟
        let clock = crate::text::clock_str();
        let clock_w = tr.measure_bold(&clock, font::BODY);
        let clock_x = w as f32 - 16.0 - clock_w;
        tr.draw_bold(buf, w, h, clock_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), &clock, font::BODY, color::text(), 0.95);

        let pill = Rect { x: clock_x as i32 - 208, y: 5, w: 192, h: 22 };
        let pill_hover = pill.contains(ui.mouse.0, ui.mouse.1);
        rounded_rect(buf, w, h, pill, 11.0, color::hairline(), if pill_hover { state::hover_strong() } else { 0.08 });
        rounded_outline(buf, w, h, pill, 11.0, color::hairline(), if pill_hover { 0.22 } else { 0.14 });
        draw_text(tr, buf, w, h, (pill.x + 12) as f32, tr.vcenter(pill.y as f32, pill.h as f32, font::CAPTION), "搜索", font::CAPTION, color::text_dim(), if pill_hover { 0.95 } else { 0.8 });
        let key = Rect { x: pill.x + pill.w - 22, y: 8, w: 16, h: 16 };
        rounded_rect(buf, w, h, key, 4.0, color::hairline(), 0.12);
        draw_text(tr, buf, w, h, (key.x + 4) as f32, tr.vcenter(key.y as f32, key.h as f32, font::LABEL), "K", font::LABEL, color::text_dim(), 0.85);
        self.search_pill = pill;

        // AI 状态指示（§4 三态：本地青 / 云端紫 / 离线灰）
        let ai_label = ui.ai_status.label();
        let ai_w = tr.measure(ai_label, font::CAPTION);
        let ai_x = pill.x as f32 - 16.0 - ai_w;
        rounded_rect(buf, w, h, Rect { x: ai_x as i32 - 12, y: 13, w: 6, h: 6 }, 3.0, ui.ai_status.color(), 0.95);
        draw_text(tr, buf, w, h, ai_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::CAPTION), ai_label, font::CAPTION, color::text_dim(), 0.85);

        // 电池：状态用中性色（在线/电量语义留给文字，避免与强调色抢注意力）
        let batt = Rect { x: ai_x as i32 - 56, y: 10, w: 26, h: 12 };
        rounded_outline(buf, w, h, batt, 3.5, color::text_dim(), 0.5);
        fill_rect(buf, w, h, Rect { x: batt.x + batt.w, y: 13, w: 2, h: 6 }, color::text_dim(), 0.5);
        fill_rect(buf, w, h, Rect { x: batt.x + 2, y: batt.y + 2, w: 16, h: 8 }, color::text(), 0.5);
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
        shadow(buf, w, h, panel, radius::MD, elevation::elev_2());
        rounded_rect(buf, w, h, panel, radius::MD, color::surface_2(), 0.98);
        rounded_outline(buf, w, h, panel, radius::MD, color::hairline(), 0.12);

        let mut rects = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let ir = Rect { x: panel.x + 4, y: panel.y + 4 + i as i32 * 30, w: panel.w - 8, h: 30 };
            if ir.contains(ui.mouse.0, ui.mouse.1) {
                rounded_rect(buf, w, h, Rect { x: ir.x + 4, y: ir.y + 3, w: ir.w - 8, h: ir.h - 6 }, radius::SM, color::surface_3(), 0.9);
            }
            draw_text(tr, buf, w, h, (ir.x + 16) as f32, tr.vcenter(ir.y as f32, ir.h as f32, font::BODY), item, font::BODY, color::text(), 0.92);
            rects.push(ir);
        }
        self.dropdown = Some((rects, items.to_vec()));
    }
}

// ---------------------------------------------------------------------------
// 窗口：磨砂深灰 + 发丝描边 + 红绿灯（左）+ 居中标题
// ---------------------------------------------------------------------------

/// 窗口几何与激活态的指纹（FNV-1a，无依赖）：变了才重算投影烘焙层。
fn desktop_key(d: &Desktop) -> u64 {
    const FNV: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let push = |h: &mut u64, v: u64| {
        *h ^= v;
        *h = h.wrapping_mul(FNV);
    };
    push(&mut h, d.active as u64);
    push(&mut h, d.wins.len() as u64);
    for w in &d.wins {
        push(&mut h, w.rect.x as u64);
        push(&mut h, w.rect.y as u64);
        push(&mut h, w.rect.w as u64);
        push(&mut h, w.rect.h as u64);
    }
    h
}

fn draw_window(buf: &mut [u32], w: usize, h: usize, r: Rect, title: &str, active: bool, mouse: (f32, f32), t: f32, tr: Option<&TextRenderer>) {
    let body = color::surface_1();
    // 不透明度定得高：合成器没有模糊（backdrop-filter），窗口一旦半透明，
    // 后面窗口的文字就会"透"上来变成鬼影——那比没有玻璃感难看得多。
    let body_alpha = if active { 0.955 } else { 0.90 };
    // 标题栏比主体亮一档——窗口必须有"头"，否则整窗是一块没有层次的灰
    let title_rgb = mix(body, color::hairline(), if active { 0.075 } else { 0.035 });
    let t_stop = metric::TITLE_H as f32 / r.h.max(1) as f32;
    let stops = [
        (0.0, mix(title_rgb, color::hairline(), 0.035)),
        ((t_stop - 0.002).max(0.0), title_rgb),
        (t_stop, body),
        (1.0, mix(body, [0, 0, 0], 0.14)),
    ];

    // 投影不在这里画：render_frame 已把它烘焙进背景层（窗口几何不变时复用），
    // 这里只负责窗口主体本身
    gradient_stops(buf, w, r, radius::LG, &stops, body_alpha);

    // 顶部内高光（玻璃厚度）
    fill_rect(buf, w, h, Rect { x: r.x + 10, y: r.y + 1, w: r.w - 20, h: 1 }, color::HIGHLIGHT, 0.10);
    // 标题栏底部发丝线
    fill_rect(buf, w, h, Rect { x: r.x + 1, y: r.y + metric::TITLE_H, w: r.w - 2, h: 1 }, color::hairline(), 0.10);
    rounded_outline(buf, w, h, r, radius::LG, color::hairline(), if active { 0.22 } else { 0.12 });

    // 红绿灯（左）：直径 10px、间距 7px，悬停时整组显示符号
    let lights = [color::close(), color::min(), color::zoom()];
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
        rounded_rect(buf, w, h, dot, metric::LIGHT_D as f32 / 2.0, *c, if active { 0.95 } else { 0.6 });
        if hovered {
            light_symbol(buf, w, h, lx + metric::LIGHT_D / 2, ly + metric::LIGHT_D / 2, i);
        }
    }

    // 居中标题：激活=纯白，非激活=明确灰阶
    if let Some(tr) = tr {
        let tw = tr.measure_bold(title, font::BODY);
        let tx = r.x as f32 + r.w as f32 / 2.0 - tw / 2.0;
        let ty = tr.vcenter(r.y as f32, metric::TITLE_H as f32, font::BODY);
        if active {
            tr.draw_bold(buf, w, h, tx, ty, title, font::BODY, color::text(), 1.0);
        } else {
            tr.draw_bold(buf, w, h, tx, ty, title, font::BODY, color::text_dim(), 0.75);
        }
    }

    let content = Rect { x: r.x + 1, y: r.y + metric::TITLE_H + 1, w: r.w - 2, h: r.h - metric::TITLE_H - 2 };
    if title == strings::WIN_TERM {
        draw_term_content(buf, w, h, content, r, t, tr);
    } else if title == strings::WIN_MUSIC {
        draw_music_content(buf, w, h, content, r, mouse, tr);
    } else {
        draw_files_content(buf, w, h, content, r, mouse, tr);
    }
}

/// 音乐窗口内容：左侧曲目列表 + 右侧"正在播放"面板。
///
/// 存在的意义是"内容形态跟着窗口语义走"——满窗一模一样的文件夹图标，
/// 是"演示占位"最刺眼的信号。窗口够宽时右挂播放面板，窄时退化为纯列表。
fn draw_music_content(buf: &mut [u32], w: usize, h: usize, r: Rect, win: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>) {
    const ROW_H: i32 = 46;
    const ART: i32 = 30;
    const PANEL_W: i32 = 250;
    const PANEL_MIN_W: i32 = 540;

    let panel_on = r.w >= PANEL_MIN_W;
    let list_w = if panel_on { r.w - PANEL_W - 20 } else { r.w };

    let list = Rect { x: r.x, y: r.y, w: list_w, h: r.h };
    fill_clipped(buf, w, h, list, win, radius::LG, color::surface_2(), 0.5);

    let Some(tr) = tr else { return };
    // 封面底色（循环取用；低饱和，只做区分不做装饰）
    let arts: [([u8; 3], [u8; 3]); 3] = [
        ([96, 150, 196], [48, 88, 138]),
        ([132, 116, 190], [78, 66, 138]),
        ([92, 164, 176], [46, 104, 122]),
    ];

    let mut y = list.y + 8;
    let mut i = 0usize;
    loop {
        // 按可用高度铺满：曲目用完后循环取用（演示内容，不是真实曲库）
        if y + ROW_H > list.y + list.h - 6 {
            break;
        }
        let (title, artist, dur) = strings::TRACKS[i % strings::TRACKS.len()];
        let row = Rect { x: list.x + 8, y, w: list.w - 16, h: ROW_H - 4 };
        let hovered = row.contains(mouse.0, mouse.1);
        let playing = i == 0;
        if hovered || playing {
            fill_clipped(buf, w, h, row, win, radius::LG, color::hairline(), if playing { 0.06 } else { state::hover() });
        }
        if playing {
            rounded_rect(buf, w, h, Rect { x: row.x, y: row.y + 9, w: 2, h: row.h - 18 }, 1.0, color::accent(), 0.95);
        }
        let (at, ab) = arts[i % arts.len()];
        gradient_tile(buf, w, Rect { x: row.x + 12, y: row.y + 5, w: ART, h: ART }, 6.0, at, ab, 0.95);

        let tx = (row.x + 12 + ART + 12) as f32;
        // 给时长让出位置，标题/艺人按剩余宽度截断（窄窗口下不会压到时长上）
        let dur_w = tr.measure(dur, font::LABEL) + 20.0;
        let text_w = (row.x + row.w) as f32 - tx - dur_w;
        let title_s = ellipsize(tr, title, font::BODY, text_w);
        let artist_s = ellipsize(tr, artist, font::LABEL, text_w);
        tr.draw_bold(
            buf, w, h, tx, tr.vcenter(row.y as f32 + 2.0, 20.0, font::BODY),
            &title_s, font::BODY,
            if playing { color::accent() } else { color::text() },
            0.96,
        );
        draw_text(
            tr, buf, w, h, tx, tr.vcenter(row.y as f32 + 22.0, 16.0, font::LABEL),
            &artist_s, font::LABEL, color::text_dim(), 0.92,
        );
        let dw = tr.measure(dur, font::LABEL);
        draw_text(
            tr, buf, w, h, (row.x + row.w - 12) as f32 - dw,
            tr.vcenter(row.y as f32 + 2.0, ROW_H as f32 - 4.0, font::LABEL),
            dur, font::LABEL, color::text_faint(), 0.85,
        );
        y += ROW_H;
        i += 1;
    }

    if !panel_on {
        return;
    }

    // 右侧"正在播放"面板：大封面 + 曲目信息 + 进度 + 传输控件
    let panel = Rect { x: r.x + r.w - PANEL_W + 2, y: r.y, w: PANEL_W - 2, h: r.h };
    fill_clipped(buf, w, h, panel, win, radius::LG, color::inset(), 0.62);
    fill_rect(buf, w, h, Rect { x: panel.x, y: r.y + 1, w: 1, h: r.h - 2 }, color::hairline(), 0.08);

    let cover_s = (PANEL_W - 76).min(r.h / 2);
    // 面板小标题
    let lab_w = tr.measure(strings::NOW_PLAYING, font::LABEL);
    draw_text(
        tr, buf, w, h,
        panel.x as f32 + (panel.w as f32 - lab_w) / 2.0,
        tr.vcenter(panel.y as f32 + 6.0, 18.0, font::LABEL),
        strings::NOW_PLAYING, font::LABEL, color::text_faint(), 0.9,
    );
    let cover = Rect {
        x: panel.x + (panel.w - cover_s) / 2,
        y: panel.y + 30,
        w: cover_s,
        h: cover_s,
    };
    shadow(buf, w, h, cover, radius::MD, elevation::elev_1());
    gradient_tile(buf, w, cover, radius::MD, [104, 178, 226], [56, 104, 176], 0.98);
    // 封面上的装饰：两道弧形光带（纯几何，呼应品牌极光）
    for k in 0..2 {
        let off = k as i32 * 18;
        for row in 0..cover_s {
            let t = row as f32 / cover_s as f32;
            let bend = ((t * 3.0 + k as f32 * 0.7).sin() * 0.5 + 0.5) * (cover_s as f32 * 0.22);
            let x = cover.x + (cover_s / 6) + bend as i32 + off;
            fill_rect(buf, w, h, Rect { x, y: cover.y + row, w: cover_s / 3, h: 1 }, color::HIGHLIGHT, 0.10);
        }
    }
    fill_rect(buf, w, h, Rect { x: cover.x + 6, y: cover.y + 1, w: cover_s - 12, h: 1 }, color::HIGHLIGHT, 0.22);

    let cx = panel.x as f32 + panel.w as f32 / 2.0;
    let mut ty = (cover.y + cover_s + 22) as f32;
    let t0 = strings::TRACKS[0].0;
    let tw = tr.measure_bold(t0, font::BODY);
    tr.draw_bold(buf, w, h, cx - tw / 2.0, ty, t0, font::BODY, color::text(), 0.98);
    ty += 22.0;
    let a0 = strings::TRACKS[0].1;
    let aw = tr.measure(a0, font::LABEL);
    draw_text(tr, buf, w, h, cx - aw / 2.0, ty, a0, font::LABEL, color::text_dim(), 0.88);

    // 进度条 + 时间（面板底部）
    let bar_w = panel.w - 48;
    let bx = panel.x + 24;
    let by = panel.y + panel.h - 58;
    rounded_rect(buf, w, h, Rect { x: bx, y: by, w: bar_w, h: 4 }, 2.0, color::hairline(), 0.14);
    rounded_rect(buf, w, h, Rect { x: bx, y: by, w: (bar_w as f32 * 0.35) as i32, h: 4 }, 2.0, color::accent(), 0.92);
    draw_text(tr, buf, w, h, bx as f32, (by + 12) as f32, "1:28", font::LABEL, color::text_faint(), 0.9);
    let t1 = strings::TRACKS[0].2;
    let t1w = tr.measure(t1, font::LABEL);
    draw_text(tr, buf, w, h, (bx + bar_w) as f32 - t1w, (by + 12) as f32, t1, font::LABEL, color::text_faint(), 0.9);

    // 传输控件：上一个 / 播放 / 下一个（纯几何）
    let cy = panel.y + panel.h - 26;
    let mid = panel.x + panel.w / 2;
    rounded_rect(buf, w, h, Rect { x: mid - 14, y: cy - 14, w: 28, h: 28 }, 14.0, color::accent(), 0.92);
    // 暂停符号（两条竖杠）
    fill_rect(buf, w, h, Rect { x: mid - 5, y: cy - 6, w: 3, h: 12 }, color::text(), 0.95);
    fill_rect(buf, w, h, Rect { x: mid + 2, y: cy - 6, w: 3, h: 12 }, color::text(), 0.95);
    arrow_glyph(buf, w, h, mid - 42, cy, 5, -1, color::text_dim(), 0.9);
    arrow_glyph(buf, w, h, mid + 40, cy, 5, 1, color::text_dim(), 0.9);
}

/// 三角箭头（上一个/下一个）：按列扫描填充，纯几何不依赖字体字形。
/// `dir` = -1 左指 / +1 右指；`cx` 为三角尖端所在列。
fn arrow_glyph(buf: &mut [u32], w: usize, h: usize, cx: i32, cy: i32, size: i32, dir: i32, rgb: [u8; 3], alpha: f32) {
    for k in 0..size {
        let hh = size - k;
        let x = if dir > 0 { cx - k } else { cx + k };
        fill_rect(buf, w, h, Rect { x, y: cy - hh, w: 1, h: hh * 2 + 1 }, rgb, alpha);
    }
}

/// 窗控符号（悬停时显示）：纯几何绘制，不依赖字体字形覆盖。
fn light_symbol(buf: &mut [u32], w: usize, h: usize, cx: i32, cy: i32, kind: usize) {
    const S: i32 = 2; // 半臂长 → 符号跨度 5px（10px 圆内，Step4 收细）
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

/// 终端内容：提示符分色 + 命令与输出 + 末尾光标。
/// 密度做足——空荡的终端窗口看起来像没做完。
fn draw_term_content(buf: &mut [u32], w: usize, h: usize, r: Rect, win: Rect, t: f32, tr: Option<&TextRenderer>) {
    fill_clipped(buf, w, h, r, win, radius::LG, color::inset(), 0.95);
    let Some(tr) = tr else { return };

    // 每行是若干 (文本, 颜色, 不透明度) 段；提示符分色是"真终端"的视觉签名。
    // 行数给足并**按可用高度裁切**——窗口矮时不会画出窗口外，窗口高时不留死灰。
    let host = ("aether@localhost", color::accent(), 0.95);
    let sep = (" ~ $ ", color::text_dim(), 0.85);
    let lines: [&[(&str, [u8; 3], f32)]; 9] = [
        &[host, sep, ("uname -a", color::text(), 0.95)],
        &[("AetherOS 0.1.0 aether-kernel x86_64 GNU/Linux", color::text_dim(), 0.9)],
        &[host, sep, ("aether-status", color::text(), 0.95)],
        &[("服务 5/5 运行中 · AI 中枢在线 · 已开机 00:07:12", color::success(), 0.85)],
        &[host, sep, ("cat /etc/aether/services/aetherd.json", color::text(), 0.95)],
        &[("name=aetherd  after=network  restart=true  essential=true", color::text_dim(), 0.9)],
        &[host, sep, ("aether-ipc --probe 7311", color::text(), 0.95)],
        &[("7311 在线 · NDJSON 协议 · 本地模型 llama3.2:3b", color::text_dim(), 0.9)],
        &[host, sep],
    ];
    let mut y = r.y + 14;
    let mut caret_x = r.x + 16;
    let bottom = r.y + r.h - 10;
    let right = r.x + r.w - 14;
    for (i, segs) in lines.iter().enumerate() {
        if y + 20 > bottom {
            break;
        }
        let mut x = (r.x + 16) as f32;
        for (text, rgb, a) in segs.iter() {
            // 逐段裁切：文本绘制没有横向裁剪，不截断就会画到窗口外面去
            let room = right as f32 - x;
            if room < 8.0 {
                break;
            }
            if tr.measure(text, font::MONO) > room {
                let cut = ellipsize(tr, text, font::MONO, room);
                x = draw_text(tr, buf, w, h, x, y as f32, &cut, font::MONO, *rgb, *a);
                break;
            }
            x = draw_text(tr, buf, w, h, x, y as f32, text, font::MONO, *rgb, *a);
        }
        if i + 1 == lines.len() {
            caret_x = x as i32;
        }
        y += 22;
    }
    if (t * 2.0) as i32 % 2 == 0 && y - 18 + 15 <= bottom && caret_x + 8 <= right {
        fill_rect(buf, w, h, Rect { x: caret_x, y: y - 18, w: 8, h: 15 }, color::text(), 0.72);
    }
}

/// 文件窗口内容：侧栏（深底座）→ 内容面（纸面）→ 状态条（深底座）三层。
fn draw_files_content(buf: &mut [u32], w: usize, h: usize, r: Rect, win: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>) {
    const SIDEBAR_W: i32 = 150;
    const STATUS_H: i32 = 26;

    // 侧栏：比窗口体暗一档
    let sidebar = Rect { x: r.x, y: r.y, w: SIDEBAR_W.min(r.w / 2), h: r.h };
    fill_clipped(buf, w, h, sidebar, win, radius::LG, color::inset(), 0.66);

    // 内容面：比窗口体亮半档的"纸面"——三层明度差是纵深感的全部来源
    let area_x = sidebar.x + sidebar.w;
    let surface = Rect { x: area_x, y: r.y, w: r.x + r.w - area_x, h: r.h - STATUS_H };
    fill_clipped(buf, w, h, surface, win, radius::LG, color::surface_2(), 0.42);
    fill_rect(buf, w, h, Rect { x: area_x, y: r.y + 1, w: 1, h: r.h - 2 }, color::hairline(), 0.08);

    if let Some(tr) = tr {
        for (i, item) in strings::SIDEBAR_ITEMS.iter().enumerate() {
            let y = r.y + 12 + i as i32 * 30;
            let selected = i == 0;
            let row = Rect { x: r.x + 8, y, w: sidebar.w - 16, h: 26 };
            if selected {
                rounded_rect(buf, w, h, row, radius::SM - 2.0, color::accent(), 0.20);
                // 选中项左侧标记条：比整块底色更克制
                rounded_rect(buf, w, h, Rect { x: row.x, y: row.y + 6, w: 2, h: 14 }, 1.0, color::accent(), 0.95);
            } else if row.contains(mouse.0, mouse.1) {
                rounded_rect(buf, w, h, row, radius::SM - 2.0, color::hairline(), state::hover());
            }
            draw_text(
                tr, buf, w, h,
                (row.x + 14) as f32,
                tr.vcenter(row.y as f32, row.h as f32, font::BODY),
                item, font::BODY,
                if selected { color::text() } else { color::text_dim() },
                if selected { 0.96 } else { 0.88 },
            );
        }
    }

    let shown = draw_icon_grid(buf, w, h, surface, win, mouse, tr);

    // 状态条：贴窗口底角
    let status = Rect { x: r.x, y: r.y + r.h - STATUS_H, w: r.w, h: STATUS_H };
    fill_clipped(buf, w, h, status, win, radius::LG, color::inset(), 0.62);
    fill_rect(buf, w, h, Rect { x: r.x + 1, y: status.y, w: r.w - 2, h: 1 }, color::hairline(), 0.08);
    if let Some(tr) = tr {
        // 数量取"实际画出来的"，不写死——写死立刻和画面矛盾
        let left = format!("{shown} 个项目");
        draw_text(tr, buf, w, h, (status.x + 12) as f32, tr.vcenter(status.y as f32, status.h as f32, font::LABEL), &left, font::LABEL, color::text_faint(), 0.9);
        let right = strings::DISK_FREE;
        let rw = tr.measure(right, font::LABEL);
        draw_text(tr, buf, w, h, (status.x + status.w - 12) as f32 - rw, tr.vcenter(status.y as f32, status.h as f32, font::LABEL), right, font::LABEL, color::text_faint(), 0.9);
    }
}

/// 图标网格：图标 + 标签（无卡片外框——"框里再放块"是廉价感的来源之一）。
/// 行列数按可用空间自适应，把窗口填满，不留下大片死灰。返回实际画出的项数。
fn draw_icon_grid(buf: &mut [u32], w: usize, h: usize, area: Rect, win: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>) -> usize {
    let Some(tr) = tr else { return 0 };
    const ICON: i32 = 46;
    const CELL_W: i32 = 96;
    const CELL_H: i32 = 84;
    const GAP_X: i32 = 10;
    const GAP_Y: i32 = 12;
    const PAD: i32 = 14;

    let avail_w = area.w - PAD * 2;
    let avail_h = area.h - PAD * 2;
    if avail_w < CELL_W || avail_h < CELL_H {
        return 0;
    }
    let cols = (avail_w / (CELL_W + GAP_X)).clamp(1, 5);
    let rows = (avail_h / (CELL_H + GAP_Y)).clamp(1, 5);
    let n = ((cols * rows) as usize).min(strings::FILE_NAMES.len());
    let grid_w = cols * CELL_W + (cols - 1) * GAP_X;
    let grid_h = rows * CELL_H + (rows - 1) * GAP_Y;
    let ox = area.x + (area.w - grid_w) / 2;
    let oy = area.y + (area.h - grid_h) / 2;

    for i in 0..n {
        let col = i as i32 % cols;
        let row = i as i32 / cols;
        let cell = Rect {
            x: ox + col * (CELL_W + GAP_X),
            y: oy + row * (CELL_H + GAP_Y),
            w: CELL_W,
            h: CELL_H,
        };
        let hovered = cell.contains(mouse.0, mouse.1);
        if hovered {
            fill_clipped(buf, w, h, cell, win, radius::LG, color::hairline(), state::hover());
        }
        let icon_x = cell.x + (CELL_W - ICON) / 2;
        let icon_y = cell.y + 8;
        draw_folder_icon(buf, w, h, icon_x, icon_y, ICON);

        let label_y = icon_y + ICON + 4;
        let shown = ellipsize(tr, strings::FILE_NAMES[i], font::LABEL, (CELL_W - 10) as f32);
        let lw = tr.measure(&shown, font::LABEL);
        draw_text(
            tr, buf, w, h,
            cell.x as f32 + (CELL_W as f32 - lw) / 2.0,
            tr.vcenter(label_y as f32, 14.0, font::LABEL),
            &shown, font::LABEL,
            if hovered { color::text() } else { color::text_dim() },
            if hovered { 0.98 } else { 0.94 },
        );
    }
    n
}

/// 文件夹图标：图形本身即图标（不套底色块），渐变填充 + 页签 + 顶部高光。
/// 饱和度刻意压低——一屏几十个图标，每个都艳就是灾难。
fn draw_folder_icon(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, size: i32) {
    let top = [104, 162, 202];
    let bottom = [50, 96, 144];
    let tab_h = (size as f32 * 0.20) as i32;
    let tab_w = (size as f32 * 0.42) as i32;
    // 页签先画，主体盖住它的下缘（避免半透明叠加出接缝）
    rounded_rect(buf, w, h, Rect { x, y, w: tab_w, h: tab_h + 8 }, 3.0, mix(top, bottom, 0.40), 0.95);
    gradient_tile(buf, w, Rect { x, y: y + tab_h, w: size, h: size - tab_h }, 4.0, top, bottom, 0.97);
    fill_rect(buf, w, h, Rect { x: x + 3, y: y + tab_h + 1, w: size - 6, h: 1 }, color::HIGHLIGHT, 0.20);
    fill_rect(buf, w, h, Rect { x: x + 3, y: y + size - 2, w: size - 6, h: 1 }, [0, 0, 0], 0.12);
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
    shadow(buf, w, h, bar, radius::MD, elevation::elev_2());
    rounded_rect(buf, w, h, bar, 26.0, color::glass(), color::glass_alpha(hovered));
    if ui.ai_focused {
        // 焦点态：青紫渐变环（AI 元素的极光配额）；思考中叠一层呼吸脉动
        let breath = if ui.ai_thinking { 0.55 + 0.45 * (t * 3.0).sin() } else { 1.0 };
        gradient_outline(buf, w, h, bar, 26.0, color::accent(), color::accent_violet(), 0.75 * breath);
    } else {
        rounded_outline(buf, w, h, bar, 26.0, color::hairline(), if hovered { 0.22 } else { 0.14 });
    }

    let Some(tr) = tr else { return };

    // 渐变圆形徽标（AI 元素：极光的容许出口之一）
    let av = Rect { x: bar.x + 12, y: bar.y + 10, w: 32, h: 32 };
    gradient_tile(buf, w, av, 16.0, color::accent(), color::accent_violet(), 0.95);
    let aw = tr.measure_bold("A", font::GLYPH);
    tr.draw_bold(buf, w, h, av.x as f32 + (av.w as f32 - aw) / 2.0, tr.vcenter(av.y as f32, av.h as f32, font::GLYPH), "A", font::GLYPH, color::text(), 0.98);

    let text_x = (bar.x + 58) as f32;
    let text_y = tr.vcenter(bar.y as f32, bar.h as f32, font::BODY);
    if input.is_empty() {
        draw_text(tr, buf, w, h, text_x, text_y, strings::AI_BAR_HINT, font::BODY, color::text_dim(), 0.85);
    } else {
        // 从左截断，保证光标所在的内容始终可见
        let max_w = bar.x as f32 + bar.w as f32 - 96.0 - text_x;
        let mut shown: String = input.to_string();
        while tr.measure(&shown, font::BODY) > max_w && shown.chars().count() > 1 {
            shown.remove(0);
        }
        draw_text(tr, buf, w, h, text_x, text_y, &shown, font::BODY, color::text(), 0.95);
    }

    // 输入光标
    let caret_x = text_x + tr.measure(if input.is_empty() { "" } else { input_tail_visible(tr, input, text_x, bar.x as f32 + bar.w as f32 - 96.0) }, font::BODY) + 4.0;
    if ui.ai_thinking {
        // 思考中：三点呼吸，明确"在等我"而不是"在等你打字"
        for i in 0..3 {
            let phase = (t * 3.0 - i as f32 * 0.5).sin() * 0.5 + 0.5;
            let a = 0.25 + 0.6 * phase;
            let cx = caret_x as i32 + i * 8;
            rounded_rect(buf, w, h, Rect { x: cx, y: bar.y + 23, w: 5, h: 5 }, 2.5, color::accent(), a);
        }
    } else if ((t_now() * 2.0) as i32) % 2 == 0 {
        fill_rect(buf, w, h, Rect { x: caret_x as i32, y: bar.y + 16, w: 2, h: 20 }, color::accent(), 0.9);
    }

    // 右侧快捷键胶囊
    let hint = "Enter";
    let hw = tr.measure(hint, font::LABEL) + 12.0;
    let key = Rect { x: bar.x + bar.w - hw as i32 - 12, y: bar.y + 15, w: hw as i32, h: 22 };
    rounded_rect(buf, w, h, key, 6.0, color::hairline(), 0.10);
    draw_text(tr, buf, w, h, (key.x + 6) as f32, tr.vcenter(key.y as f32, key.h as f32, font::LABEL), hint, font::LABEL, color::text_dim(), 0.8);
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
    shadow(buf, w, h, r, radius::MD, alpha * elevation::elev_2());
    let bg = match kind {
        BubbleKind::User => color::surface_3(),
        _ => color::surface_2(),
    };
    rounded_rect(buf, w, h, r, 18.0, bg, alpha * 0.94);
    match kind {
        // AI 回复：青紫渐变环（AI 元素）
        BubbleKind::Ai => gradient_outline(buf, w, h, r, 18.0, color::accent(), color::accent_violet(), alpha * 0.55),
        BubbleKind::User => rounded_outline(buf, w, h, r, 18.0, color::hairline(), alpha * 0.16),
        // 工具调用：左侧状态条（成功绿/失败红），由文案前缀决定
        BubbleKind::Tool => {
            rounded_outline(buf, w, h, r, 18.0, color::hairline(), alpha * 0.12);
            let ok = !text.starts_with('✗');
            let bar_rgb = if ok { color::success() } else { color::danger() };
            rounded_rect(buf, w, h, Rect { x: r.x + 10, y: r.y + 9, w: 4, h: r.h - 18 }, 2.0, bar_rgb, alpha * 0.95);
        }
    }
    // 保留开头 + 省略号：消息的关键信息在句首，从左删字符等于把内容读反了
    // ——用户会看到结尾、看不到"要干什么"，而且没有任何"被截断"的提示（P3-23）
    let shown = ellipsize(tr, text, font::BODY, max_w);
    let tx = match kind {
        BubbleKind::User => tr.measure(&shown, font::BODY) + pad,
        BubbleKind::Tool => tr.measure(&shown, font::BODY) + pad + 6.0,
        BubbleKind::Ai => pad,
    };
    draw_text(tr, buf, w, h, r.x as f32 + tx, tr.vcenter(r.y as f32, r.h as f32, font::BODY), &shown, font::BODY, color::text(), alpha);
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
        shadow(buf, w, h, tray, radius::MD, elevation::elev_2());
        // 走 glass_alpha()：明亮模式必须够白（0.68 会让粉彩壁纸透上来发脏，
        // 与文档声称的"悬浮层 0.95"也不符），深色模式则是比窗口更暗的底座（P3-27）
        rounded_rect(
            buf,
            w,
            h,
            tray,
            20.0,
            color::glass(),
            color::glass_alpha(tray.contains(ui.mouse.0, ui.mouse.1)),
        );
        rounded_outline(buf, w, h, tray, 20.0, color::hairline(), 0.14);
        // 托盘顶部内高光（玻璃厚度）
        fill_rect(buf, w, h, Rect { x: tray.x + 16, y: tray.y + 1, w: tray.w - 32, h: 1 }, color::HIGHLIGHT, 0.10);

        self.dock_icons.clear();
        for (i, name) in apps.iter().enumerate() {
            let ix = tray.x + metric::DOCK_PAD + i as i32 * (metric::DOCK_ICON + metric::DOCK_PAD);
            let iy = tray.y + metric::DOCK_PAD;
            let tile = Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON };
            self.dock_icons.push(tile);
            let hovered = tile.contains(ui.mouse.0, ui.mouse.1);
            let running = open_titles.contains(name);

            if *name == strings::INSTALLER {
                // 安装是 Live ISO 里唯一需要被一眼找到的动作：琥珀警示色
                gradient_tile(buf, w, Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON }, radius::MD, [236, 180, 90], [156, 104, 44], if hovered { 1.0 } else { 0.92 });
            } else {
                // 图标底座：上亮下暗（"自上方受光"），与文件夹图标同一光照语言。
                // 上暗下亮会读成"压扁的按钮"，上亮下暗才读成"立体的图标"。
                let base = match i {
                    0 => ([126, 200, 232], [52, 116, 172]), // 文件：青蓝
                    1 => ([96, 142, 196], [42, 66, 110]),   // 终端：钢青
                    2 => ([100, 172, 244], [44, 98, 192]),  // 浏览器：蓝
                    3 => ([180, 136, 234], [108, 70, 178]), // 音乐：紫
                    _ => ([150, 160, 182], [82, 90, 112]),  // 设置：蓝灰
                };
                gradient_tile(buf, w, Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON }, radius::MD, base.0, base.1, 0.97);
                // 顶部内高光（与窗口同一手法）
                fill_rect(buf, w, h, Rect { x: ix + 4, y: iy + 1, w: metric::DOCK_ICON - 8, h: 1 }, color::HIGHLIGHT, 0.24);
                if hovered {
                    rounded_rect(buf, w, h, tile, radius::MD, color::hairline(), 0.14);
                }
                if running {
                    // 运行中：青色微光（状态反馈，不是装饰）
                    rounded_rect(buf, w, h, tile, radius::MD, color::accent(), 0.10);
                }
            }
            rounded_outline(buf, w, h, tile, radius::MD, color::hairline(), if hovered { 0.30 } else { 0.18 });

            // 白色几何符号（全部纯 rect 绘制，不依赖字体字形覆盖）
            let sym = color::HIGHLIGHT;
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
                rounded_rect(buf, w, h, Rect { x: ix + metric::DOCK_ICON / 2 - 1, y: iy + metric::DOCK_ICON + 5, w: 3, h: 3 }, 1.5, color::accent(), 0.9);
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
        shadow(buf, w, h, win, radius::LG, elevation::elev_3());
        rounded_rect(buf, w, h, win, radius::LG, color::surface_2(), 0.98);
        rounded_outline(buf, w, h, win, radius::LG, color::hairline(), 0.16);
        let Some(tr) = tr else { return };

        // 标题栏（TITLE 20px 在 36px 栏内垂直居中）
        tr.draw_bold(buf, w, h, (win.x + 18) as f32, tr.vcenter(win.y as f32, metric::TITLE_H as f32, font::TITLE), "安装 AetherOS", font::TITLE, color::text(), 0.95);
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + metric::TITLE_H, w: win.w - 2, h: 1 }, color::hairline(), 0.08);

        // 提示行
        draw_text(tr, buf, w, h, (win.x + 18) as f32, (win.y + 52) as f32,
                  "选择目标磁盘并确认。整盘覆盖，目标盘上的数据将丢失。", font::CAPTION, color::text_dim(), 0.9);

        // 磁盘行
        self.installer_rows.clear();
        let row_y0 = win.y + 78;
        for (i, (dev, mb)) in inst.disks.iter().enumerate() {
            let r = Rect { x: win.x + 16, y: row_y0 + i as i32 * 50, w: win.w - 32, h: 44 };
            if inst.selected == i {
                rounded_rect(buf, w, h, r, radius::MD, color::accent(), 0.16);
                rounded_outline(buf, w, h, r, radius::MD, color::accent(), 0.45);
                rounded_rect(buf, w, h, Rect { x: r.x + 12, y: r.y + 16, w: 12, h: 12 }, 6.0, color::accent(), 0.95);
            } else {
                rounded_rect(buf, w, h, r, radius::MD, color::hairline(), 0.05);
                rounded_outline(buf, w, h, r, radius::MD, color::hairline(), 0.10);
                rounded_outline(buf, w, h, Rect { x: r.x + 11, y: r.y + 15, w: 14, h: 14 }, 7.0, color::text_dim(), 0.7);
            }
            tr.draw_bold(buf, w, h, (r.x + 36) as f32, tr.vcenter(r.y as f32, r.h as f32, font::BODY), dev, font::BODY, color::text(), 0.92);
            let sz = format!("{mb} MB 可用");
            let sw = tr.measure(&sz, font::CAPTION);
            draw_text(tr, buf, w, h, (r.x + r.w - 16) as f32 - sw, tr.vcenter(r.y as f32, r.h as f32, font::CAPTION), &sz, font::CAPTION, color::text_dim(), 0.8);
            self.installer_rows.push((r, dev.clone(), *mb));
        }

        // 阶段信息行
        let msg_y = (win.y + win_h - 104) as f32;
        match (inst.phase, inst.message) {
            (InstallerPhase::Running, _) => {
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, "正在写入磁盘，请勿关机…", font::BODY, color::accent(), 0.95);
            }
            (InstallerPhase::Done, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, &shown, font::CAPTION, color::success(), 0.95);
            }
            (InstallerPhase::Failed, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, &shown, font::CAPTION, color::danger(), 0.95);
            }
            _ => {
                draw_text(tr, buf, w, h, (win.x + 18) as f32, msg_y, "就绪。也可以选中磁盘后按 Enter 开始。", font::CAPTION, color::text_dim(), 0.85);
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
            // 与确认弹窗一致：accent() 直接做底 + text() 做字实测只有深色 2.07:1，
            // 读不清"我要开始擦盘了"这个按钮。改用加深的 accent_strong + 纯白字。
            rounded_rect(buf, w, h, button, radius::SM, color::accent_strong(), 0.95);
            if hovered {
                rounded_rect(buf, w, h, button, radius::SM, color::hairline(), state::hover());
            }
            if pressed {
                // 按下是压暗（叠黑），不是降低底色——后者会透出背景显得像变淡
                rounded_rect(buf, w, h, button, radius::SM, [0, 0, 0], state::pressed());
            }
        } else if matches!(inst.phase, InstallerPhase::Done) {
            rounded_rect(buf, w, h, button, radius::SM, color::success(), 0.85);
        } else {
            rounded_rect(buf, w, h, button, radius::SM, color::surface_3(), 0.9);
        }
        let (label_rgb, label_a) = if enabled {
            (color::HIGHLIGHT, 0.98)
        } else if matches!(inst.phase, InstallerPhase::Done) {
            // success 底在明亮模式下是深绿，配白字约 4.3:1，比原来的 text() 好得多
            (color::HIGHLIGHT, 0.95)
        } else {
            (color::text_faint(), state::DISABLED)
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
                // 头尾都留：只留头部时，"良性前缀 + 第 25 字符起的恶意值"会完整
                // 躲过用户的视线。截断处用省略号明确标出"这里被砍过"（P2-18）。
                let shown = if v.chars().count() > 34 {
                    let head: String = v.chars().take(20).collect();
                    let tail: String = {
                        let rev: Vec<char> = v.chars().rev().take(10).collect();
                        rev.into_iter().rev().collect()
                    };
                    format!("{head}…{tail}")
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
        fill_rect(buf, w, h, Rect { x: 0, y: 0, w: w as i32, h: h as i32 }, color::scrim(), color::scrim_alpha());
        shadow(buf, w, h, win, radius::LG, elevation::elev_3());
        rounded_rect(buf, w, h, win, radius::LG, color::surface_2(), 0.99);
        rounded_outline(buf, w, h, win, radius::LG, color::hairline(), 0.18);
        let Some(tr) = tr else { return };

        // 标题行：等级徽章（L2 黄 / L3 红）+ 操作名
        let badge_rgb = c.badge_color();
        let badge_text_rgb = c.badge_text_color();
        let badge_text = format!("L{} {}", c.level, if c.level >= 3 { "危险" } else { "敏感写" });
        let bw = tr.measure_bold(&badge_text, font::LABEL) + 18.0;
        let badge = Rect { x: win.x + 20, y: win.y + 20, w: bw as i32, h: 22 };
        // 底色压到 0.12：再浓就会把文字对比度拖到 AA 以下（P2-20 同类）
        rounded_rect(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.12);
        rounded_outline(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.7);
        draw_text(tr, buf, w, h, (badge.x + 9) as f32, tr.vcenter(badge.y as f32, badge.h as f32, font::LABEL), &badge_text, font::LABEL, badge_text_rgb, 1.0);
        tr.draw_bold(
            buf, w, h,
            (badge.x + badge.w + 12) as f32,
            tr.vcenter(win.y as f32 + 18.0, 26.0, 17.0),
            tool_label(c.tool), 17.0, color::text(), 0.96,
        );
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + 58, w: win.w - 2, h: 1 }, color::hairline(), 0.08);

        // 参数明文（逐项原样展示：确认的前提是看清对象）
        let mut y = win.y + 74;
        draw_text(tr, buf, w, h, (win.x + 20) as f32, tr.vcenter(y as f32, 20.0, font::CAPTION), "工具", font::CAPTION, color::text_faint(), 0.9);
        draw_text(tr, buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, 20.0, font::BODY), c.tool, font::BODY, color::text_dim(), 0.95);
        y += 22;
        for (k, v) in &args {
            draw_text(tr, buf, w, h, (win.x + 20) as f32, tr.vcenter(y as f32, 20.0, font::CAPTION), k, font::CAPTION, color::text_faint(), 0.9);
            tr.draw_bold(buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, 20.0, font::BODY), v, font::BODY, color::text(), 0.96);
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
        // 保留开头 + 省略号：后果说明的关键信息在句首（"整盘覆盖写入…不可恢复"），
        // 从尾部静默截断会把最要紧的半句吃掉（P3-23）。
        // 文字用主文字色：彩色文字在淡底上达不到 AA；警示语义由左侧三角与徽章承担。
        let shown = ellipsize(tr, c.consequence, font::CAPTION, (win.w - 64) as f32);
        draw_text(tr, buf, w, h, (win.x + 42) as f32, tr.vcenter(y as f32, 20.0, font::CAPTION), &shown, font::CAPTION, color::text(), 0.95);
        y += 34;

        // L3 回显确认：必须原样输入目标，防"手滑点确认"
        if let Some(target) = c.echo_required {
            let tip = format!("L3 危险操作：请原样输入 {target} 以确认");
            draw_text(tr, buf, w, h, (win.x + 20) as f32, y as f32, &tip, font::CAPTION, color::text_dim(), 0.9);
            y += 22;
            let field = Rect { x: win.x + 20, y, w: win.w - 40, h: 38 };
            let ok = c.echo_ok();
            rounded_rect(buf, w, h, field, radius::SM, color::inset(), 0.9);
            rounded_outline(buf, w, h, field, radius::SM, if ok { color::success() } else { color::hairline() }, if ok { 0.75 } else { 0.18 });
            let fty = tr.vcenter(field.y as f32, field.h as f32, font::BODY);
            if c.echo_input.is_empty() {
                let hint = format!("输入 {target}");
                draw_text(tr, buf, w, h, (field.x + 12) as f32, fty, &hint, font::BODY, color::text_faint(), 0.8);
            } else {
                draw_text(tr, buf, w, h, (field.x + 12) as f32, fty, c.echo_input, font::BODY, color::text(), 0.96);
            }
            let tw = tr.measure(c.echo_input, font::BODY);
            if ((t * 2.0) as i32) % 2 == 0 {
                fill_rect(buf, w, h, Rect { x: (field.x + 12) as i32 + tw as i32, y: field.y + 10, w: 2, h: 18 }, color::accent(), 0.9);
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

        // 拒绝按钮：透明底 + 高对比红字。悬停只加轻填充（0.14 的填充会把
        // 文字对比度压到 4.1:1 以下），按下才给更明显的反馈。
        let deny_hover = deny.contains(mouse.0, mouse.1);
        let deny_fill = if deny_hover && mouse_down { 0.14 } else { 0.0 };
        if deny_fill > 0.0 {
            rounded_rect(buf, w, h, deny, radius::SM, color::danger(), deny_fill);
        }
        rounded_outline(buf, w, h, deny, radius::SM, color::danger(), if deny_hover { 0.85 } else { 0.6 });
        let dw = tr.measure_bold("拒绝", font::BODY);
        tr.draw_bold(buf, w, h, (deny.x + deny.w / 2) as f32 - dw / 2.0, tr.vcenter(deny.y as f32, deny.h as f32, font::BODY), "拒绝", font::BODY, color::danger_text(), 0.98);

        let ready = c.echo_ok();
        let allow_hover = ready && allow.contains(mouse.0, mouse.1);
        if ready {
            // 底色用 accent_strong：直接拿 accent 做底、text 做字实测只有
            // 深色 2.07:1 / 明亮 4.11:1，用户在最需要看清的"放行"按钮上读不清字
            rounded_rect(buf, w, h, allow, radius::SM, color::accent_strong(), 0.95);
            if allow_hover {
                rounded_rect(buf, w, h, allow, radius::SM, color::hairline(), state::hover());
                if mouse_down {
                    rounded_rect(buf, w, h, allow, radius::SM, [0, 0, 0], state::pressed());
                }
            }
        } else {
            rounded_rect(buf, w, h, allow, radius::SM, color::surface_3(), 0.9);
        }
        // 实心底上用纯白：这是"放行"按钮，必须一眼读清
        let (lrgb, la) = if ready { (color::HIGHLIGHT, 0.98) } else { (color::text_faint(), state::DISABLED) };
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
    shadow(buf, w, h, r, radius::SM, alpha * elevation::elev_2());
    rounded_rect(buf, w, h, r, 20.0, color::surface_2(), alpha * 0.92);
    rounded_outline(buf, w, h, r, 20.0, color::hairline(), alpha * 0.15);
    fill_rect(buf, w, h, Rect { x: r.x + 18, y: r.y + 17, w: 6, h: 6 }, color::accent(), alpha);
    draw_text(tr, buf, w, h, (r.x + 34) as f32, tr.vcenter(r.y as f32, r.h as f32, font::BODY), msg, font::BODY, color::text(), alpha);
}

// ---------------------------------------------------------------------------
// BMP 导出（自检/截图模式）
// ---------------------------------------------------------------------------

#[cfg_attr(target_os = "linux", allow(dead_code))] // 仅预览/走查路径使用
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

/// 找出两帧之间不同的行范围 `[y0, y1)`；全等返回 `None`。
///
/// 上屏（`fbdev::blit_dirty`）用它把"整屏 4MB 写入"降成"只写变化的行"。
/// 放在这里而不是 `fbdev.rs`：后者只在 Linux 编译，测试在开发机上跑不到，
/// 而"哪些行变了"恰恰是最容易写错、又最该被覆盖的一步。
///
/// `w` 是帧缓冲的行宽，`fw`/`fh` 是实际有效区域（行距可能大于行宽）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn dirty_rows(prev: &[u32], cur: &[u32], w: usize, fw: usize, fh: usize) -> Option<(usize, usize)> {
    let mut y0 = usize::MAX;
    let mut y1 = 0usize;
    for y in 0..fh {
        if cur[y * w..y * w + fw] != prev[y * fw..(y + 1) * fw] {
            if y0 == usize::MAX {
                y0 = y;
            }
            y1 = y + 1;
        }
    }
    if y0 >= y1 {
        None
    } else {
        Some((y0, y1))
    }
}

#[cfg(test)]
mod span_tests {
    use super::*;

    fn r() -> Rect {
        Rect { x: 10, y: 20, w: 100, h: 40 }
    }

    /// 形状之外的行必须返回空区间 —— 否则调用方会把整行当成"直填带"刷上颜色。
    /// 这是实际踩过的坑：AI 指令条（胶囊形）底边多出一整行 512 像素。
    #[test]
    fn row_span_is_empty_outside_shape() {
        for y in [19.5f32, 19.0, 60.5, 61.0] {
            let (a, b) = row_inner_span(r(), 12.0, y);
            assert!(a >= b, "y={y} 在形状外，应返回空区间，实得 ({a},{b})");
        }
    }

    #[test]
    fn row_span_covers_full_width_in_middle() {
        // 垂直居中处远离圆角，应覆盖整个宽度
        assert_eq!(row_inner_span(r(), 12.0, 40.0), (10, 110));
    }

    #[test]
    fn row_span_shrinks_near_rounded_corner() {
        let (a0, b0) = row_inner_span(r(), 12.0, 20.5); // 顶端行
        let (a1, b1) = row_inner_span(r(), 12.0, 40.0); // 中间行
        assert!(a0 > a1 && b0 < b1, "顶端行的内部区间必须比中间行窄");
    }

    #[test]
    fn row_span_handles_radius_larger_than_half_height() {
        // 胶囊形（radius == h/2）：中间行仍应覆盖整个宽度
        let rr = Rect { x: 0, y: 0, w: 200, h: 52 };
        assert_eq!(row_inner_span(rr, 26.0, 26.0), (0, 200));
        // 顶端行必须内缩
        let (a, b) = row_inner_span(rr, 26.0, 0.5);
        assert!(a > 0 && b < 200, "顶端行应内缩，实得 ({a},{b})");
    }

    /// 关键正确性约束：直填带内的每个像素，逐像素 SDF 算出的覆盖率必须真的是 1。
    ///
    /// 直填带用 `blend_pixel(alpha)` 而不再乘覆盖率 —— 一旦 `i` 取得不够保守，
    /// 边缘像素会被当成"满覆盖"，表现就是圆角边缘变实、和逐像素版本对不上。
    #[test]
    fn inner_span_only_covers_fully_opaque_pixels() {
        let shapes = [
            (Rect { x: 10, y: 20, w: 100, h: 40 }, 12.0f32),
            (Rect { x: 0, y: 0, w: 200, h: 52 }, 26.0),
            (Rect { x: 5, y: 5, w: 46, h: 46 }, 12.0),
            (Rect { x: 3, y: 7, w: 150, h: 42 }, 8.0),
        ];
        for (r, radius) in shapes {
            for yi in 0..r.h {
                let ay = (r.y + yi) as f32 + 0.5;
                let (a, b) = row_inner_span(r, radius, ay);
                for x in a..b {
                    let d = sdf_round_rect(r, radius, x as f32 + 0.5, ay);
                    let cov = (0.5 - d).clamp(0.0, 1.0);
                    assert!(
                        cov >= 0.999,
                        "矩形 {r:?} r={radius} 行 {yi} 像素 {x} 落在直填带内，但覆盖率只有 {cov}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod render_equiv_tests {
    use super::*;

    /// 逐像素参考实现：优化前的朴素写法，用于等价性比对。
    fn ref_rounded_rect(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
        let x0 = (r.x as f32 - 1.0).max(0.0) as usize;
        let y0 = (r.y as f32 - 1.0).max(0.0) as usize;
        let x1 = ((r.x + r.w) as f32 + 1.0).min(w as f32) as usize;
        let y1 = ((r.y + r.h) as f32 + 1.0).min(h as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(r, radius, x as f32 + 0.5, y as f32 + 0.5);
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    blend_pixel(buf, y * w + x, rgb, alpha * cov);
                }
            }
        }
    }

    /// 逐像素参考实现（描边）。
    fn ref_rounded_outline(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
        let x0 = (r.x - 2).max(0) as usize;
        let y0 = (r.y - 2).max(0) as usize;
        let x1 = ((r.x + r.w) as usize + 2).min(w);
        let y1 = ((r.y + r.h) as usize + 2).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(r, radius, x as f32 + 0.5, y as f32 + 0.5);
                if d < 0.5 && d > -1.5 {
                    let cov = (0.5 - d).clamp(0.0, 1.0);
                    blend_pixel(buf, y * w + x, rgb, alpha * cov);
                }
            }
        }
    }

    fn shapes() -> Vec<(Rect, f32)> {
        vec![
            (Rect { x: 20, y: 15, w: 100, h: 60 }, 12.0),
            (Rect { x: 0, y: 0, w: 200, h: 52 }, 26.0), // 胶囊
            (Rect { x: 5, y: 5, w: 46, h: 46 }, 12.0),  // Dock 图标
            (Rect { x: 3, y: 7, w: 150, h: 42 }, 8.0),  // 按钮
            (Rect { x: 40, y: 30, w: 20, h: 20 }, 3.0), // 小圆点
        ]
    }

    #[test]
    fn rounded_rect_fast_path_matches_reference() {
        let (w, h) = (240usize, 140);
        for (r, radius) in shapes() {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            rounded_rect(&mut fast, w, h, r, radius, [255, 255, 255], 0.7);
            ref_rounded_rect(&mut reference, w, h, r, radius, [255, 255, 255], 0.7);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            assert_eq!(diff, 0, "rounded_rect {r:?} r={radius}：{diff} 个像素与逐像素参考不一致");
        }
    }

    #[test]
    fn rounded_outline_band_scan_matches_reference() {
        let (w, h) = (240usize, 140);
        for (r, radius) in shapes() {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            rounded_outline(&mut fast, w, h, r, radius, [0, 0, 0], 0.6);
            ref_rounded_outline(&mut reference, w, h, r, radius, [0, 0, 0], 0.6);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            assert_eq!(diff, 0, "rounded_outline {r:?} r={radius}：{diff} 个像素与逐像素参考不一致");
        }
    }

    /// 逐像素参考实现（裁切填充）。
    fn ref_fill_clipped(
        buf: &mut [u32],
        w: usize,
        h: usize,
        area: Rect,
        shape: Rect,
        shape_radius: f32,
        rgb: [u8; 3],
        alpha: f32,
    ) {
        let x0 = area.x.max(0) as usize;
        let y0 = area.y.max(0) as usize;
        let x1 = ((area.x + area.w) as usize).min(w);
        let y1 = ((area.y + area.h) as usize).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(shape, shape_radius, x as f32 + 0.5, y as f32 + 0.5);
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    blend_pixel(buf, y * w + x, rgb, alpha * cov);
                }
            }
        }
    }

    #[test]
    fn fill_clipped_fast_path_matches_reference() {
        let (w, h) = (240usize, 140);
        let cases = [
            // (area, shape, radius)：内容区贴合窗口圆角
            (Rect { x: 0, y: 0, w: 60, h: 60 }, Rect { x: 0, y: 0, w: 200, h: 120 }, 16.0),
            (Rect { x: 10, y: 20, w: 180, h: 90 }, Rect { x: 10, y: 20, w: 180, h: 90 }, 16.0),
            (Rect { x: 5, y: 5, w: 50, h: 50 }, Rect { x: 5, y: 5, w: 100, h: 100 }, 30.0),
            (Rect { x: 30, y: 10, w: 100, h: 40 }, Rect { x: 0, y: 0, w: 240, h: 140 }, 26.0),
        ];
        for (area, shape, radius) in cases {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            fill_clipped(&mut fast, w, h, area, shape, radius, [200, 100, 50], 0.8);
            ref_fill_clipped(&mut reference, w, h, area, shape, radius, [200, 100, 50], 0.8);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            assert_eq!(
                diff, 0,
                "fill_clipped area={area:?} shape={shape:?} r={radius}：{diff} 像素与参考不一致"
            );
        }
    }

    /// 逐像素参考实现（渐变描边）。
    fn ref_gradient_outline(
        buf: &mut [u32],
        w: usize,
        h: usize,
        r: Rect,
        radius: f32,
        left: [u8; 3],
        right: [u8; 3],
        alpha: f32,
    ) {
        let x0 = (r.x - 2).max(0) as usize;
        let y0 = (r.y - 2).max(0) as usize;
        let x1 = ((r.x + r.w) as usize + 2).min(w);
        let y1 = ((r.y + r.h) as usize + 2).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(r, radius, x as f32 + 0.5, y as f32 + 0.5);
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

    #[test]
    fn gradient_outline_band_scan_matches_reference() {
        let (w, h) = (240usize, 140);
        for (r, radius) in shapes() {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            gradient_outline(&mut fast, w, h, r, radius, [10, 200, 210], [140, 120, 230], 0.9);
            ref_gradient_outline(&mut reference, w, h, r, radius, [10, 200, 210], [140, 120, 230], 0.9);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            if diff > 0 {
                let mut shown = 0;
                for (i, (a, b)) in fast.iter().zip(&reference).enumerate() {
                    if a != b && shown < 8 {
                        eprintln!("    差异 ({},{}) fast={:08x} ref={:08x}", i % w, i / w, a, b);
                        shown += 1;
                    }
                }
            }
            assert_eq!(diff, 0, "gradient_outline {r:?} r={radius}：{diff} 像素与参考不一致");
        }
    }
}

#[cfg(test)]
mod blit_tests {
    use super::dirty_rows;

    #[test]
    fn dirty_rows_detects_single_changed_row() {
        let (w, fw, fh) = (4usize, 4usize, 3usize);
        let prev = vec![0u32; w * fh];
        let mut cur = prev.clone();
        cur[w..2 * w].fill(7); // 只有第 1 行变化
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), Some((1, 2)));
    }

    #[test]
    fn dirty_rows_returns_none_when_identical() {
        let (w, fw, fh) = (4usize, 4usize, 3usize);
        let prev = vec![1u32; w * fh];
        assert_eq!(dirty_rows(&prev, &prev, w, fw, fh), None);
    }

    #[test]
    fn dirty_rows_spans_first_to_last_change() {
        let (w, fw, fh) = (4usize, 4usize, 4usize);
        let prev = vec![0u32; w * fh];
        let mut cur = prev.clone();
        cur[0] = 9; // 第 0 行
        cur[3 * w + 2] = 9; // 第 3 行
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), Some((0, 4)));
    }

    #[test]
    fn dirty_rows_ignores_padding_beyond_valid_width() {
        // fw < w（行距大于行宽）：padding 区不参与比较
        let (w, fw, fh) = (6usize, 4usize, 2usize);
        let prev = vec![0u32; w * fh];
        let mut cur = prev.clone();
        cur[4] = 5; // 第 0 行有效宽度之外
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), None);
        cur[1] = 5; // 有效宽度之内
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), Some((0, 1)));
    }
}
