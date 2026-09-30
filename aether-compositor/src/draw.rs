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
/// docs/archive/ui-design-plan.md §3：色板 / 字阶 / 圆角与阴影 / 交互态）。
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
        // 2026-09-30：真机反馈浅色界面"发粉"。浅色基色一律改成**纯白/纯中性**（不带蓝调也不带暖调），
        // 靠描边与阴影做层次 —— 之前那套偏蓝的灰在真机帧缓冲上会读成粉。
        /// 壁纸渐变顶（纯白）
        pub fn bg_top() -> [u8; 3] {
            pick([255, 255, 255], [27, 29, 33])
        }
        /// 壁纸渐变底（极浅中性灰，几乎看不出渐变）
        pub fn bg_bottom() -> [u8; 3] {
            pick([244, 245, 247], [23, 24, 28])
        }
        /// 底座（菜单栏、侧栏、终端底、状态条、音乐面板）。
        /// 浅色下**必须比窗口体暗**，否则底座与窗口同色，所有区域糊成一片。
        pub fn inset() -> [u8; 3] {
            pick([235, 237, 240], [30, 32, 41])
        }
        /// 窗口/面板主面（纯白，与壁纸同色，靠描边区分）
        pub fn surface_1() -> [u8; 3] {
            pick([255, 255, 255], [46, 49, 60])
        }
        /// 内容"纸面"、卡片、浮层、侧栏选中
        pub fn surface_2() -> [u8; 3] {
            pick([255, 255, 255], [64, 68, 80])
        }
        /// 输入框、悬停中的控件
        pub fn surface_3() -> [u8; 3] {
            pick([241, 243, 246], [80, 84, 97])
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

        /// 悬浮玻璃层不透明度（P0：整体提纯——面板不透明化，浮层保留玻璃）
        pub fn glass_alpha(hovered: bool) -> f32 {
            if is_light() {
                if hovered {
                    0.99
                } else {
                    0.98
                }
            } else if hovered {
                0.95
            } else {
                0.92
            }
        }

        // —— 强调色（明亮模式需在白底上达到可读对比度，故整体加深）——
        /// 主青：选中、焦点、主按钮
        pub fn accent() -> [u8; 3] {
            pick([11, 132, 150], [64, 190, 205])
        }
        /// 辅紫（AI 相关元素）—— 2026-09-29 P0 移除：强调色收敛为单一 `accent()`，
        /// 所有渐变组合（AI 状态点/焦点环/徽标/气泡环/品牌徽标）改为单色 accent。
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

        // —— 2026-09-29 P0：极光/粉彩壁纸（aurora_* 与 vignette）移除，静态壁纸见 draw_background_rows ——
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
        /// 列表项高亮标题 / 设置项标题（P0 新增；P3 设置中心接入后移除 allow）
        #[allow(dead_code)]
        pub const FLOAT: f32 = 16.0;
        /// 设置页大标题（P0 新增；P3 设置中心接入后移除 allow）
        #[allow(dead_code)]
        pub const PAGE_TITLE: f32 = 24.0;
        /// 特大标题（关于页，P0 新增；P3 设置中心接入后移除 allow）
        #[allow(dead_code)]
        pub const HERO: f32 = 28.0;
        /// 图标内字形（Dock glyph、AI 徽标字母；非文字层级，不占字阶）
        pub const GLYPH: f32 = 16.0;
    }

    /// 圆角 §3.3：三档（2026-09-29 P0：整体收小，去"圆润玩具感"）
    pub mod radius {
        /// 按钮、输入框、小控件
        pub const SM: f32 = 4.0;
        /// 卡片、面板、列表容器
        pub const MD: f32 = 6.0;
        /// 窗口、弹窗
        pub const LG: f32 = 8.0;
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

        /// 窗口浮起（P0：调轻，配合单层阴影；层次主要由 1px 描边承担）
        pub fn elev_1() -> f32 {
            pick(0.10, 0.16)
        }
        /// 非活动窗口
        pub fn elev_1_dim() -> f32 {
            pick(0.05, 0.08)
        }
        /// 弹窗、浮层（下拉、AI 指令条、Toast、Dock）
        pub fn elev_2() -> f32 {
            pick(0.14, 0.22)
        }
        /// 模态（安装向导、权限确认）
        pub fn elev_3() -> f32 {
            pick(0.20, 0.30)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
/// 轻阴影（两层近似软影）：偏移 y+2、小扩散，强弱由 `theme::elevation` 档位缩放。
/// 2026-09-29 P0：从 5 层大软影改为两层小影 —— "发光卡片"观的来源，层次感交给描边。
pub fn shadow(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, strength: f32) {
    for i in 0..2 {
        let grow = i as f32 * 6.0;
        let s = Rect {
            x: r.x - grow as i32 + 1,
            y: r.y - grow as i32 + 2,
            w: r.w + grow as i32 * 2,
            h: r.h + grow as i32 * 2,
        };
        let a0 = strength * (0.30 - i as f32 * 0.14);
        let rad = radius + grow;
        let x0 = (s.x as f32 - 1.0).max(0.0) as usize;
        let y0 = (s.y as f32 - 1.0).max(0.0) as usize;
        let x1 = ((s.x + s.w) as f32 + 1.0).min(w as f32) as usize;
        let y1 = ((s.y + s.h) as f32 + 1.0).min(h as f32) as usize;
        for y in y0..y1 {
            let row = y * w;
            for x in x0..x1 {
                let d = sdf_round_rect(s, rad, x as f32 + 0.5, y as f32 + 0.5);
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

/// 文件系统条目 —— 文件管理器的数据源。
#[derive(Clone, Debug)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

impl FsEntry {
    /// 人类可读的大小（目录不显示大小）。
    pub fn size_label(&self) -> String {
        if self.is_dir {
            return "—".into();
        }
        let n = self.size;
        if n < 1024 {
            format!("{n} B")
        } else if n < 1024 * 1024 {
            format!("{:.1} KB", n as f64 / 1024.0)
        } else if n < 1024 * 1024 * 1024 {
            format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }
}

/// 默认起始目录：Linux 取家目录，Windows 预览取 USERPROFILE。
pub fn default_cwd() -> String {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    if cfg!(windows) { "C:/".into() } else { "/".into() }
}

/// 读取目录内容：目录在前，同类按名称（不区分大小写）排序。
///
/// 隐藏文件（以 `.` 开头）默认不列出 —— 与 Finder/资源管理器的默认行为一致，
/// 也避免一进家目录就被 `.cargo`/`.config` 刷屏。
///
/// 读失败时返回空列表与原因字符串：**调用方必须把原因显示出来**，
/// 否则用户看到的是"空目录"，与"没权限"无法区分。
pub fn read_dir_entries(path: &str) -> (Vec<FsEntry>, Option<String>) {
    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => return (Vec::new(), Some(format!("{e}"))),
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let meta = ent.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        out.push(FsEntry { name, is_dir, size });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    (out, None)
}

/// 路径拼接（处理结尾斜杠，Windows 下也兼容正斜杠）。
pub fn join_path(base: &str, name: &str) -> String {
    if base.ends_with('/') || base.ends_with('\\') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// 路径是否已在根（无法再向上）。
fn path_is_root(path: &str) -> bool {
    let t = path.trim_end_matches(['/', '\\']);
    t.is_empty() || t.ends_with(':')
}

/// 取路径的上级；已在根时返回原值。
pub fn parent_of(path: &str) -> String {
    let t = path.trim_end_matches(['/', '\\']);
    if t.is_empty() || t.ends_with(':') {
        return path.to_string();
    }
    match t.rfind(['/', '\\']) {
        Some(0) => t[..1].to_string(),
        Some(i) if t[..i].ends_with(':') => t[..=i].to_string(),
        Some(i) => t[..i].to_string(),
        None => path.to_string(),
    }
}

/// 取路径末段作为窗口标题；根目录返回原路径。
pub fn path_leaf(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => path.to_string(),
    }
}

/// 文本预览内容。
#[derive(Clone, Debug)]
pub struct PreviewData {
    /// 被打开文件的完整路径（标题栏显示）
    pub path: String,
    /// 已按行切好的内容（限长）
    pub lines: Vec<String>,
    /// 读取失败的原因；有值时窗口内显示它而不是内容
    pub error: Option<String>,
    /// 是否因超长被截断
    pub truncated: bool,
    /// 滚动偏移（2.4：长文件能翻页，不再只能看前 N 行）。
    /// 夹取逻辑在 `textview::ScrollView` 里只有一份。
    pub scroll: crate::textview::ScrollView,
}

/// 预览单次读取的字节上限：再大就不是"看一眼"而是"打开大文件"了，
/// 而合成器是单线程渲染，读大文件会直接卡住整个界面。
const PREVIEW_MAX_BYTES: u64 = 64 * 1024;
/// 预览最多渲染的行数（超出部分只显示提示）
const PREVIEW_MAX_LINES: usize = 400;

/// 读取一个文件用于预览。二进制文件与超大文件都要给出明确原因，
/// 不能静默显示空白 —— 那和"文件是空的"无法区分。
pub fn read_preview(path: &str) -> PreviewData {
    let meta = std::fs::metadata(path);
    if let Ok(m) = &meta {
        if m.is_dir() {
            return PreviewData { path: path.into(), lines: Vec::new(), error: Some("这是一个目录".into()), truncated: false, scroll: Default::default() };
        }
        if m.len() > PREVIEW_MAX_BYTES {
            return PreviewData {
                path: path.into(),
                lines: Vec::new(),
                error: Some(format!("文件过大（{}），暂不支持预览", FsEntry { name: String::new(), is_dir: false, size: m.len() }.size_label())),
                truncated: false,
                scroll: Default::default(),
            };
        }
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            // 非 UTF-8 多半是二进制：不要用 lossy 糊一屏乱码，直接说明
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(_) => {
                    return PreviewData { path: path.into(), lines: Vec::new(), error: Some("二进制文件，无法以文本预览".into()), truncated: false, scroll: Default::default() };
                }
            };
            let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
            let truncated = lines.len() > PREVIEW_MAX_LINES;
            if truncated {
                lines.truncate(PREVIEW_MAX_LINES);
            }
            PreviewData { path: path.into(), lines, error: None, truncated, scroll: Default::default() }
        }
        Err(e) => PreviewData { path: path.into(), lines: Vec::new(), error: Some(format!("{e}")), truncated: false, scroll: Default::default() },
    }
}

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

/// 帧渲染器：持有跨帧缓存（背景层等）与可点击区域登记（供命中测试）。
pub struct Renderer {
    bg: Vec<u32>,
    frames_since_bg: u32,
    bg_w: usize,
    bg_h: usize,
    /// 壁纸分帧生成进度：已生成的行数（= h 表示整屏就绪）。
    ///
    /// 壁纸整屏生成一次要 0.4–0.6 秒，压在一帧里就是开机后一次明显卡顿。
    /// 改成每帧生成 `BG_ROWS_PER_FRAME` 行后，单帧只付 1/N 的成本，
    /// 而绘制目标是独立的 `bg` 缓冲，未完成前屏幕上看不到横向接缝。
    bg_row: usize,
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
    /// Dock 图标命中区
    pub dock_icons: Vec<Rect>,
    /// 已装应用（`(id, 显示名)`），按 id 排序。由 main 扫描 `/var/apps` 后填进来。
    ///
    /// 为什么放在 Renderer 上而不是当参数传：`draw_dock` 的调用链已经很深，
    /// 而且这份数据是"每帧画一次"的稳定状态，不是逐帧变化的参数。
    pub installed_apps: Vec<(String, String)>,
    /// 用户设置（P3）：启动时从 `/var/lib/aether/settings.json` 读入，改动即写回
    pub settings: crate::settings::Settings,
    /// 设置窗口当前页（左栏选中项）
    pub settings_page: usize,
    /// 设置面板本帧登记的可交互项（绘制期登记、事件循环消费）
    pub settings_hits: Vec<(Rect, crate::settings::SettingsHit)>,
    /// 当前展开的下拉菜单：(各项命中区, 文案)
    pub dropdown: Option<(Vec<Rect>, Vec<&'static str>)>,
    /// 安装向导磁盘行的命中区 (矩形, 设备名, 容量MB)
    pub installer_rows: Vec<(Rect, String, u64)>,
    /// 文件管理器图标网格的命中区 (矩形, 条目索引)，每帧重建
    pub file_cells: Vec<(Rect, usize)>,
    /// 文件管理器侧栏"上级目录"按钮的命中区；无上级时为空矩形
    pub file_up: Rect,
    /// 侧栏"位置"命中区：(矩形, 侧栏索引)
    pub sidebar_hits: Vec<(Rect, usize)>,
    /// 面包屑路径段命中区：(矩形, 目标路径)
    pub crumb_hits: Vec<(Rect, String)>,
    /// 窗口红绿灯命中区：(窗口索引, 关闭按钮, 最大化按钮)。
    ///
    /// 此前红绿灯**只画不响应** —— 开了 8 个窗口后没有任何关闭手段，用户会直接卡住。
    /// 每帧重建，与窗口几何保持同步。
    pub window_lights: Vec<(usize, Rect, Rect)>,
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
            bg_row: 0,
            shadow_layer: Vec::new(),
            shadow_key: 0,
            menubar_menus: Vec::new(),
            dock_icons: Vec::new(),
            installed_apps: Vec::new(),
            settings: crate::settings::Settings::default(),
            settings_page: crate::settings::first_enabled_page(),
            settings_hits: Vec::new(),
            dropdown: None,
            installer_rows: Vec::new(),
            file_cells: Vec::new(),
            file_up: Rect { x: 0, y: 0, w: 0, h: 0 },
            window_lights: Vec::new(),
            sidebar_hits: Vec::new(),
            crumb_hits: Vec::new(),
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

    /// 一次性把壁纸补全（单帧渲染与基准测试用）。
    ///
    /// 交互路径靠分帧逐步生成，而 `--shot` 只渲染一帧 —— 必须先补全，
    /// 否则截出来的图只有顶上若干行是壁纸，其余是空白。
    #[cfg_attr(target_os = "linux", allow(dead_code))] // 仅 --shot/--bench 走查路径使用
    pub fn prepare_background(&mut self, w: usize, h: usize, t: f32) {
        if self.bg.len() != w * h {
            self.bg = vec![0; w * h];
        }
        draw_background(&mut self.bg, w, h, t);
        self.bg_w = w;
        self.bg_h = h;
        self.bg_row = h;
        self.frames_since_bg = 0;
        self.shadow_key = 0;
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

        // 壁纸分帧生成：每帧只生成一段行带，整屏 0.4–0.6s 的成本摊到 ~16 帧。
        //
        // 为什么必须分：这不是"优化"，是**卡顿**问题——压在单帧里实测首帧 561ms，
        // 开机后第一眼就是半秒死机（VM 上更久）。分帧后单帧只多 ~35ms/16 ≈ 2ms 级，
        // 且因为画进的是独立的 bg 缓冲，未完成时屏幕上是上一版完整壁纸，不会有接缝。
        //
        // 漂移是**分钟级**的变化，但生成一次要 ~0.4s：240 帧在 10fps 下是 24 秒、
        // 在 30fps 下只剩 8 秒 —— 帧率提上来之后必须把间隔一起提上去，否则
        // "优化帧率"反而让周期性卡顿变密。1800 帧 @30fps ≈ 60 秒。
        const BG_REFRESH_FRAMES: u32 = 1800;
        let size_changed = self.bg_w != w || self.bg_h != h || self.bg.len() != w * h;
        if size_changed || self.frames_since_bg > BG_REFRESH_FRAMES {
            if self.bg.len() != w * h {
                self.bg = vec![0; w * h];
            }
            self.bg_w = w;
            self.bg_h = h;
            self.bg_row = 0;
            self.frames_since_bg = 0;
        }
        if self.bg_row < h {
            let y1 = (self.bg_row + BG_ROWS_PER_FRAME).min(h);
            draw_background_rows(&mut self.bg, w, h, t, self.bg_row, y1);
            self.bg_row = y1;
            self.frames_since_bg = 0;
        }
        mark!("bg_gen");
        self.frames_since_bg = self.frames_since_bg.saturating_add(1);

        // 投影烘焙：只取决于窗口几何与激活态，鼠标移动时完全不变。
        // 未变化时这一层直接复用，稳态每帧只剩一次内存拷贝。
        //
        // 壁纸**生成中**时每帧跟随一次（只拷不烘）——否则屏幕会停在第 1 帧那张
        // "一条壁纸 + 一片黑"的画面上直到铺满，反而比原来更难看。
        let skey = desktop_key(desktop);
        let bg_incomplete = self.bg_row < h;
        if self.shadow_layer.len() != w * h || self.shadow_key != skey || bg_incomplete {
            if self.shadow_layer.len() != w * h {
                self.shadow_layer = vec![0; w * h];
            }
            self.shadow_layer.copy_from_slice(&self.bg);
            if bg_incomplete {
                self.shadow_key = 0; // 铺满后再烘一次（含投影）
            } else {
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
        }
        buf.copy_from_slice(&self.shadow_layer);
        mark!("bg_copy");

        if let Some(z) = ui.snap {
            rounded_rect(buf, w, h, z, radius::LG, color::accent(), 0.08);
            rounded_outline(buf, w, h, z, radius::LG, color::accent(), 0.5);
        }
        mark!("snap");

        let fv = FileView {
            cwd: &desktop.cwd,
            entries: &desktop.entries,
            selected: desktop.selected,
            error: desktop.dir_error.as_deref(),
            scroll: desktop.scroll,
            sidebar: &desktop.sidebar,
        };
        self.file_cells.clear();
        self.window_lights.clear();
        self.sidebar_hits.clear();
        self.crumb_hits.clear();
        self.settings_hits.clear();
        for (i, win) in desktop.wins.iter().enumerate() {
            draw_window(
                buf, w, h, win, i == desktop.active, ui.mouse, t, tr, &fv,
                &mut self.file_cells, &mut self.file_up, i, &mut self.window_lights,
                &mut self.sidebar_hits, &mut self.crumb_hits,
                self.settings, self.settings_page, &mut self.settings_hits,
            );
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

        let open_titles: Vec<&str> = desktop.wins.iter().map(|x| x.title.as_str()).collect();
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
// 2026-09-29 P0：band_profile / grain / AuroraBand / wash 随"极光壁纸"一并移除，
// 静态壁纸见 draw_background_rows。

/// 壁纸分帧生成时每帧生成的行数。
///
/// 760 行 ÷ 32 ≈ 24 帧（30fps 下 0.8 秒铺完），单帧增量约 17ms —— 落在
/// 30fps 预算（33.3ms）之内，所以生成期间画面是**平滑地逐层刷出**，
/// 而不是"卡一下再整屏跳出"。取更小的值会让铺满时间变长，取更大则开始掉帧。
pub const BG_ROWS_PER_FRAME: usize = 32;

/// 生成壁纸的 `[y0, y1)` 行。
///
/// **分帧生成的基础**：整屏逐像素生成要 0.4–0.6 秒，压在一帧里就是开机后
/// 一次肉眼可见的卡死（实测首帧 561ms）。切成行带后每帧只付 1/N 的成本，
/// 而且因为绘制目标是与屏幕分离的 `bg` 缓冲，未完成前屏幕上不会出现横向接缝。
pub fn draw_background_rows(buf: &mut [u32], w: usize, h: usize, _t: f32, y0: usize, y1: usize) {
    let y1 = y1.min(h);
    if y0 >= y1 || buf.len() < w * h {
        return;
    }
    // 2026-09-29 P0：壁纸改为**静态**——低饱和中性 + 极浅的垂直层次。
    // 逐像素的极光/柔光团/颗粒/暗角移除；每行一次 lerp、整行同色，成本大幅下降。
    let (top, bottom) = (color::bg_top(), color::bg_bottom());
    for y in y0..y1 {
        let t = y as f32 / h.max(1) as f32;
        let rgb = [
            lerp(top[0] as f32, bottom[0] as f32, t) as u32,
            lerp(top[1] as f32, bottom[1] as f32, t) as u32,
            lerp(top[2] as f32, bottom[2] as f32, t) as u32,
        ];
        let px = (rgb[0] << 16) | (rgb[1] << 8) | rgb[2];
        let row = y * w;
        for x in 0..w {
            buf[row + x] = px;
        }
    }
}

/// 整屏一次生成（单帧模式与等价性测试用）。
#[cfg_attr(target_os = "linux", allow(dead_code))] // 单帧整屏生成，仅走查/测试路径调用
pub fn draw_background(buf: &mut [u32], w: usize, h: usize, t: f32) {
    draw_background_rows(buf, w, h, t, 0, h);
}

// 2026-09-29 P0：light_wallpaper_rows 移除（静态壁纸见 draw_background_rows）。

// 2026-09-29 P0：aurora_rows 移除（静态壁纸见 draw_background_rows）。

// ---------------------------------------------------------------------------
// 菜单栏：发丝底 + 品牌 + 菜单（可点击）+ 搜索胶囊 + 电池 + 时钟
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_menubar(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let bar = Rect { x: 0, y: 0, w: w as i32, h: metric::MENUBAR_H };
        fill_rect(buf, w, h, bar, color::inset(), 0.62);
        fill_rect(buf, w, h, Rect { x: 0, y: metric::MENUBAR_H - 1, w: w as i32, h: 1 }, color::hairline(), 0.10);

        let Some(tr) = tr else { return };

        // 品牌：三角徽标（P0：单色 accent，去"青→紫"渐变）
        let (bx, by) = (12i32, 8i32);
        let accent = color::accent();
        for row in 0..16 {
            let half = row / 2;
            let rgb = accent;
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
        // 2026-09-29 P3：时钟改由用户设置驱动（时区偏移 / 24 或 12 小时制 / 是否显示秒），
        // 不再只有"硬编码 +8"一种可能。
        let clock = crate::text::clock_fmt(
            crate::text::now_utc_secs(),
            self.settings.tz_offset_min,
            self.settings.clock_24h,
            self.settings.clock_seconds,
        );
        let clock_w = tr.measure_bold(&clock, font::BODY);
        let clock_x = w as f32 - 16.0 - clock_w;
        tr.draw_bold(buf, w, h, clock_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), &clock, font::BODY, color::text(), 0.95);

        // 2026-09-29 P2：**删掉"搜索"胶囊**。它只显示"搜索 K"，点下去弹一句 toast ——
        // 一个纯粹的假控件（真正的启动器/搜索是 P4 控制中心的活）。顶栏宁缺毋滥：
        // 现在右侧只剩 AI 状态与时钟，两个都是真实信息。
        // 命令栏（下方）本来就恒聚焦，不需要"点搜索再输入"这一层假仪式。

        // AI 状态指示（§4 三态：本地青 / 云端紫 / 离线灰）
        let ai_label = ui.ai_status.label();
        let ai_w = tr.measure(ai_label, font::CAPTION);
        let ai_x = clock_x as f32 - 20.0 - ai_w;
        rounded_rect(buf, w, h, Rect { x: ai_x as i32 - 12, y: 13, w: 6, h: 6 }, 3.0, ui.ai_status.color(), 0.95);
        draw_text(tr, buf, w, h, ai_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::CAPTION), ai_label, font::CAPTION, color::text_dim(), 0.85);

        // 2026-09-29 P2：**删掉假电量图标**。它是画上去的固定 50% 填充，既没有
        // /sys/class/power_supply 数据源也不代表任何状态 —— 桌面上的假信息比没有更糟。
        // 真有电池节点时再按真实读数画（P3/P4 接 ACPI）。
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

#[allow(clippy::too_many_arguments)]
fn draw_window(
    buf: &mut [u32],
    w: usize,
    h: usize,
    win: &Win,
    active: bool,
    mouse: (f32, f32),
    t: f32,
    tr: Option<&TextRenderer>,
    fv: &FileView,
    cells: &mut Vec<(Rect, usize)>,
    up: &mut Rect,
    win_idx: usize,
    lights_out: &mut Vec<(usize, Rect, Rect)>,
    sidebar_hits: &mut Vec<(Rect, usize)>,
    crumb_hits: &mut Vec<(Rect, String)>,
    settings: crate::settings::Settings,
    settings_page: usize,
    settings_hits: &mut Vec<(Rect, crate::settings::SettingsHit)>,
) {
    let r = win.rect;
    let title = win.title.as_str();
    let kind = win.kind;
    let body = color::surface_1();
    // 不透明度定得高：合成器没有模糊（backdrop-filter），窗口一旦半透明，
    // 后面窗口的文字就会"透"上来变成鬼影——那比没有玻璃感难看得多。
    // 2026-09-29 P2：活动/非活动对比拉开（0.955/0.90 → 0.97/0.94）——
    // 多窗口叠放时要一眼看出焦点在哪。
    let body_alpha = if active { 0.97 } else { 0.94 };
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
    // 描边：1px 承担层次（P0 把大软影撤了，边必须更清楚；P2 由 0.22/0.12 提到 0.30/0.16）
    rounded_outline(buf, w, h, r, radius::LG, color::hairline(), if active { 0.30 } else { 0.16 });

    // 2026-09-29 P2：**边缘缩放提示**。缩放逻辑一直有（main.rs::topmost_edge 的 6px 抓取带），
    // 但屏幕上没有任何反馈、光标也不变 —— 用户根本不知道窗口边缘能拖。
    // 判定带与命中逻辑同宽（6px），刻意不门控 `active`：非活动窗口边缘同样可拖（命中逻辑如此），
    // 提示必须与行为一致。被上层窗口盖住的部分由绘制顺序自然裁剪。
    {
        const GRAB: i32 = 6;
        let (mx, my) = (mouse.0 as i32, mouse.1 as i32);
        let (l, rr) = (r.x, r.x + r.w);
        let (t, b) = (r.y, r.y + r.h);
        let in_y = my >= t && my <= b;
        let in_x = mx >= l && mx <= rr;
        let round = radius::LG as i32;
        if in_y && (mx - l).abs() <= GRAB {
            fill_rect(buf, w, h, Rect { x: l, y: t + round, w: 2, h: (r.h - 2 * round).max(0) }, color::accent(), 0.55);
        } else if in_y && (mx - rr).abs() <= GRAB {
            fill_rect(buf, w, h, Rect { x: rr - 2, y: t + round, w: 2, h: (r.h - 2 * round).max(0) }, color::accent(), 0.55);
        } else if in_x && (my - t).abs() <= GRAB {
            fill_rect(buf, w, h, Rect { x: l + round, y: t, w: (r.w - 2 * round).max(0), h: 2 }, color::accent(), 0.55);
        } else if in_x && (my - b).abs() <= GRAB {
            fill_rect(buf, w, h, Rect { x: l + round, y: b - 2, w: (r.w - 2 * round).max(0), h: 2 }, color::accent(), 0.55);
        }
    }

    // 红绿灯（左）：直径 10px、间距 7px，悬停时整组显示符号
    //
    // 2026-09-30：**中间那颗（最小化）没有接线** —— 没有"最小化到哪去"的语义。
    // 所以把它画成**禁用态**（暗一档、不显示悬停符号）：画一颗看起来能点、点了没反应的灯，
    // 比不放这颗灯更糟（本项目对假控件零容忍，P2 已删掉假电量与假搜索胶囊）。
    // 接线时要一并改：`Win` 加 `minimized`、渲染/命中/tab 循环跳过、Dock 图标负责恢复。
    const MINIMIZE_WIRED: bool = false;
    let lights = [color::close(), color::min(), color::zoom()];
    let ly = r.y + (metric::TITLE_H - metric::LIGHT_D) / 2;
    let group = Rect {
        x: r.x + 8,
        y: r.y,
        w: metric::LIGHT_D * 3 + metric::LIGHT_GAP * 2 + 12,
        h: metric::TITLE_H,
    };
    let hovered = active && group.contains(mouse.0, mouse.1);
    // 命中区按"可视圆点 + 一点余量"登记（圆点只有 10px，直接用圆点本身太难点）
    let mut close_rect = Rect { x: 0, y: 0, w: 0, h: 0 };
    let mut zoom_rect = Rect { x: 0, y: 0, w: 0, h: 0 };
    for (i, c) in lights.iter().enumerate() {
        let lx = r.x + 14 + (i as i32) * (metric::LIGHT_D + metric::LIGHT_GAP);
        let dot = Rect { x: lx, y: ly, w: metric::LIGHT_D, h: metric::LIGHT_D };
        let disabled = i == 1 && !MINIMIZE_WIRED;
        let alpha = if disabled {
            0.28
        } else if active {
            0.95
        } else {
            0.6
        };
        rounded_rect(buf, w, h, dot, metric::LIGHT_D as f32 / 2.0, *c, alpha);
        if hovered && !disabled {
            light_symbol(buf, w, h, lx + metric::LIGHT_D / 2, ly + metric::LIGHT_D / 2, i);
        }
        // 命中区扩到 24×24 且垂直居中于标题栏：10px 的圆点在真机上不好点
        let hit = Rect {
            x: lx + metric::LIGHT_D / 2 - 12,
            y: r.y + (metric::TITLE_H - 24) / 2,
            w: 24,
            h: 24,
        };
        match i {
            0 => close_rect = hit,
            2 => zoom_rect = hit,
            // 中间那颗不登记命中区：未接线就不该响应（与上面的禁用态一致）
            _ => {}
        }
    }
    lights_out.push((win_idx, close_rect, zoom_rect));

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
    match kind {
        WinKind::Terminal => draw_term_content(buf, w, h, r, win.term.as_ref(), active, t, tr),
        WinKind::Music => draw_music_content(buf, w, h, content, r, mouse, tr),
        // Preview 暂时复用文件管理器的纸面底，真正的预览渲染在后续阶段接入
        WinKind::Files => draw_files_content(
            buf, w, h, content, r, mouse, tr, fv, cells, up, sidebar_hits, crumb_hits,
        ),
        WinKind::Preview => draw_preview_content(buf, w, h, content, win.preview.as_ref(), tr),
        WinKind::Wayland => draw_wayland_content(buf, w, h, content, win.wayland.as_ref()),
        WinKind::Settings => draw_settings_content(
            buf, w, h, content, mouse, tr, settings, settings_page, settings_hits,
        ),
    }
}

/// 设置中心的内容（P3）：左侧分组导航 + 右侧页面。
///
/// 结构对齐 Windows 11 / macOS 的设置（左分组、右页面），但**只用真实能力**：
/// 能用的页给真控件，不能用的页在左栏置灰并标「待接入」—— 本项目对"点了没用的假控件"
/// 是零容忍（P2 刚删掉假电量与假搜索胶囊），所以宁可少画。
///
/// 绘制期**只登记命中区**（`hits`），不改任何状态：状态变更由事件循环按命中结果执行。
fn draw_settings_content(
    buf: &mut [u32],
    w: usize,
    h: usize,
    r: Rect,
    mouse: (f32, f32),
    tr: Option<&TextRenderer>,
    s: crate::settings::Settings,
    page: usize,
    hits: &mut Vec<(Rect, crate::settings::SettingsHit)>,
) {
    use crate::settings::{SettingsHit, PAGES};
    use crate::widgets;
    const SIDEBAR_W: i32 = 168;

    // 左栏底：用 inset 与右侧页面区分（设置窗口是"两栏"，不是一整块纸）
    let side = Rect { x: r.x, y: r.y, w: SIDEBAR_W, h: r.h };
    fill_rect(buf, w, h, side, color::inset(), 0.55);
    fill_rect(buf, w, h, Rect { x: side.x + side.w - 1, y: side.y, w: 1, h: side.h }, color::hairline(), 0.08);

    let Some(tr) = tr else { return };

    // ---- 左栏：分组标题 + 页行 ----
    let mut y = r.y + 12;
    let mut group = "";
    for (i, p) in PAGES.iter().enumerate() {
        if p.group != group {
            if !group.is_empty() {
                y += 10;
            }
            widgets::group_header(buf, w, h, r.x + 16, y, p.group, Some(tr));
            y += 20;
            group = p.group;
        }
        let row = Rect { x: r.x + 8, y, w: SIDEBAR_W - 16, h: 30 };
        let st = widgets::States {
            hover: p.enabled && row.contains(mouse.0, mouse.1),
            selected: i == page,
            disabled: !p.enabled,
            ..Default::default()
        };
        widgets::row(buf, w, h, row, p.title, None, st, Some(tr));
        if p.enabled {
            hits.push((row, SettingsHit::Page(i)));
        } else {
            // 不可用的页如实标注，而不是让用户点进去发现是空壳
            let tag = tr.measure("待接入", font::LABEL);
            tr.draw(
                buf, w, h,
                (row.x + row.w - 8) as f32 - tag,
                tr.vcenter(row.y as f32, row.h as f32, font::LABEL),
                "待接入", font::LABEL, color::text_dim(), 0.5,
            );
        }
        y += 32;
        if y > r.y + r.h - 40 {
            break; // 窗口再矮也不越界
        }
    }

    // ---- 右侧页面 ----
    let content = Rect { x: r.x + SIDEBAR_W, y: r.y, w: r.w - SIDEBAR_W, h: r.h };
    let title = PAGES.get(page).map(|p| p.title).unwrap_or("设置");
    tr.draw_bold(buf, w, h, (content.x + 24) as f32, (content.y + 18) as f32, title, font::PAGE_TITLE, color::text(), 0.98);
    widgets::divider(buf, w, h, Rect { x: content.x + 24, y: content.y + 54, w: (content.w - 48).max(0), h: 1 });

    let row_w = (content.w - 48).max(120);
    let mut ry = content.y + 70;

    /// 一行「标题 + 副标题 + 右侧开关」。**只登记开关本身**：
    /// 整行与开关都登记会让一次点击命中两个目标（判定打架）。
    fn toggle_row(
        buf: &mut [u32], w: usize, h: usize, mouse: (f32, f32), tr: &TextRenderer,
        row: Rect, label: &str, sub: Option<&str>, on: bool,
        id: SettingsHit, hits: &mut Vec<(Rect, SettingsHit)>,
    ) {
        let st = widgets::States { hover: row.contains(mouse.0, mouse.1), ..Default::default() };
        widgets::row(buf, w, h, row, label, sub, st, Some(tr));
        let sw = Rect { x: row.x + row.w - 58, y: row.y + 12, w: 46, h: 20 };
        widgets::switch(buf, w, h, sw, on, widgets::States::at(sw, mouse));
        hits.push((sw, id));
    }

    match title {
        "时钟与时区" => {
            // 实时预览：改任何一项都能立刻看到结果（不用重启、不用"应用"按钮）
            let now = crate::text::now_utc_secs();
            let preview = crate::text::clock_fmt(now, s.tz_offset_min, s.clock_24h, s.clock_seconds);
            tr.draw_bold(buf, w, h, (content.x + 24) as f32, ry as f32, &preview, font::HERO, color::text(), 0.98);
            let tz = format!("{}　·　每次微调 30 分钟", s.tz_label());
            tr.draw(buf, w, h, (content.x + 24) as f32, (ry + 42) as f32, &tz, font::CAPTION, color::text_dim(), 0.9);
            ry += 78;

            let r1 = Rect { x: content.x + 18, y: ry, w: row_w + 12, h: 44 };
            toggle_row(buf, w, h, mouse, tr, r1, "24 小时制", Some("关闭后显示为「下午 9:10」"), s.clock_24h, SettingsHit::Clock24h, hits);
            ry += 48;
            let r2 = Rect { x: content.x + 18, y: ry, w: row_w + 12, h: 44 };
            toggle_row(buf, w, h, mouse, tr, r2, "显示秒", Some("顶栏时钟精确到秒"), s.clock_seconds, SettingsHit::ClockSeconds, hits);
            ry += 54;

            // 时区微调：用两个按钮而不是滑杆 —— 时区是离散值、且要能精确到半小时
            let back = Rect { x: content.x + 24, y: ry + 6, w: 40, h: 30 };
            let fwd = Rect { x: content.x + 72, y: ry + 6, w: 40, h: 30 };
            widgets::button(buf, w, h, back, "-1", widgets::BtnKind::Secondary, widgets::States::at(back, mouse), Some(tr));
            widgets::button(buf, w, h, fwd, "+1", widgets::BtnKind::Secondary, widgets::States::at(fwd, mouse), Some(tr));
            tr.draw(buf, w, h, (content.x + 124) as f32, tr.vcenter(ry as f32, 42.0, font::CAPTION), "时区（半小时制）", font::CAPTION, color::text_dim(), 0.9);
            hits.push((back, SettingsHit::TzShift(-30)));
            hits.push((fwd, SettingsHit::TzShift(30)));
        }
        "输入" => {
            let r1 = Rect { x: content.x + 18, y: ry, w: row_w + 12, h: 44 };
            toggle_row(buf, w, h, mouse, tr, r1, "默认启用中文输入法", Some("开机即生效，Ctrl+空格 可随时切换"), s.ime_default, SettingsHit::ImeDefault, hits);
            ry += 58;
            widgets::group_header(buf, w, h, content.x + 24, ry, "键盘快捷键", Some(tr));
            ry += 22;
            for (k, v) in [
                ("Alt+1 … Alt+4", "切换窗口布局"),
                ("Ctrl+空格", "中英文切换"),
                ("Ctrl+Shift+C / V", "复制 / 粘贴"),
                ("Enter", "命令栏发送"),
                ("Esc", "关闭菜单或命令栏"),
            ] {
                tr.draw_bold(buf, w, h, (content.x + 24) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 190) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += 24;
            }
        }
        "AI" => {
            widgets::group_header(buf, w, h, content.x + 24, ry, "推理通道", Some(tr));
            ry += 24;
            for (k, v) in [
                ("aetherd", "本地服务（socket / TCP 7311）"),
                ("离线意图", "内置规则，断网可用"),
                ("动作执行", "布局 / 开应用 / 关窗口"),
                ("权限闸门", "危险动作需确认卡片"),
            ] {
                tr.draw_bold(buf, w, h, (content.x + 24) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 140) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += 24;
            }
            ry += 10;
            tr.draw(buf, w, h, (content.x + 24) as f32, ry as f32, "模型与密钥由 aetherd 管理，不经过合成器进程。", font::CAPTION, color::text_dim(), 0.8);
        }
        "关于" => {
            tr.draw_bold(buf, w, h, (content.x + 24) as f32, ry as f32, "AetherOS", font::TITLE, color::text(), 0.98);
            ry += 36;
            for (k, v) in [
                ("版本", env!("CARGO_PKG_VERSION")),
                ("许可证", "GPL-3.0-only"),
                ("架构", std::env::consts::ARCH),
                ("内核", "Linux（Buildroot 2024.02.1）"),
                ("设置文件", crate::settings::SETTINGS_PATH),
            ] {
                tr.draw_bold(buf, w, h, (content.x + 24) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 130) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += 24;
            }
        }
        _ => {}
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
fn draw_term_content(
    buf: &mut [u32],
    w: usize,
    h: usize,
    win_rect: Rect,
    term: Option<&crate::term::Terminal>,
    active: bool,
    t: f32,
    tr: Option<&TextRenderer>,
) {
    let r = term_content_rect(win_rect);
    // 终端底色固定深色（理由见 term::ansi_rgb）：内容区之外也要铺满，
    // 否则窗口体（浅色）会从内边距里透出来，看起来像没画完
    fill_clipped(buf, w, h, r, win_rect, radius::LG, TERM_BG, 1.0);

    let Some(tr) = tr else { return };
    let Some(term) = term else {
        draw_text(
            tr, buf, w, h, (r.x + 6) as f32, (r.y + 4) as f32,
            "（终端会话未建立）", font::MONO, color::text_faint(), 0.9,
        );
        return;
    };
    let (cell_w, cell_h) = tr.mono_cell();
    let (cols, rows) = term_grid_size(win_rect, cell_w, cell_h);

    // 拖选高亮：铺在字形**下面**（不是加在文字上），颜色与选中态一致
    if let Some(sel) = term.sel.as_ref() {
        let ((ax, ay), (bx, by)) = {
            let (sx, sy, ex, ey) = (sel.sx, sel.sy, sel.ex, sel.ey);
            let (ax, ay, bx, by) = if (sy, sx) <= (ey, ex) { (sx, sy, ex, ey) } else { (ex, ey, sx, sy) };
            ((ax.min(bx), ay.min(by)), (bx.max(ax), by.max(ay)))
        };
        for y in ay..=by.min(rows.saturating_sub(1)) {
            for x in ax.min(cols.saturating_sub(1))..=bx.min(cols.saturating_sub(1)) {
                let px = r.x as f32 + x as f32 * cell_w;
                let py = r.y as f32 + y as f32 * cell_h;
                fill_rect(
                    buf, w, h,
                    Rect { x: px as i32, y: py as i32, w: cell_w.ceil() as i32, h: cell_h as i32 },
                    color::accent(),
                    0.35,
                );
            }
        }
    }

    for y in 0..rows.min(term.screen.rows) {
        let py = r.y as f32 + y as f32 * cell_h;
        for x in 0..cols.min(term.screen.cols) {
            let cell = term.screen.cell(x, y);
            // 宽字符的第二格是占位：画了就重复，跳过才是对齐的
            if cell.wide_tail {
                continue;
            }
            let px = r.x as f32 + x as f32 * cell_w;
            if crate::term::cell_has_custom_bg(&cell) {
                fill_rect(
                    buf, w, h,
                    Rect { x: px as i32, y: py as i32, w: cell_w.ceil() as i32, h: cell_h as i32 },
                    crate::term::ansi_rgb(cell.bg, false),
                    1.0,
                );
            }
            if cell.ch == ' ' {
                continue; // 空格不画字形：整屏大半是空的，这一条省掉绝大部分绘制
            }
            let rgb = crate::term::ansi_rgb(cell.fg, cell.bold);
            let mut buf4 = [0u8; 4];
            let glyph = cell.ch.encode_utf8(&mut buf4);
            draw_text(tr, buf, w, h, px, py, glyph, font::MONO, rgb, 1.0);
        }
    }

    // 光标：只有活动窗口才闪（非活动窗口闪烁是干扰）
    if active {
        let cx = term.screen.cur_x.min(cols.saturating_sub(1));
        let cy = term.screen.cur_y.min(rows.saturating_sub(1));
        if (t * 2.0) as i32 % 2 == 0 {
            let px = r.x as f32 + cx as f32 * cell_w;
            let py = r.y as f32 + cy as f32 * cell_h;
            fill_rect(
                buf, w, h,
                Rect { x: px as i32, y: py as i32, w: cell_w.ceil() as i32, h: cell_h as i32 },
                crate::term::ansi_rgb(7, false),
                0.65,
            );
        }
    }

    // 会话状态角标：退出/不可用时必须说清楚，否则用户会以为"卡住了"
    match term.status() {
        crate::term::TermStatus::Running => {}
        crate::term::TermStatus::Exited(code) => {
            let msg = format!("会话已结束（状态 {code}）· 点 Dock 里的终端可再开一个");
            draw_text(
                tr, buf, w, h, (r.x + 6) as f32, (r.y + r.h - 18) as f32,
                &msg, font::LABEL, color::text_faint(), 0.95,
            );
        }
        crate::term::TermStatus::Unavailable(why) => {
            let shown = ellipsize(tr, why, font::LABEL, (r.w - 12) as f32);
            draw_text(
                tr, buf, w, h, (r.x + 6) as f32, (r.y + r.h - 18) as f32,
                &shown, font::LABEL, color::warning_text(), 0.95,
            );
        }
    }
}

/// 文件窗口内容：侧栏（深底座）→ 内容面（纸面）→ 状态条（深底座）三层。
#[allow(clippy::too_many_arguments)]
fn draw_files_content(
    buf: &mut [u32],
    w: usize,
    h: usize,
    r: Rect,
    win: Rect,
    mouse: (f32, f32),
    tr: Option<&TextRenderer>,
    fv: &FileView,
    cells: &mut Vec<(Rect, usize)>,
    up: &mut Rect,
    sidebar_hits: &mut Vec<(Rect, usize)>,
    crumb_hits: &mut Vec<(Rect, String)>,
) {
    *up = Rect { x: 0, y: 0, w: 0, h: 0 };
    const SIDEBAR_W: i32 = 150;
    const STATUS_H: i32 = 26;

    // 侧栏：比窗口体暗一档
    let sidebar = Rect { x: r.x, y: r.y, w: SIDEBAR_W.min(r.w / 2), h: r.h };
    fill_clipped(buf, w, h, sidebar, win, radius::LG, color::inset(), 0.66);

    // 内容面：比窗口体亮半档的"纸面"——三层明度差是纵深感的全部来源。
    // 几何走 `files_grid_rect`，与键盘导航用的是同一份计算
    let area_x = sidebar.x + sidebar.w;
    let surface = files_grid_rect(win);
    fill_clipped(buf, w, h, surface, win, radius::LG, color::surface_2(), 0.42);
    fill_rect(buf, w, h, Rect { x: area_x, y: r.y + 1, w: 1, h: r.h - 2 }, color::hairline(), 0.08);

    if let Some(tr) = tr {
        // 顶部"← 上级目录"：没有它就只能进不能出，导航不成立
        let mut base = r.y + 12;
        if !path_is_root(fv.cwd) {
            let row = Rect { x: r.x + 8, y: base, w: sidebar.w - 16, h: 26 };
            let hovered = row.contains(mouse.0, mouse.1);
            if hovered {
                rounded_rect(buf, w, h, row, radius::SM - 2.0, color::hairline(), state::hover());
            }
            draw_text(
                tr, buf, w, h,
                (row.x + 14) as f32,
                tr.vcenter(row.y as f32, row.h as f32, font::BODY),
                "← 上级目录", font::BODY,
                if hovered { color::text() } else { color::text_dim() },
                if hovered { 0.98 } else { 0.88 },
            );
            *up = row;
            base += 30;
        }
        for (i, (label, path)) in fv.sidebar.iter().enumerate() {
            let y = base + i as i32 * 30;
            // 当前所在位置高亮：用户随时知道"我在哪一栏"（此前恒为 false，等于没有反馈）
            let selected = path == fv.cwd || fv.cwd.starts_with(&format!("{path}{}", std::path::MAIN_SEPARATOR));
            let row = Rect { x: r.x + 8, y, w: sidebar.w - 16, h: 26 };
            sidebar_hits.push((row, i));
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
                label, font::BODY,
                if selected { color::text() } else { color::text_dim() },
                if selected { 0.96 } else { 0.88 },
            );
        }
    }

    let shown = draw_icon_grid(buf, w, h, surface, win, mouse, tr, fv.entries, fv.selected, fv.scroll, cells);

    // 状态条：贴窗口底角
    let status = Rect { x: r.x, y: r.y + r.h - STATUS_H, w: r.w, h: STATUS_H };
    fill_clipped(buf, w, h, status, win, radius::LG, color::inset(), 0.62);
    fill_rect(buf, w, h, Rect { x: r.x + 1, y: status.y, w: r.w - 2, h: 1 }, color::hairline(), 0.08);
    if let Some(tr) = tr {
        // 读目录失败时优先显示原因 —— 否则用户看到的是"空目录"，与"没权限"无法区分
        let left = match fv.error {
            Some(e) => format!("无法读取目录：{e}"),
            None => {
                // 有滚动时给出**可见区间**（"1–8 / 76"），而不是只说"显示 8"——
                // 后者让用户不知道下面还有没有内容、也不知道自己看到哪儿了
                if fv.entries.len() > shown && shown > 0 {
                    format!("{}–{} / {} 项", fv.scroll + 1, fv.scroll + shown, fv.entries.len())
                } else {
                    format!("{} 项", fv.entries.len())
                }
            }
        };
        let left_rgb = if fv.error.is_some() { color::danger_text() } else { color::text_faint() };
        draw_text(tr, buf, w, h, (status.x + 12) as f32, tr.vcenter(status.y as f32, status.h as f32, font::LABEL), &left, font::LABEL, left_rgb, 0.9);
        // 右侧：**可点击的面包屑**（整条路径不再是一串只能看的字）。
        // 只保留能放下的末几段：从右侧往左累加宽度，放不下就停，并在前面补一个"…"。
        let segs = crumbs(fv.cwd);
        let sep_w = tr.measure(" › ", font::LABEL);
        let budget = (status.w / 2) as f32;
        let mut used = 0.0f32;
        let mut keep: Vec<usize> = Vec::new();
        for (i, (name, _)) in segs.iter().enumerate().rev() {
            let wseg = tr.measure(name, font::LABEL) + if keep.is_empty() { 0.0 } else { sep_w };
            if used + wseg > budget {
                break;
            }
            used += wseg;
            keep.push(i);
        }
        keep.reverse();
        let mut x = (status.x + status.w - 12) as f32 - used;
        for (n, &i) in keep.iter().enumerate() {
            let (name, path) = &segs[i];
            if n > 0 {
                draw_text(tr, buf, w, h, x, tr.vcenter(status.y as f32, status.h as f32, font::LABEL), " › ", font::LABEL, color::text_faint(), 0.7);
                x += sep_w;
            }
            let last = i + 1 == segs.len();
            let wname = tr.measure(name, font::LABEL);
            // 末段（当前目录）用主色强调，其余是次要色
            let rgb = if last { color::text_dim() } else { color::text_faint() };
            draw_text(tr, buf, w, h, x, tr.vcenter(status.y as f32, status.h as f32, font::LABEL), name, font::LABEL, rgb, if last { 0.95 } else { 0.9 });
            crumb_hits.push((
                Rect { x: x as i32, y: status.y, w: wname.ceil() as i32, h: status.h },
                path.clone(),
            ));
            x += wname;
        }

    }
}

/// 图标网格：图标 + 标签（无卡片外框——"框里再放块"是廉价感的来源之一）。
/// 行列数按可用空间自适应，把窗口填满，不留下大片死灰。返回实际画出的项数。
const GRID_ICON: i32 = 46;
const GRID_CELL_W: i32 = 96;
const GRID_CELL_H: i32 = 84;
const GRID_GAP_X: i32 = 10;
const GRID_GAP_Y: i32 = 12;
const GRID_PAD: i32 = 14;

/// 图标网格的几何。渲染与键盘导航**共用同一份计算** —— 否则"方向键移动几列"
/// 和"画出来几列"迟早会对不上（两处各写一套几何是这类 bug 的温床）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridLayout {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: i32,
    pub cell_h: i32,
    /// 网格左上角
    pub ox: i32,
    pub oy: i32,
}

impl GridLayout {
    /// 一屏能放多少条
    pub fn page_size(&self) -> usize {
        self.cols * self.rows
    }

    /// 第 `i` 个格子（`i` 为页内序号）的矩形
    pub fn cell(&self, i: usize) -> Rect {
        let col = (i % self.cols.max(1)) as i32;
        let row = (i / self.cols.max(1)) as i32;
        Rect {
            x: self.ox + col * (GRID_CELL_W + GRID_GAP_X),
            y: self.oy + row * (GRID_CELL_H + GRID_GAP_Y),
            w: GRID_CELL_W,
            h: GRID_CELL_H,
        }
    }
}

/// 文件窗口的网格区（= 内容面，去掉侧栏与状态条）。
/// main.rs 的键盘导航要拿它算页大小，所以必须与绘制共用。
pub fn files_grid_rect(win: Rect) -> Rect {
    const SIDEBAR_W: i32 = 150;
    const STATUS_H: i32 = 26;
    let content_y = win.y + metric::TITLE_H + 1;
    let content_h = win.h - metric::TITLE_H - 2;
    let x0 = win.x + 1 + SIDEBAR_W.min((win.w - 2) / 2);
    Rect {
        x: x0,
        y: content_y,
        w: (win.x + win.w - 1 - x0).max(0),
        h: (content_h - STATUS_H).max(0),
    }
}

/// 路径 → 面包屑：(显示名, 目标路径)。Windows 盘符与 Unix 根都处理。
///
/// 纯函数，便于单测 —— 路径解析是最容易出边界 bug 的地方（盘符、根、结尾分隔符）。
pub fn crumbs(path: &str) -> Vec<(String, String)> {
    let sep = if path.contains('\\') { '\\' } else { '/' };
    let mut out = Vec::new();
    let mut acc = String::new();
    for (i, part) in path.split(sep).filter(|s| !s.is_empty()).enumerate() {
        if i == 0 {
            acc = if part.ends_with(':') {
                format!("{part}{sep}")
            } else {
                format!("{sep}{part}")
            };
        } else if acc.ends_with(sep) {
            acc.push_str(part);
        } else {
            acc = format!("{acc}{sep}{part}");
        }
        out.push((part.to_string(), acc.clone()));
    }
    out
}

/// 终端窗口里能放下的列/行数。
///
/// 渲染与"同步给 PTY 的 winsize"必须用**同一个**结果 —— 否则 `vim` 会按
/// 一个尺寸排版、而画面按另一个尺寸裁剪，出现谁也说不清的对不齐。
pub fn term_grid_size(win: Rect, cell_w: f32, cell_h: f32) -> (usize, usize) {
    let r = term_content_rect(win);
    let cols = (r.w as f32 / cell_w.max(1.0)).floor().max(1.0) as usize;
    let rows = (r.h as f32 / cell_h.max(1.0)).floor().max(1.0) as usize;
    (cols, rows)
}

/// 终端内容区（窗口体去掉标题栏与内边距）。
pub fn term_content_rect(win: Rect) -> Rect {
    let pad = 8;
    Rect {
        x: win.x + 1 + pad,
        y: win.y + metric::TITLE_H + 1 + pad,
        w: (win.w - 2 - pad * 2).max(1),
        h: (win.h - metric::TITLE_H - 2 - pad * 2).max(1),
    }
}

/// 终端底色（两种主题都用深底，理由见 `term::ansi_rgb` 的注释）。
const TERM_BG: [u8; 3] = [18, 20, 26];

/// 计算网格布局；空间不足时返回 0 列（调用方据此不画任何格子）。
pub fn grid_layout(area: Rect) -> GridLayout {
    let mut g = GridLayout { cols: 0, rows: 0, cell_w: GRID_CELL_W, cell_h: GRID_CELL_H, ox: area.x, oy: area.y };
    let avail_w = area.w - GRID_PAD * 2;
    let avail_h = area.h - GRID_PAD * 2;
    if avail_w < GRID_CELL_W || avail_h < GRID_CELL_H {
        return g;
    }
    g.cols = (avail_w / (GRID_CELL_W + GRID_GAP_X)).clamp(1, 5) as usize;
    g.rows = (avail_h / (GRID_CELL_H + GRID_GAP_Y)).clamp(1, 5) as usize;
    let grid_w = g.cols as i32 * GRID_CELL_W + (g.cols as i32 - 1) * GRID_GAP_X;
    let grid_h = g.rows as i32 * GRID_CELL_H + (g.rows as i32 - 1) * GRID_GAP_Y;
    g.ox = area.x + (area.w - grid_w) / 2;
    g.oy = area.y + (area.h - grid_h) / 2;
    g
}

/// 让 `selected` 落在以 `scroll` 为首页的可视范围内，返回修正后的 `scroll`。
///
/// 纯函数：方向键移动、翻页、Home/End 之后都过它归一化，
/// 保证"选中项永远可见"这条规则只有一处实现（也便于单测）。
pub fn scroll_to_show(selected: usize, scroll: usize, per_page: usize) -> usize {
    if per_page == 0 {
        return 0;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + per_page {
        selected + 1 - per_page
    } else {
        scroll
    }
}

/// 文件名网格：图标 + 标签（无卡片外框——"框里再放个块"是廉价感的来源之一）。
/// 从 `scroll` 开始铺满可视页，返回实际画出的项数。
fn draw_icon_grid(buf: &mut [u32], w: usize, h: usize, area: Rect, win: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>, entries: &[FsEntry], selected: Option<usize>, scroll: usize, cells: &mut Vec<(Rect, usize)>) -> usize {
    let Some(tr) = tr else { return 0 };
    let g = grid_layout(area);
    let per_page = g.page_size();
    if per_page == 0 {
        return 0;
    }
    // 只铺可视页：从 scroll 开始，画满一页为止
    let start = scroll.min(entries.len());
    let end = (start + per_page).min(entries.len());

    for (slot, i) in (start..end).enumerate() {
        let cell = g.cell(slot);
        let is_sel = selected == Some(i);
        let hovered = cell.contains(mouse.0, mouse.1);
        // 记下命中区：main.rs 的点击处理据此判断"点了哪个条目"
        cells.push((cell, i));
        // 选中比 hover 更"实"：底色 + 描边都用强调色，一眼区分"只是路过"和"已选中"
        if is_sel {
            fill_clipped(buf, w, h, cell, win, radius::LG, color::accent(), 0.20);
            rounded_outline(buf, w, h, cell, radius::LG, color::accent(), 0.55);
        } else if hovered {
            fill_clipped(buf, w, h, cell, win, radius::LG, color::hairline(), state::hover());
        }
        let icon_x = cell.x + (GRID_CELL_W - GRID_ICON) / 2;
        let icon_y = cell.y + 8;
        if entries[i].is_dir {
            draw_folder_icon(buf, w, h, icon_x, icon_y, GRID_ICON);
        } else {
            draw_file_icon(buf, w, h, icon_x, icon_y, GRID_ICON);
        }

        let label_y = icon_y + GRID_ICON + 4;
        let shown = ellipsize(tr, &entries[i].name, font::LABEL, (GRID_CELL_W - 10) as f32);
        let lw = tr.measure(&shown, font::LABEL);
        draw_text(
            tr, buf, w, h,
            cell.x as f32 + (GRID_CELL_W as f32 - lw) / 2.0,
            tr.vcenter(label_y as f32, 14.0, font::LABEL),
            &shown, font::LABEL,
            if is_sel || hovered { color::text() } else { color::text_dim() },
            if is_sel { 1.0 } else if hovered { 0.98 } else { 0.94 },
        );
    }

    // 细滚动条：内容超过一页时出现。位置即"看到哪儿了"——
    // 没有它用户不知道下面还有东西（此前状态栏只写「显示 N」，等于让用户自己猜）。
    if entries.len() > per_page && area.h > 60 {
        let track_h = area.h - 26;
        let thumb_h = ((track_h as usize * per_page / entries.len()) as i32).max(20);
        let max_scroll = entries.len() - per_page;
        let t = if max_scroll == 0 { 0.0 } else { scroll as f32 / max_scroll as f32 };
        let ty = area.y + 13 + ((track_h - thumb_h) as f32 * t) as i32;
        let x = area.x + area.w - 6;
        rounded_rect(buf, w, h, Rect { x, y: area.y + 13, w: 3, h: track_h }, 1.5, color::hairline(), 0.07);
        rounded_rect(buf, w, h, Rect { x, y: ty, w: 3, h: thumb_h }, 1.5, color::hairline(), 0.26);
    }
    end - start
}

/// 文本预览窗口一屏能显示多少行。
///
/// **必须与 `draw_preview_content` 的算法一致** —— 主循环用它算翻页步长，
/// 渲染用它算可见区间；两处不一致会导致翻页跳过或重复内容。
pub fn preview_visible_lines(rect: Rect) -> usize {
    const LINE_H: i32 = 18;
    ((rect.h - 20) / LINE_H).max(1) as usize
}

/// 文本预览窗口：行号栏 + 内容行。
///
/// 出错时**在窗口内显示原因**（而不是空白）—— "打不开"和"文件是空的"必须能区分，
/// 否则用户只会看到一个空窗口，不知道该重试还是该换个文件。
fn draw_preview_content(buf: &mut [u32], w: usize, h: usize, r: Rect, data: Option<&PreviewData>, tr: Option<&TextRenderer>) {
    fill_clipped(buf, w, h, r, r, radius::LG, color::surface_2(), 0.42);
    let Some(tr) = tr else { return };

    let Some(d) = data else {
        draw_text(tr, buf, w, h, (r.x + 16) as f32, (r.y + 14) as f32, "（无内容）", font::BODY, color::text_faint(), 0.9);
        return;
    };

    if let Some(err) = &d.error {
        draw_text(tr, buf, w, h, (r.x + 16) as f32, (r.y + 14) as f32, "无法预览", font::BODY, color::danger_text(), 0.95);
        draw_text(tr, buf, w, h, (r.x + 16) as f32, (r.y + 40) as f32, err, font::CAPTION, color::text_dim(), 0.9);
        return;
    }

    const LINE_H: i32 = 18;
    const GUTTER: i32 = 52;
    let max_lines = preview_visible_lines(r);
    // 滚动偏移在这里**只读地**夹取：窗口缩放后原偏移可能越界，
    // 而渲染函数拿的是 `&PreviewData`，不该改状态（状态由主循环改）
    let total = d.lines.len();
    let max_off = total.saturating_sub(max_lines);
    let start = d.scroll.offset().min(max_off);
    let end = (start + max_lines).min(total);

    // 行号栏
    fill_clipped(buf, w, h, Rect { x: r.x, y: r.y, w: GUTTER, h: r.h }, r, radius::LG, color::inset(), 0.5);
    fill_rect(buf, w, h, Rect { x: r.x + GUTTER, y: r.y + 1, w: 1, h: r.h - 2 }, color::hairline(), 0.08);

    for (slot, i) in (start..end).enumerate() {
        let ly = (r.y + 8 + slot as i32 * LINE_H) as f32;
        // 行号用**真实行号**（滚动后仍与源文件一一对应）
        let num = format!("{}", i + 1);
        let nw = tr.measure(&num, font::LABEL);
        draw_text(tr, buf, w, h, (r.x + GUTTER - 10) as f32 - nw, ly, &num, font::LABEL, color::text_faint(), 0.7);
        // 预览不换行：行号必须与源文件行号一一对应，换行会打乱这个对应关系
        let shown = ellipsize(tr, &d.lines[i], font::LABEL, (r.w - GUTTER - 20) as f32);
        draw_text(tr, buf, w, h, (r.x + GUTTER + 10) as f32, ly, &shown, font::LABEL, color::text(), 0.92);
    }

    // 三种情况都要给提示：向上滚过、还有下文、**文件被截断**
    if start > 0 || end < total || d.truncated {
        let msg = if total == 0 {
            "… 空文件".to_string()
        } else if d.truncated {
            // 截断时 "共 N 行" 是**已载入的**行数，不是文件的真实行数 ——
            // 不加这句，用户会以为文件就这么长（改滚动时差点丢掉这个区分）
            format!(
                "… 第 {}-{} 行 / 已载入 {total} 行（文件过长，未全部载入）",
                start + 1,
                end
            )
        } else {
            format!("… 第 {}-{} 行 / 共 {total} 行", start + 1, end)
        };
        draw_text(tr, buf, w, h, (r.x + GUTTER + 10) as f32, (r.y + r.h - 24) as f32, &msg, font::LABEL, color::text_faint(), 0.85);
    }

    // 右下角显示完整路径：标题栏只放得下文件名，"这文件到底在哪"是打开后第一个疑问
    let pw = tr.measure(&d.path, font::LABEL);
    let px = (r.x + r.w - 12) as f32 - pw;
    if px > (r.x + GUTTER + 10) as f32 {
        draw_text(tr, buf, w, h, px, (r.y + r.h - 24) as f32, &d.path, font::LABEL, color::text_faint(), 0.8);
    }
}

/// Wayland 客户端窗口：把客户端提供的 RGBA 像素画进内容区。
///
/// **不缩放**：客户端的 buffer 多大就画多大，超出窗口的裁掉，小于窗口的露底色
/// —— 拉伸会让客户端以为自己的尺寸判断是对的，而"窗口比 buffer 小"恰恰是
/// 合成器应该让客户端通过 configure 事件知道的事，不是靠悄悄变形来遮掩。
///
/// alpha 走 `blend_pixel`：客户端可以画半透明（ARGB8888），XRGB 的 alpha 恒 255。
/// framebuffer 越界由 `blend_pixel` 的 `idx >= buf.len()` 兜底（窗口贴边时 dy/dx
/// 可能算出负数行，先在循环里裁掉）。
fn draw_wayland_content(
    buf: &mut [u32],
    w: usize,
    h: usize,
    clip: Rect,
    surface: Option<&WaylandSurface>,
) {
    fill_clipped(buf, w, h, clip, clip, radius::LG, color::surface_1(), 0.96);
    let Some(s) = surface else { return };
    let need = (s.width as usize) * (s.height as usize) * 4;
    if s.width == 0 || s.height == 0 || s.pixels.len() < need {
        return; // 没内容或数据不完整：露底色（Pending pool 的 commit 也走这里）
    }
    for row in 0..s.height as i64 {
        let dy = clip.y as i64 + row;
        if dy < clip.y as i64 || dy >= clip.y as i64 + clip.h as i64 {
            continue;
        }
        for col in 0..s.width as i64 {
            let dx = clip.x as i64 + col;
            if dx < clip.x as i64 || dx >= clip.x as i64 + clip.w as i64 {
                continue;
            }
            let p = (row as usize * s.width as usize + col as usize) * 4;
            let rgb = [s.pixels[p], s.pixels[p + 1], s.pixels[p + 2]];
            let a = s.pixels[p + 3] as f32 / 255.0;
            blend_pixel(buf, dy as usize * w + dx as usize, rgb, a);
        }
    }
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

/// 普通文件图标：冷白纸面 + 右上折角 + 三条文字线。
///
/// 与文件夹刻意用不同色系（文件偏中性灰、文件夹偏蓝），
/// 这样一屏几十个图标扫过去，类型是"看得出"的，不用读标签。
fn draw_file_icon(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, size: i32) {
    let top = [250, 251, 254];
    let bottom = [216, 221, 232];
    let fold = ((size as f32) * 0.30) as i32;
    let body_y = y + 3;
    let body_h = size - 3;
    gradient_tile(buf, w, Rect { x, y: body_y, w: size, h: body_h }, 3.0, top, bottom, 0.97);
    // 折角：右上角压暗一块，读起来像纸被折了过去
    rounded_rect(buf, w, h, Rect { x: x + size - fold, y: body_y, w: fold, h: fold }, 2.0, [198, 205, 220], 0.95);
    fill_rect(buf, w, h, Rect { x: x + 3, y: body_y + 1, w: size - 6, h: 1 }, color::HIGHLIGHT, 0.45);
    // 三条横线暗示"里面有内容"
    let line = [178, 186, 202];
    for i in 0..3 {
        let ly = body_y + body_h / 2 + i * 5 - 4;
        fill_rect(buf, w, h, Rect { x: x + 8, y: ly, w: size - 16 - i * 4, h: 1 }, line, 0.5);
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
    shadow(buf, w, h, bar, radius::LG, elevation::elev_2());
    // 2026-09-29 P2：从"26px 大胶囊"改成**命令栏形态**（radius::LG = 8px，
    // 与 Windows 11 搜索框/输入框同一规范）。胶囊 + 渐变环是最典型的 AI 产品观感。
    rounded_rect(buf, w, h, bar, radius::LG, color::glass(), color::glass_alpha(hovered));
    if ui.ai_focused {
        // 焦点环：**只在思考中脉动**（那是真实状态），空闲时是静态环 —— 此前无条件呼吸，
        // 属于纯装饰动效。
        let ring = if ui.ai_thinking {
            0.45 + 0.25 * (t * 3.0).sin()
        } else {
            0.65
        };
        gradient_outline(buf, w, h, bar, radius::LG, color::accent(), color::accent(), ring);
    } else {
        rounded_outline(buf, w, h, bar, radius::LG, color::hairline(), if hovered { 0.22 } else { 0.14 });
    }

    let Some(tr) = tr else { return };

    // 渐变圆形徽标（AI 元素：极光的容许出口之一）
    let av = Rect { x: bar.x + 12, y: bar.y + 10, w: 32, h: 32 };
    rounded_rect(buf, w, h, av, radius::MD, color::accent(), 0.95);
    let aw = tr.measure_bold("A", font::GLYPH);
    tr.draw_bold(buf, w, h, av.x as f32 + (av.w as f32 - aw) / 2.0, tr.vcenter(av.y as f32, av.h as f32, font::GLYPH), "A", font::GLYPH, color::text(), 0.98);

    let text_x = (bar.x + 58) as f32;
    let text_y = tr.vcenter(bar.y as f32, bar.h as f32, font::BODY);
    // 可见窗口按**光标**位置算，而不是"保证末尾可见" —— 2.4 支持中间编辑后，
    // 光标可能在任意位置，"末尾可见"对用户没有意义
    let max_w = bar.x as f32 + bar.w as f32 - 96.0 - text_x;
    let (shown, prefix) = visible_window(tr, input, ui.ai_cursor, max_w);
    if input.is_empty() {
        draw_text(tr, buf, w, h, text_x, text_y, strings::AI_BAR_HINT, font::BODY, color::text_dim(), 0.85);
    } else {
        draw_text(tr, buf, w, h, text_x, text_y, shown, font::BODY, color::text(), 0.95);
    }

    // 输入光标：x 由"光标前那一段"的宽度决定
    let caret_x = text_x + tr.measure(prefix, font::BODY) + 4.0;

    // 输入法候选框（2.3）：画在指令条**上方** —— 贴着输入位置，视线不用来回跳
    if ui.ime.show_candidates() {
        draw_ime_candidates(buf, w, h, bar, ui.ime, tr);
    }
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

/// 输入法候选框（2.3）：画在指令条上方。
///
/// 每项带数字前缀 —— 数字键选词是最快的路径，界面上要看得见这个可能性。
fn draw_ime_candidates(
    buf: &mut [u32],
    w: usize,
    h: usize,
    bar: Rect,
    ime: &crate::ime::Ime,
    tr: &TextRenderer,
) {
    let cands = ime.candidates();
    if cands.is_empty() {
        return;
    }
    let pad = 10i32;
    let gap = 6i32;
    let widths: Vec<f32> = cands.iter().map(|c| tr.measure(c, font::BODY)).collect();
    let content: f32 = widths.iter().sum::<f32>()
        + (pad * 2 * cands.len() as i32) as f32
        + (gap * (cands.len() as i32 - 1)).max(0) as f32;
    let box_h = 36i32;
    let rect = Rect {
        x: bar.x + 58, // 与指令条里的文字左对齐
        y: bar.y - box_h - 8,
        w: content.ceil() as i32,
        h: box_h,
    };
    shadow(buf, w, h, rect, radius::MD, elevation::elev_2());
    rounded_rect(buf, w, h, rect, radius::MD, color::surface_1(), 0.98);
    rounded_outline(buf, w, h, rect, radius::MD, color::hairline(), 0.20);

    // 拼音串：用户要知道自己打了什么（打错时才看得出来）
    draw_text(
        tr, buf, w, h,
        rect.x as f32,
        (rect.y - 18) as f32,
        ime.buffer(),
        font::LABEL,
        color::text_faint(),
        0.9,
    );

    let ty = tr.vcenter(rect.y as f32, box_h as f32, font::BODY);
    let mut x = rect.x + pad;
    for (i, c) in cands.iter().enumerate() {
        let cw = widths[i];
        if i == ime.selected() {
            rounded_rect(
                buf, w, h,
                Rect { x: x - 5, y: rect.y + 5, w: cw as i32 + 10, h: box_h - 10 },
                6.0,
                color::accent(),
                0.85,
            );
        }
        // 序号（数字键选词）
        draw_text(
            tr, buf, w, h,
            x as f32,
            (rect.y + 2) as f32,
            &format!("{}", i + 1),
            font::CAPTION,
            color::text_faint(),
            0.75,
        );
        let fg = if i == ime.selected() { color::text() } else { color::text_dim() };
        draw_text(tr, buf, w, h, x as f32, ty, c, font::BODY, fg, 0.95);
        x += cw as i32 + pad * 2 + gap;
    }
}

/// 指令条的可见窗口：保证**光标**（第 `caret` 个字符之前）落在窗口内。
///
/// 返回 `(要画的整段, 光标前那一段)` —— 后者用来定位光标的 x。
/// 返回 `&str` 切片而非 `String`：这个函数**每帧都跑**，不该制造分配。
fn visible_window<'a>(tr: &TextRenderer, text: &'a str, caret: usize, max_w: f32) -> (&'a str, &'a str) {
    let caret_byte = text
        .char_indices()
        .nth(caret)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let mut start = 0usize;
    // 向右推起点直到宽度合适，但**不越过光标** —— 否则光标会被推出可视区
    while start < caret_byte {
        if tr.measure(&text[start..], font::BODY) <= max_w {
            break;
        }
        start += text[start..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    (&text[start..], &text[start..caret_byte])
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
        BubbleKind::Ai => gradient_outline(buf, w, h, r, 18.0, color::accent(), color::accent(), alpha * 0.55),
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
        // 已装应用插在**内建图标与"安装向导"之间** —— 安装向导必须留在最后，
        // 因为 Live ISO 里它是唯一需要被一眼找到的动作。
        // 数据来自 main.rs::load_installed_apps（扫 /var/apps）。
        apps.extend(self.installed_apps.iter().map(|(_, name)| name.as_str()));
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
                // 安装是 Live ISO 里唯一需要被一眼找到的动作：琥珀警示色（P0：纯色去渐变）
                rounded_rect(buf, w, h, Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON }, radius::MD, [196, 142, 67], if hovered { 1.0 } else { 0.92 });
            } else {
                // 图标底座（P0：纯色，去"上亮下暗"渐变与紫色系——
                // "彩色渐变底座"正是 AI 产品味的来源之一）
                let base: [u8; 3] = if i >= DOCK_BUILTINS {
                    [61, 149, 149] // 已装应用：青绿，与内建一眼可分
                } else {
                    match i {
                        0 => [99, 164, 206],  // 文件：青蓝
                        1 => [82, 118, 168],  // 终端：钢青
                        2 => [88, 148, 224],  // 浏览器：蓝
                        3 => [108, 122, 148], // 音乐：雾灰蓝（P0：由紫色改）
                        _ => [116, 125, 147], // 设置：蓝灰
                    }
                };
                rounded_rect(buf, w, h, Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON }, radius::MD, base, 0.97);
                // 顶部内高光（P0：0.24 → 0.08，去"玻璃高光"感）
                fill_rect(buf, w, h, Rect { x: ix + 4, y: iy + 1, w: metric::DOCK_ICON - 8, h: 1 }, color::HIGHLIGHT, 0.08);
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

/// 缩放光标（双头箭头）。`horizontal = true` → 左右拉伸（↔），否则上下（↕）。
///
/// 为什么需要：窗口边缘早就能拖拽缩放（`main.rs::topmost_edge` 的 6px 抓取带），
/// 但光标恒为箭头 —— 用户不可能知道边缘能拖。与 `draw_cursor` 同一手法：
/// 像素谓词 + 白填充 + 黑描边，不依赖字体字形。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn draw_resize_cursor(
    buf: &mut [u32],
    w: usize,
    h: usize,
    mx: f32,
    my: f32,
    horizontal: bool,
) {
    let (cx, cy) = (mx.round() as i32, my.round() as i32);
    // 以光标为中心的双头箭头：中轴杆 + 两端三角头。
    // 垂直箭头 = 把坐标对调（同一套谓词，避免两份形状定义走样）。
    let inside = |dx: i32, dy: i32| -> bool {
        let (dx, dy) = if horizontal { (dx, dy) } else { (dy, dx) };
        let head = dx.abs();
        if dy.abs() <= 1 && head <= 6 {
            return true; // 杆
        }
        if (5..=8).contains(&head) {
            return dy.abs() <= 9 - head; // 三角头：越靠外越窄
        }
        false
    };
    for dy in -10..=10 {
        for dx in -10..=10 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                continue;
            }
            let idx = y as usize * w + x as usize;
            if inside(dx, dy) {
                blend_pixel(buf, idx, [255, 255, 255], 0.95);
            } else if inside(dx - 1, dy)
                || inside(dx + 1, dy)
                || inside(dx, dy - 1)
                || inside(dx, dy + 1)
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
    use super::{dirty_rows, draw_background, draw_background_rows};
    use crate::draw::theme::color;

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

    /// 分帧生成（行带）必须与整屏一次生成**逐像素一致**。
    ///
    /// 这条不是凑数：壁纸改成分帧生成后，交互路径与 `--shot` 截图走了不同代码路径
    /// （前者分帧、后者 [`Renderer::prepare_background`] 整屏）。两者只要有差异，
    /// 就会出现"截图和真机不一样"这种最难查的 bug。
    /// 设置中心：**渲染一次并直接断言**（走查图的 diff 只能证明"场景变了"，
    /// 证明不了窗口内部结构画对了 —— 两栏布局、控件命中区、不可用页）。
    #[test]
    fn settings_content_renders_two_columns_and_registers_controls() {
        // 本测试所在的模块只导入了几何相关的名字，这里显式引入所需的类型
        use crate::draw::{draw_settings_content, Rect};
        use crate::text::TextRenderer;
        let Some(tr) = TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        // 铺白底：这样才能用"谁更暗"判断左栏（inset 底）与右侧页面
        for p in buf.iter_mut() {
            *p = 0x00FF_FFFF;
        }
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        let page = crate::settings::first_enabled_page();
        draw_settings_content(
            &mut buf, w, h, r, (0.0, 0.0), Some(&tr),
            crate::settings::Settings::default(), page, &mut hits,
        );

        // 1) 命中区要齐：页面行 + 本页的真控件
        use crate::settings::SettingsHit as H;
        assert!(
            hits.iter().any(|(_, x)| matches!(x, H::Page(_))),
            "左栏必须登记可切换的页面"
        );
        assert!(hits.iter().any(|(_, x)| matches!(x, H::Clock24h)), "24 小时制开关缺失");
        assert!(hits.iter().any(|(_, x)| matches!(x, H::ClockSeconds)), "显示秒开关缺失");
        assert!(hits.iter().any(|(_, x)| matches!(x, H::TzShift(30))), "时区 +1 缺失");
        assert!(hits.iter().any(|(_, x)| matches!(x, H::TzShift(-30))), "时区 -1 缺失");

        // 2) 命中区必须都在窗口内：越界会让点击落到别的控件上
        for (rr, id) in &hits {
            assert!(
                rr.x >= r.x && rr.y >= r.y && rr.x + rr.w <= r.x + r.w && rr.y + rr.h <= r.y + r.h,
                "命中区越界：{id:?} {rr:?}"
            );
            assert!(rr.w > 0 && rr.h > 0, "命中区不能为空：{id:?}");
        }

        // 3) 不可用的页不能出现在命中区里（点了不该有反应）
        let disabled = crate::settings::PAGES.iter().position(|p| !p.enabled).unwrap();
        assert!(
            !hits.iter().any(|(_, x)| matches!(x, H::Page(i) if *i == disabled)),
            "不可用页不该登记命中区"
        );

        // 4) 两栏结构：左栏底（inset 0.55）应比右侧页面（保持底色）暗
        let lum = |p: u32| ((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF);
        let y = 590; // 左栏页行在 ~390 之前结束，取靠下的空白处更稳
        let left = lum(buf[y * w + 80]);
        let right = lum(buf[y * w + 700]);
        assert!(
            left < right,
            "左栏应比右侧页面暗（两栏结构）：left={left} right={right}"
        );
    }

    /// 缩放光标：两种朝向都要画出像素，且在屏幕边缘（含 0,0 与右下角）不越界、不 panic。
    #[test]
    fn resize_cursor_draws_and_clamps_at_edges() {
        use crate::draw::{draw_cursor, draw_resize_cursor};
        let (w, h) = (64usize, 48usize);
        for (mx, my) in [(32.0, 24.0), (0.0, 0.0), (63.0, 47.0)] {
            for horizontal in [true, false] {
                let mut buf = vec![0u32; w * h];
                draw_resize_cursor(&mut buf, w, h, mx, my, horizontal);
                assert!(
                    buf.iter().any(|p| *p != 0),
                    "缩放光标应画出像素（{mx},{my} horizontal={horizontal}）"
                );
            }
            let mut b2 = vec![0u32; w * h];
            draw_cursor(&mut b2, w, h, mx, my);
            assert!(b2.iter().any(|p| *p != 0), "普通箭头应画出像素（{mx},{my}）");
        }
    }

    #[test]
    fn background_rows_match_full_generation() {
        let (w, h) = (37usize, 53usize); // 非 64 整数倍，逼出末段不足一次行数的边界
        for &mode in &[color::Mode::Light, color::Mode::Dark] {
            color::set_mode(mode);
            let (mut whole, mut banded) = (vec![0u32; w * h], vec![0u32; w * h]);
            draw_background(&mut whole, w, h, 1.2);
            let mut y = 0;
            while y < h {
                let y1 = (y + 8).min(h);
                draw_background_rows(&mut banded, w, h, 1.2, y, y1);
                y = y1;
            }
            assert_eq!(whole, banded, "mode={mode:?}: 分帧生成与整屏生成不一致");
        }
        color::set_mode(color::Mode::Light); // 恢复默认，避免影响后续测试
    }

    /// 越界的行带必须是无操作，不能 panic、不能写出缓冲区。
    #[test]
    fn background_rows_clamps_out_of_range() {
        let (w, h) = (8usize, 8usize);
        let mut buf = vec![0u32; w * h];
        draw_background_rows(&mut buf, w, h, 1.0, h, h + 5); // y0 == h
        draw_background_rows(&mut buf, w, h, 1.0, 100, 200); // 完全越界
        draw_background_rows(&mut buf, w, h, 1.0, 5, 2); // 反向区间
        assert!(buf.iter().all(|&p| p == 0), "越界行带不应写入任何像素");
    }
}
