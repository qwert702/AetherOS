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
/// docs/归档/界面设计方案（UI-DESIGN-PLAN）-2026-09-19.md §3：色板 / 字阶 / 圆角与阴影 / 交互态）。
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
        use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

        /// 主题模式。启动时设定一次，绘制期间只读。
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Mode {
            /// 明亮通透（默认）
            Light = 0,
            /// 深空
            Dark = 1,
        }

        static MODE: AtomicU8 = AtomicU8::new(Mode::Light as u8);
        /// 高对比度（无障碍 §3.4）。与主题模式**正交**：明亮/深色都能开高对比。
        static HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);

        pub fn set_mode(m: Mode) {
            MODE.store(m as u8, Ordering::Relaxed);
        }

        #[inline]
        pub fn is_light() -> bool {
            MODE.load(Ordering::Relaxed) == Mode::Light as u8
        }

        /// 开关高对比度（无障碍 §3.4）。启动时按用户设置设定一次。
        pub fn set_high_contrast(on: bool) {
            HIGH_CONTRAST.store(on, Ordering::Relaxed);
        }

        #[inline]
        pub fn high_contrast() -> bool {
            HIGH_CONTRAST.load(Ordering::Relaxed)
        }

        /// 高对比度下把**前景**色推向与背景相反的一端（纯函数，便于确定性测试）。
        ///
        /// 方向按主题决定：浅色主题的前景要**更深**，深色主题的要**更亮**。
        /// 如果无脑"推向极端"，浅色主题下的灰色文字会变得更亮、与白底对比更差 ——
        /// 那是帮倒忙，所以这里必须知道主题。
        pub fn push_fg(c: [u8; 3], light: bool) -> [u8; 3] {
            c.map(|v| {
                if light {
                    (v as u32 * 5 / 8) as u8
                } else {
                    (255 - (255 - v as u32) * 5 / 8) as u8
                }
            })
        }

        /// 高对比度下把**背景**色推向纯白（浅色）或纯黑（深色），与前景拉开距离。
        pub fn push_bg(c: [u8; 3], light: bool) -> [u8; 3] {
            c.map(|v| {
                if light {
                    (255 - (255 - v as u32) * 3 / 8) as u8
                } else {
                    (v as u32 * 3 / 8) as u8
                }
            })
        }

        /// 前景取色（高对比度时按方向推一把）。
        #[inline]
        fn fg(c: [u8; 3]) -> [u8; 3] {
            if high_contrast() {
                push_fg(c, is_light())
            } else {
                c
            }
        }

        /// 背景取色（高对比度时按方向推一把）。
        #[inline]
        fn bg(c: [u8; 3]) -> [u8; 3] {
            if high_contrast() {
                push_bg(c, is_light())
            } else {
                c
            }
        }

        // ---- WCAG 对比度（用于**客观**验证无障碍声明，而不是靠肉眼说"更清楚了"）----
        /// 相对亮度（WCAG 2.x 定义）。
        // 只在测试下编译：这两个函数是**验证无障碍声明**的工具，不是运行时功能。
        // 高对比度本身靠 `push_fg`/`push_bg` 生效；对不对要靠客观数字说话。
        // （标成 pub 却没人调用会被死代码检查拦下 —— 门禁把警告当失败。）
        #[cfg(test)]
        pub fn relative_luminance(c: [u8; 3]) -> f64 {
            fn ch(v: u8) -> f64 {
                let s = v as f64 / 255.0;
                if s <= 0.03928 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * ch(c[0]) + 0.7152 * ch(c[1]) + 0.0722 * ch(c[2])
        }

        /// 两色的对比度（1.0 ~ 21.0）。WCAG：AA 正文 4.5、AAA 正文 7.0。
        // 只在测试下编译：这两个函数是**验证无障碍声明**的工具，不是运行时功能。
        // 高对比度本身靠 `push_fg`/`push_bg` 生效；对不对要靠客观数字说话。
        // （标成 pub 却没人调用会被死代码检查拦下 —— 门禁把警告当失败。）
        #[cfg(test)]
        pub fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
            let (la, lb) = (relative_luminance(a), relative_luminance(b));
            let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
            (hi + 0.05) / (lo + 0.05)
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
            bg(pick([255, 255, 255], [27, 29, 33]))
        }
        /// 壁纸渐变底（极浅中性灰，几乎看不出渐变）
        pub fn bg_bottom() -> [u8; 3] {
            bg(pick([244, 245, 247], [23, 24, 28]))
        }
        /// 底座（菜单栏、侧栏、终端底、状态条、音乐面板）。
        /// 浅色下**必须比窗口体暗**，否则底座与窗口同色，所有区域糊成一片。
        pub fn inset() -> [u8; 3] {
            bg(pick([235, 237, 240], [30, 32, 41]))
        }
        /// 窗口/面板主面（纯白，与壁纸同色，靠描边区分）
        pub fn surface_1() -> [u8; 3] {
            bg(pick([255, 255, 255], [46, 49, 60]))
        }
        /// 内容"纸面"、卡片、浮层、侧栏选中
        pub fn surface_2() -> [u8; 3] {
            bg(pick([255, 255, 255], [64, 68, 80]))
        }
        /// 输入框、悬停中的控件
        pub fn surface_3() -> [u8; 3] {
            bg(pick([241, 243, 246], [80, 84, 97]))
        }
        /// 对比线：描边、分隔线、悬停叠层。
        /// 深色模式是白线，明亮模式是黑线——**所有调用点的 alpha 无需改动**。
        pub fn hairline() -> [u8; 3] {
            fg(pick([16, 18, 24], [255, 255, 255]))
        }
        /// 主文字
        pub fn text() -> [u8; 3] {
            fg(pick([26, 28, 34], [240, 241, 246]))
        }
        /// 次要文字
        pub fn text_dim() -> [u8; 3] {
            fg(pick([94, 98, 108], [180, 182, 192]))
        }
        /// 占位/禁用文字
        pub fn text_faint() -> [u8; 3] {
            fg(pick([146, 150, 160], [136, 139, 150]))
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
            bg(pick([255, 255, 255], [30, 32, 41]))
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

        // —— 标题栏按钮（2026-10-01 起为 Windows 风格三键）——
        //
        // 红绿灯的 close/min/zoom 三色已删除：换成 Windows 按钮后不再需要
        // （Windows 的按钮是**灰底 + 细线符号**，唯一有颜色的是关闭键悬停）。
        /// 关闭按钮悬停底色（Windows 11 的 #C42B1C）
        pub fn close_hover() -> [u8; 3] {
            [196, 43, 28]
        }

        // —— 2026-09-29 P0：极光/粉彩壁纸（aurora_* 与 vignette）移除，静态壁纸见 draw_background_rows ——
    }

    /// 字阶 §3.2：5 档（+ 图标字形档）。11px 是可读下限，不得更小。
    /// 界面缩放（FEATURE-GAPS §3.4「字号」）。
///
/// `Settings::ui_scale_pct` 这个字段一直存在、也能落盘，但**从来没有生效** ——
/// 与 `locale`、`notify_sound` 同一类"存了不用"。本模块把它接上。
///
/// ## 为什么缩放做在文本渲染器里，而不是调用点
///
/// `font::BODY` 这类常量被**几百处**调用点引用。要缩放就在调用点逐个乘系数，
/// 既不可能改全，也不可能长期维持（新写一处忘了乘，那处就不跟着变）。
/// 所以在 `TextRenderer` 的内部入口统一乘 —— 调用点写的是"我要正文大小"，
/// "正文实际多大"由这里决定。
///
/// ## 已知边界（如实写明）
///
/// 本批**只缩放字号，不缩放布局尺寸**（行高、标题栏、边距都还是固定值）。
/// 原因：那些是散落的字面量与 `const`，改起来是另一个量级的重构。
/// 后果：150% 时文字会明显更挤（有护栏测试保证**不重叠**：关键字号仍小于它所在的
/// 固定行高）。真要做到"整体缩放"，得先有一份集中的尺寸表 —— 那是独立批次。
/// 布局**尺寸表**（§2.4「整体缩放」）。
///
/// ## 为什么要有它
///
/// [`ui`] 只缩放**字号**（见那里的说明：几百处调用点，逐个乘系数改不全也维持不住）。
/// 布局尺寸（行高、间距、边距）则散在 `draw.rs` 的几百处字面量里 ——
/// 要"整体缩放"就必须先有一份**集中的表**，否则每加一个页面就多一处不跟着缩放的数字。
///
/// ## 为什么是"挑出来的"，不是把 600 多个字面量机械替换
///
/// `draw.rs` 里的数字分两类，**只有一类该跟着缩放**：
///
/// * **尺寸**（行高、控件高、间距、内容边距）→ 走 [`px`]；
/// * **像素级细节**（发丝分隔线 `h: 1`、阴影偏移 `x + 1`、文本基线微调）→ **不缩放**。
///   1 像素的分隔线在 125% 下还应该是 1 像素，否则会糊成 1.25 像素的边。
///
/// 所以这张表只收**命名得清楚、且确实成体系**的那几个值；其余等用到时再逐个判断。
pub mod size {
    use std::sync::atomic::{AtomicU16, Ordering};

    /// 内容左边距（页正文、卡片内容都从这里开始）。
    pub const CONTENT_PAD: i32 = 24;
    /// 控件相对内容区的左缩进（比 `CONTENT_PAD` 小，让控件看起来"嵌在"内容里）。
    pub const CONTROL_INSET: i32 = 18;
    /// 通用行高（设置页的一行控件）。
    pub const ROW_H: i32 = 40;
    /// 次级行高（较矮的按钮/条目）。
    pub const ROW_H_SM: i32 = 36;
    /// 分组标题与上一块内容之间的间距。
    pub const GROUP_GAP: i32 = 22;
    /// 相邻两行之间的间距。
    pub const LINE_GAP: i32 = 24;
    /// 说明文字与其上方控件之间的间距。
    pub const HINT_GAP: i32 = 26;
    /// 小间距（图标与文字之间之类）。
    pub const TIGHT: i32 = 12;

    // ---- 间距阶梯 ----
    //
    // 布局里反复出现的间距都取自这几级。**命名按"用在哪"，不按"多大"** ——
    // 这样以后加页面时能对着位置挑，而不是"上次那个 44 是用在哪来着"。
    /// 小节内的紧凑间距。
    pub const GAP_TIGHT: i32 = 8;
    /// 控件之间的标准间距。
    pub const GAP_STD: i32 = 20;
    /// 大块内容之间的间距。
    pub const GAP_WIDE: i32 = 44;
    /// 页面分段之间的间距。
    pub const GAP_HUGE: i32 = 58;
    /// 一行"控件 + 说明"占的高度。
    pub const ROW_TALL: i32 = 46;
    /// 次级左缩进（比 `CONTROL_INSET` 更靠左的输入框/条目）。
    pub const INSET_SM: i32 = 16;

    // ---- 控件高度阶梯 ----
    /// 按钮 / 输入框的标准高度。
    pub const CTRL_H: i32 = 44;
    /// 图标框与列表行的高度。
    pub const CTRL_H_SM: i32 = 30;
    /// 面板与次级图标的高度。
    pub const CTRL_H_XS: i32 = 26;

    // ---- 内边距 ----
    /// 面板内容与边框之间的内边距。
    pub const PAD_SM: i32 = 8;
    /// 较大的内边距（卡片/浮层内容）。
    pub const PAD_LG: i32 = 20;
    /// 子项相对父项的缩进（例如审计条目相对页正文）。
    pub const INDENT_SUB: i32 = 36;

    /// AI 指令条的高度（悬浮胶囊）。
    pub const AI_BAR_H: i32 = 52;

    // ---------------------------------------------------------------------
    // **故意不缩**的（写在这里，免得后来的人以为是漏了）
    //
    // * `h: 1` / `h: 0`（发丝分隔线、占位）与 `x + 1` / `y + 1`（阴影偏移）：
    //   1 像素的线在 125% 下还应该是 1 像素，否则会糊成 1.25 像素的边；
    // * `x + 4` / `x + 6`：抽样看到**混用** —— 既有内边距，也有「高亮条」这类装饰
    //   （`cover.x + 6, w: cover_s - 12, h: 1`）。要缩就得逐个看，留到有人能看屏幕时判断；
    // * `h: 12` / `h: 6`：装饰性短条（均衡器）与状态点（`w: 6, h: 6` 配 `radius 3.0`）——
    //   后者要缩就得连圆角半径一起缩，属于「成套缩放」，同样留待有屏幕时判断；
    // * `y + 8`：混用（`y + 8.0` 是文本基线微调，`list.y + 8` 是内边距）。
    // ---------------------------------------------------------------------

    static PCT: AtomicU16 = AtomicU16::new(100);

    /// 设置缩放百分比（与 [`super::ui::set_scale_pct`] **同一份输入**）。
    pub fn set_pct(pct: u16) {
        PCT.store(pct, Ordering::Relaxed);
    }

    /// 当前缩放百分比。
    pub fn pct() -> u16 {
        PCT.load(Ordering::Relaxed)
    }

    /// 把一个布局尺寸按给定百分比换算（**纯函数**，测试用它，不碰全局状态）。
    ///
    /// 用整数运算并四舍五入到最近整数：布局坐标是整数，用浮点会引入
    /// 与平台有关的舍入差异 —— 那会让"逐像素不变"的截图比对对不上。
    pub fn scale(n: i32, pct: u16) -> i32 {
        let n = n as i64 * pct as i64;
        // 负数（阴影偏移之类）也要按远离零的方向四舍五入，保持对称
        let half = if n >= 0 { 50 } else { -50 };
        ((n + half) / 100) as i32
    }

    /// 按当前缩放取一个布局尺寸。100% 下**恒等于入参**。
    pub fn px(n: i32) -> i32 {
        scale(n, pct())
    }
}

#[cfg(test)]
mod tests {
    // 这个测试模块是 size 的**兄弟**（在 	heme 层），所以要走 super::size
    use super::size::*;

    /// 100% 下**恒等于入参** —— 这是"纯重构、行为不变"的依据。
    #[test]
    fn scale_is_identity_at_100_percent() {
        for n in [-24, -7, -1, 0, 1, 8, 12, 18, 22, 24, 26, 36, 40, 44, 46, 100, 1000] {
            assert_eq!(scale(n, 100), n, "{n} 在 100% 下应当不变");
        }
    }

    /// 取整规则：四舍五入到最近整数（布局坐标是整数，用浮点会引入平台相关的差异）。
    #[test]
    fn scale_rounds_to_nearest_and_keeps_symmetry() {
        assert_eq!(scale(40, 125), 50);
        assert_eq!(scale(24, 125), 30);
        assert_eq!(scale(22, 125), 28, "27.5 进位到 28");
        assert_eq!(scale(18, 125), 23, "22.5 进位到 23");
        assert_eq!(scale(40, 75), 30);
        // 1 像素在 50% 下**不能缩成 0**：控件不能因为缩放而消失
        assert_eq!(scale(1, 50), 1);
        // 负数（阴影偏移之类）按远离零的方向取整，保持左右对称
        assert_eq!(scale(-1, 50), -1);
        assert_eq!(scale(-24, 125), -30);
    }

    /// 不变量：尺寸表里的每个常量都**至少有一个调用点**。
    ///
    /// 为什么值得一条测试：这张表的意义就是"被页面代码引用"。
    /// 加了常量却没人用，它就在慢慢变成装饰 —— 与项目其它"死表/死字段"问题同族
    /// （`ui_scale_pct` 曾经就是"存了不用"的典型）。
    ///
    /// 用 `include_str!` 读自己的源码：与 `aetherd` 那条
    /// `every_request_handler_is_gated_and_classified` 同一手法 —— 把结论变成不变量。
    #[test]
    fn every_size_constant_is_actually_used() {
        let src = include_str!("draw.rs");
        for name in [
            "CONTENT_PAD",
            "CONTROL_INSET",
            "INSET_SM",
            "ROW_H",
            "ROW_H_SM",
            "ROW_TALL",
            "GROUP_GAP",
            "LINE_GAP",
            "HINT_GAP",
            "TIGHT",
            "GAP_TIGHT",
            "GAP_STD",
            "GAP_WIDE",
            "GAP_HUGE",
            "CTRL_H",
            "CTRL_H_SM",
            "CTRL_H_XS",
            "PAD_SM",
            "PAD_LG",
            "INDENT_SUB",
            "AI_BAR_H",
        ] {
            // 带上右括号：否则 `ROW_H` 会被 `ROW_H_SM` 的调用点误算成"用过"
            let needle = format!("px(theme::size::{name})");
            assert!(
                src.matches(&needle).count() > 0,
                "尺寸表里的 {name} 没有任何调用点 —— 要么用上，要么从表里删掉"
            );
        }
    }

    /// 全局状态：默认 100%；设过之后 `px` 跟着变。
    #[test]
    fn px_follows_the_shared_setting() {
        set_pct(100);
        assert_eq!(px(24), 24);
        set_pct(200);
        assert_eq!(px(24), 48);
        // 复原：这是全局状态，别影响别的用例
        set_pct(100);
        assert_eq!(px(24), 24);
    }
}

pub mod ui {
    use std::sync::atomic::{AtomicU16, Ordering};

    static SCALE_PCT: AtomicU16 = AtomicU16::new(100);

    /// 设置缩放（**吸附到最近的合法档位**：设置文件可被手改，不能信任它的值）。
    pub fn set_scale_pct(pct: u16) {
        // 布局尺寸表与字号**共用同一份缩放输入**（§2.4）：两处各自读设置会漂移
        super::size::set_pct(pct);
        SCALE_PCT.store(crate::settings::snap_scale(pct), Ordering::Relaxed);
    }

    pub fn scale_pct() -> u16 {
        SCALE_PCT.load(Ordering::Relaxed)
    }

    /// **纯函数**：按给定百分比缩放一个字号（测试用它，不碰全局状态）。
    pub fn font_at(pct: u16, px: f32) -> f32 {
        let pct = crate::settings::snap_scale(pct);
        (px * pct as f32 / 100.0).max(1.0)
    }

    /// 按当前缩放取实际字号（所有文字都走这里）。
    pub fn font(px: f32) -> f32 {
        font_at(scale_pct(), px)
    }
}

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
        // 2026-10-01：红绿灯换 Windows 三键后，LIGHT_D / LIGHT_GAP 已删除（圆点尺寸不再需要）。
        /// Windows 标题栏按钮宽度（最小化 / 最大化 / 关闭各占一格，高度 = TITLE_H）
        pub const CAPTION_BTN_W: i32 = 46;
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

/// 预览失败的原因（§3.5）。
///
/// 与 `aether_ipc::DnsError` 同一套理由：这一层是**数据层**，
/// 让它出中英两套文案就得依赖 `i18n`（分层坏掉）或维护对照表（必然漂移）。
/// 所以它只回答「**为什么**预览不了」，措辞交给界面（`preview_error_text`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    /// 目标是目录
    IsDir,
    /// 文件太大（带**已经格式化好的**大小文字，避免这一层再依赖 `text::human_bytes` 之外的规则）
    TooLarge(String),
    /// 看起来是二进制
    Binary,
    /// 读文件失败（系统的错误文本原样带过来 —— 那不是我们的话，不该改）
    Io(String),
    /// **界面自己的**说明文字（已经按语言取好）。
    ///
    /// 存在的理由：通知中心复用 `PreviewData` 表达"空态"（"还没有通知" /
    /// "通知记录已关闭…"），那两句是**界面**说的，不是数据层说的 ——
    /// 所以由界面按语言取好后放进这个变体，数据层只负责带着它走。
    Message(String),
}

/// 把 `PreviewError` 渲染成**给用户看的一句话**（§3.5）。
///
/// 抽成纯函数：中英文两套文案都有测试，且"几种原因是否都覆盖"由编译器保证（`match` 穷尽）。
pub fn preview_error_text(lang: crate::i18n::Lang, e: &PreviewError) -> String {
    use crate::i18n::{pick, Lang};
    match e {
        PreviewError::IsDir => pick(
            lang,
            "这是一个目录",
            "This is a folder",
        )
        .to_string(),
        PreviewError::TooLarge(size) => {
            if lang == Lang::En {
                format!("File too large ({size}) — preview not supported yet")
            } else {
                format!("文件过大（{size}），暂不支持预览")
            }
        }
        PreviewError::Binary => pick(
            lang,
            "二进制文件，无法以文本预览",
            "Binary file — cannot preview as text",
        )
        .to_string(),
        // 系统错误与界面自带文案都原样显示：它们不是本模块的话，不该改
        PreviewError::Io(msg) => msg.clone(),
        PreviewError::Message(msg) => msg.clone(),
    }
}

/// 文本预览内容。
#[derive(Clone, Debug)]


pub struct PreviewData {
    /// 被打开文件的完整路径（标题栏显示）
    pub path: String,
    /// 已按行切好的内容（限长）
    pub lines: Vec<String>,
    /// 读取失败的原因；有值时窗口内显示它而不是内容（§3.5：**枚举**，文案由界面渲染）
    pub error: Option<PreviewError>,
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
            return PreviewData { path: path.into(), lines: Vec::new(), error: Some(PreviewError::IsDir), truncated: false, scroll: Default::default() };
        }
        if m.len() > PREVIEW_MAX_BYTES {
            return PreviewData {
                path: path.into(),
                lines: Vec::new(),
                error: Some(PreviewError::TooLarge(
                    FsEntry { name: String::new(), is_dir: false, size: m.len() }.size_label(),
                )),
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
                    return PreviewData { path: path.into(), lines: Vec::new(), error: Some(PreviewError::Binary), truncated: false, scroll: Default::default() };
                }
            };
            let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
            let truncated = lines.len() > PREVIEW_MAX_LINES;
            if truncated {
                lines.truncate(PREVIEW_MAX_LINES);
            }
            PreviewData { path: path.into(), lines, error: None, truncated, scroll: Default::default() }
        }
        Err(e) => PreviewData {
            path: path.into(),
            lines: Vec::new(),
            error: Some(PreviewError::Io(e.to_string())), truncated: false, scroll: Default::default() },
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

/// 帧渲染器：持有跨帧缓存（背景层等）与可点击区域登记（供命中测试）。
/// 性能 HUD（§5.10）：右下角一块半透明小面板，显示**真实**帧时间与内存。
///
/// 三条自我约束：
/// 1. **只显示真数据** —— 拿不到就写"不可用"，绝不编一个数字（本项目对假数据零容忍）。
/// 2. **默认关**（设置里的 `perf_hud`）—— 它是排障工具，不是常显装饰。
/// 3. 放在**右下角**且不拦截点击：HUD 不该影响正常操作（它没有登记任何命中区）。
fn draw_perf_hud(buf: &mut [u32], w: usize, h: usize, text: &str, tr: Option<&TextRenderer>) {
    let Some(tr) = tr else { return };
    let tw = tr.measure(text, font::CAPTION).ceil() as i32;
    let panel = Rect { x: w as i32 - tw - 24, y: h as i32 - 34, w: tw + 16, h: theme::size::px(theme::size::CTRL_H_XS) };
    rounded_rect(buf, w, h, panel, radius::SM, color::surface_3(), 0.82);
    rounded_outline(buf, w, h, panel, radius::SM, color::hairline(), 0.3);
    draw_text(
        tr,
        buf,
        w,
        h,
        (panel.x + theme::size::px(theme::size::PAD_SM)) as f32,
        tr.vcenter(panel.y as f32, panel.h as f32, font::CAPTION),
        text,
        font::CAPTION,
        color::text(),
        0.95,
    );
}

/// 控制中心的行布局：**高度从行数推导**，不写死数字。
///
/// 为什么（2026-10-02 功能批次 J-2）：`PANEL_H` 原先是手写常量，旁边一句注释
/// "加一行就要同步这里"。那种约定迟早会忘 —— 忘了的后果是最后一行**画在面板外**
/// （看得见、点不到，或直接被裁掉），而且只在特定分辨率下才明显。
/// 现在行数与行高是唯一事实来源，高度是算出来的：加行自动变高。
pub mod control_layout {
    /// 开关行（深色模式 / 输入法 / 24 小时制）
    pub const SWITCH_ROWS: i32 = 3;
    /// 快捷跳转行（设置 / 终端 / 通知 —— §3.2 加了通知中心入口）
    pub const JUMP_ROWS: i32 = 3;
    /// 电源行（关机 / 重启 / 注销）
    pub const POWER_ROWS: i32 = 3;
    /// 音量行（批次 K-2）：一条可点击设定的音量轨道
    pub const VOLUME_ROWS: i32 = 1;
    /// 虚拟桌面行（§5.4）：新增桌面 / 移除当前桌面
    pub const SPACE_ROWS: i32 = 2;

    /// 开关行高（含行距）
    pub const ROW_H_SWITCH: i32 = 36;
    /// 动作行高（快捷跳转 / 电源）
    pub const ROW_H_ACTION: i32 = 32;
    /// 顶部内边距 + AI 三态行
    const HEAD: i32 = 12 + 26;
    /// 一次分隔 + 组标题
    const SECTION: i32 = 8 + 20;
    /// 底部提示留白
    pub const FOOTER: i32 = 22;

    /// 内容总高（不含底部留白）
    pub const CONTENT_H: i32 = HEAD
        + SECTION
        + SWITCH_ROWS * ROW_H_SWITCH
        + SECTION
        + JUMP_ROWS * ROW_H_ACTION
        + SECTION
        + VOLUME_ROWS * ROW_H_ACTION
        + SECTION
        + SPACE_ROWS * ROW_H_ACTION
        + SECTION
        + POWER_ROWS * ROW_H_ACTION;

    /// 面板高度 = 内容 + 底部提示留白
    pub const PANEL_H: i32 = CONTENT_H + FOOTER;
}

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

/// 画一个应用图标：彩色底座 + 白色几何符号（全部纯 rect/圆，不依赖字体字形覆盖）。
///
/// **Dock 与桌面图标共用这一个函数** —— 两处各画一套必然会走形，而且改一处忘另一处。
///
/// - `idx`：内建序号（0 文件 / 1 终端 / 2 浏览器 / 3 音乐 / 4 设置）
/// - `installed`：已装应用（青绿底座，与内建一眼可分）
/// - `installer`：安装向导（琥珀警示色 —— Live ISO 里唯一要一眼找到的动作）
/// - `running`：窗口已打开（青色微光，是状态反馈不是装饰）
#[allow(clippy::too_many_arguments)]
pub fn draw_app_icon(
    buf: &mut [u32],
    w: usize,
    h: usize,
    tile: Rect,
    idx: usize,
    installed: bool,
    installer: bool,
    hovered: bool,
    running: bool,
) {
    let base: [u8; 3] = if installer {
        [196, 142, 67] // 安装向导：琥珀
    } else if installed {
        [61, 149, 149] // 已装应用：青绿
    } else {
        match idx {
            0 => [99, 164, 206],  // 文件：青蓝
            1 => [82, 118, 168],  // 终端：钢青
            2 => [88, 148, 224],  // 浏览器：蓝
            3 => [108, 122, 148], // 音乐：雾灰蓝（P0：由紫色改）
            _ => [116, 125, 147], // 设置：蓝灰
        }
    };
    // 底座（P0：纯色，去"上亮下暗"渐变与紫色系 —— "彩色渐变底座"正是 AI 产品味的来源之一）
    rounded_rect(buf, w, h, tile, radius::MD, base, if installer && !hovered { 0.92 } else { 0.97 });
    // 顶部内高光（P0：0.24 → 0.08，去"玻璃高光"感）
    fill_rect(buf, w, h, Rect { x: tile.x + 4, y: tile.y + 1, w: tile.w - 8, h: 1 }, color::HIGHLIGHT, 0.08);
    if hovered {
        rounded_rect(buf, w, h, tile, radius::MD, color::hairline(), 0.14);
    }
    if running {
        rounded_rect(buf, w, h, tile, radius::MD, color::accent(), 0.10);
    }
    rounded_outline(buf, w, h, tile, radius::MD, color::hairline(), if hovered { 0.30 } else { 0.18 });

    let (ix, iy) = (tile.x, tile.y);
    let sym = color::HIGHLIGHT;
    let a = 0.95;
    if installer {
        // 安装：向下箭头写入磁盘底座
        fill_rect(buf, w, h, Rect { x: ix + 20, y: iy + 10, w: 6, h: 12 }, sym, a);
        for r in 0..6i32 {
            fill_rect(buf, w, h, Rect { x: ix + 14 + r, y: iy + 22 + r, w: 18 - 2 * r, h: 2 }, sym, a);
        }
        fill_rect(buf, w, h, Rect { x: ix + 12, y: iy + 33, w: 22, h: 3 }, sym, a);
        return;
    }
    match idx {
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
            rounded_outline(buf, w, h, Rect { x: ix + 8, y: iy + 8, w: 30, h: theme::size::px(theme::size::CTRL_H_SM) }, 15.0, sym, a);
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
        // 设置：齿轮 = 外环 + 内环 + 四齿（已装应用也走这一支，与原来一致）
        _ => {
            rounded_outline(buf, w, h, Rect { x: ix + 10, y: iy + 10, w: 26, h: theme::size::px(theme::size::CTRL_H_XS) }, 13.0, sym, a);
            rounded_outline(buf, w, h, Rect { x: ix + 16, y: iy + 16, w: 14, h: 14 }, 7.0, sym, a);
            for (tx, ty) in [(21, 8), (21, 33), (8, 21), (33, 21)] {
                fill_rect(buf, w, h, Rect { x: ix + tx, y: iy + ty, w: 5, h: 5 }, sym, a);
            }
        }
    }
}

/// Windows 标题栏按钮的几何符号。
///
/// `kind`：0 = 最小化（横线）/ 1 = 最大化（方框）/ 2 = 关闭（叉）。
/// 全部用 1px 细线画 —— Windows 的这三个符号就是细线，用粗块会立刻变成"另一个系统"。
fn caption_glyph(buf: &mut [u32], w: usize, h: usize, r: Rect, kind: u8, rgb: [u8; 3], alpha: f32) {
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;
    match kind {
        // 最小化：一条横线
        0 => fill_rect(buf, w, h, Rect { x: cx - 5, y: cy, w: 10, h: 1 }, rgb, alpha),
        // 最大化：1px 方框（四条边分别画，圆角函数画不出这种硬边）
        1 => {
            fill_rect(buf, w, h, Rect { x: cx - 5, y: cy - 5, w: 11, h: 1 }, rgb, alpha);
            fill_rect(buf, w, h, Rect { x: cx - 5, y: cy + 5, w: 11, h: 1 }, rgb, alpha);
            fill_rect(buf, w, h, Rect { x: cx - 5, y: cy - 5, w: 1, h: 11 }, rgb, alpha);
            fill_rect(buf, w, h, Rect { x: cx + 5, y: cy - 5, w: 1, h: 11 }, rgb, alpha);
        }
        // 关闭：两条 1px 对角线
        _ => {
            for k in -5..=5 {
                fill_rect(buf, w, h, Rect { x: cx + k, y: cy + k, w: 1, h: 1 }, rgb, alpha);
                fill_rect(buf, w, h, Rect { x: cx + k, y: cy - k, w: 1, h: 1 }, rgb, alpha);
            }
        }
    }
}

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
    /// 「网络」页的 DNS 输入框状态（§2.4）。**不放进 `Settings`**：
    /// 那个结构会被序列化落盘，而这个只是编辑期的临时状态（含光标与未提交文本）。
    pub dns: crate::settings::DnsField,
    /// §2.3：改口令表单（三个**遮显**输入框 + 焦点 + 回执）。
    ///
    /// 为什么放在 `Renderer` 而不是 `Settings`：`Settings` 会被**写到磁盘**
    /// （`settings.json`）—— 口令绝不能进那里。
    pub pass_form: crate::settings::PassForm,
    /// §2.3：人类账户列表（只读，来自 aetherd 的 `ListUsers`）。
    pub users: Vec<aether_ipc::UserInfo>,
    /// 是否已经读过一次（区分"正在读取"与"确实没有账户"）。
    pub users_loaded: bool,
    /// §3.7：检测到的外设（只读，来自 aetherd 的 `ListPeripherals`）。
    pub peripherals: Vec<aether_ipc::PeripheralWire>,
    /// 是否已经读过一次。
    pub peripherals_loaded: bool,
    /// 锁屏状态（§2.3）。放在 Renderer 里是因为"锁"影响**每一帧的绘制**
    /// （整屏遮罩），而不只是某个页面。
    pub lock: crate::lock::LockScreen,
    /// 命令面板（§5.5）。跨帧存在（查询、结果、选中项），所以要放在 Renderer 上。
    pub palette: crate::palette::Palette,
    /// 「工具权限」页的视图状态（§5.3）。列表来自 aetherd（异步取），状态跨帧保留。
    pub policy: crate::policyview::PolicyView,
    /// 首次启动向导（§5.8）。状态跨帧保留（走到第几步、还开不开）。
    pub setup: crate::setup::Setup,
    /// 「应用商店」页的状态（§5.11）。目录扫描只在进页面/点重扫时做，不每帧做。
    pub store: crate::store::StoreView,
    /// 「时钟与时区」页的对时/系统时区状态（§3.3）。
    pub time: crate::timeview::TimeView,
    /// 「快照与回滚」页的状态（§5.6）。
    pub snapshots: crate::snapview::SnapView,
    /// 「系统更新」页的状态（§3.1）。
    pub update: crate::updateview::UpdateView,
    /// §5.2 时间线的搜索词与编辑态。放在 Renderer 上：绘制要读它、
    /// 事件循环要写它，两者都拿得到 Renderer。
    pub audit_query: String,
    /// 搜索框是否正在接收键盘输入（与 DNS 编辑态同一套做法）。
    ///
    /// 读它在 **Linux 的输入循环**里；非 Linux 上只有测试写它 —— 按平台 allow。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub audit_editing: bool,
    /// 首启向导选项按钮的命中区（§5.8 鼠标点选）：(矩形, 选项下标)。
    pub setup_hits: Vec<(Rect, usize)>,
    /// 命令面板可见行的命中区（§5.5 鼠标点选）：(矩形, 条目下标)。
    /// 每帧重算 —— 面板的候选会随输入变化，"上一帧的命中区"没有意义。
    pub palette_hits: Vec<(Rect, usize)>,
    /// 显示输出（§3.8）：**只读**，没有交互状态，所以直接存列表而不是单开视图模块。
    pub displays: Vec<aether_ipc::DisplayWire>,
    /// 合成器驱动的输出下标（由 aetherd 告知，约定见 `aetherd::display::DRIVEN_INDEX`）。
    pub driven_index: u32,
    /// 是否已经拉过一次（决定"正在读取"还是"没有输出"）
    pub displays_loaded: bool,
    /// 可移动设备（§3.7）：列表 + 正在操作的设备 + 上一次的结果。
    ///
    /// 这里有交互状态（进行中 / 结果），但只有三个字段 —— 单开一个视图模块
    /// 反而更绕（对比 §5.6/§3.1 那两个有多步确认与两步状态机的页）。
    pub removables: Vec<aether_ipc::RemovableWire>,
    pub removables_loaded: bool,
    pub removable_pending: Option<String>,
    pub removable_message: Option<String>,
    /// 当前虚拟桌面数量（§5.4）。绘制要用它显示"N 个桌面"——
    /// 桌面状态住在事件循环的 `spaces` 里（那里才拿得到窗口），所以这里只镜像一个数字。
    pub space_count: usize,
    /// 设置面板本帧登记的可交互项（绘制期登记、事件循环消费）
    pub settings_hits: Vec<(Rect, crate::settings::SettingsHit)>,
    /// 控制中心是否展开（P4）。状态放渲染器里：主循环直接读写，**不必动 `UiState`**
    /// （它有 3 个构造点，为一个小面板改结构体不划算）。
    pub control_open: bool,
    /// 顶栏"状态簇"命中区（= 现有 AI 状态区，不额外加视觉元素）
    pub status_cluster: Rect,
    /// 控制中心面板矩形（用于区分"点面板内空白"与"点别处关闭"）
    pub control_panel: Rect,
    /// 控制中心的滚动偏移（像素）。100% 下内容装得下 → 恒为 0，画面不变。
    pub control_scroll: i32,
    /// 控制中心**内容实际高度**（绘制时量出），用于把滚动量夹住。
    pub control_content_h: i32,
    /// 背景缓存需要重建（主题切换等）。惰性标记：下一帧 `prepare_background` 自动处理，
    /// 免得为切个主题把 `w/h` 传遍所有调用点。
    pub bg_dirty: bool,
    /// 性能 HUD 文本（§5.10）。`None` = 不显示。
    ///
    /// **由主循环每帧喂进来**（而不是从 settings 现算）：帧时间与内存占用
    /// 只有主循环知道，绘制层不该去读 `/proc`（那会让"画一帧"带上 IO）。
    pub perf_text: Option<String>,
    /// 通知历史（§3.2）。放在 Renderer 上而不是主循环局部变量：
    /// **两个事件循环（fbdev 与预览）都要写它**，局部变量就得声明两份，
    /// 而"只给其中一个接线"正是会产生静默不一致的那类改动。
    pub notifs: crate::notifications::Notifications,
    /// 桌面图标本帧的命中区（矩形 → 应用序号）。默认桌面**不开窗口**，图标是主要入口。
    pub desktop_icons: Vec<(Rect, usize)>,
    /// 安装向导的关闭按钮（2026-10-01：用户反馈"向导点开就关不掉"）。
    /// Esc 一直能关，但**界面上没有可见入口** —— 用户看不到就等同于关不掉。
    pub installer_close: Rect,
    /// 控制中心本帧登记的可交互项
    pub control_hits: Vec<(Rect, ControlHit)>,
    /// 中文输入法当前状态（面板要显示开关；真值在事件循环的 `ime` 里，主循环负责同步）
    pub ime_on: bool,
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
    /// 音乐列表行矩形（**只作几何取证**：`--dump-layout` 用它，
    /// 让 `layout-check.py` 能查「行与行是否重叠」；本批**不**接点击行为）。
    pub music_hits: Vec<(Rect, usize)>,
    /// 面包屑路径段命中区：(矩形, 目标路径)
    pub crumb_hits: Vec<(Rect, String)>,
    /// 窗口红绿灯命中区：(窗口索引, 关闭按钮, 最大化按钮)。
    ///
    /// 此前红绿灯**只画不响应** —— 开了 8 个窗口后没有任何关闭手段，用户会直接卡住。
    /// 每帧重建，与窗口几何保持同步。
    pub window_lights: Vec<(usize, Rect, Rect, Rect)>,
    /// "开始安装"按钮命中区
    pub installer_button: Rect,
    /// 权限确认弹窗的按钮命中区
    pub confirm_buttons: Vec<(Rect, ConfirmButton)>,
    /// 权限确认弹窗的回显输入框命中区
    pub confirm_echo: Rect,
}

/// 锁屏遮罩（§2.3）。
///
/// 设计要点：
/// * **整屏压暗**：不透明就不叫锁屏 —— 但纯黑会让人以为死机，用高不透明度而非全黑；
/// * **不画任何界面内容**（窗口标题、dock、通知都不画）—— 锁屏的意义就是「看不到」；
/// * 提示语如实显示：拒绝理由、退避倒计时、以及「本镜像未设口令」这种**必须让人知道**的事。
fn draw_lock_screen(
    buf: &mut [u32],
    w: usize,
    h: usize,
    lock: &crate::lock::LockScreen,
    tr: Option<&TextRenderer>,
) {
    use crate::draw::theme::color;
    fill_rect(
        buf,
        w,
        h,
        Rect { x: 0, y: 0, w: w as i32, h: h as i32 },
        color::scrim(),
        color::scrim_alpha(),
    );
    let Some(tr) = tr else { return };
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let title = crate::i18n::tr("已锁定", "Locked");
    let tw = tr.measure(title, font::TITLE);
    tr.draw_bold(buf, w, h, cx - tw / 2.0, cy - 90.0, title, font::TITLE, color::text(), 0.95);

    // 口令框：掩码 + 光标（与设置页的输入框同一视觉语言）
    let shown = format!("{}▏", lock.masked());
    let box_w = 360.0f32.min(w as f32 - 80.0);
    let bx = cx - box_w / 2.0;
    let by = cy - 20.0;
    fill_rect(
        buf,
        w,
        h,
        Rect { x: bx as i32, y: by as i32, w: box_w as i32, h: theme::size::px(theme::size::CTRL_H) },
        color::inset(),
        0.95,
    );
    let sw = tr.measure(&shown, font::BODY);
    tr.draw(
        buf, w, h, cx - sw / 2.0, by + 12.0, &shown, font::BODY,
        if lock.is_empty() { color::text_dim() } else { color::text() },
        if lock.is_empty() { 0.5 } else { 0.95 },
    );

    let hint = lock
        .message()
        .map(|s| s.to_string())
            .unwrap_or_else(|| crate::i18n::tr("输入口令后回车解锁", "Type your password and press Enter").to_string());
    let hw = tr.measure(&hint, font::CAPTION);
    tr.draw(buf, w, h, cx - hw / 2.0, by + 60.0, &hint, font::CAPTION, color::text_dim(), 0.9);

    if lock.attempts() > 0 {
        let n = format!("{} {} {}", crate::i18n::tr("已尝试", "Attempts:"), lock.attempts(), crate::i18n::tr("次", ""));
        let nw = tr.measure(&n, font::CAPTION);
        tr.draw(buf, w, h, cx - nw / 2.0, by + 84.0, &n, font::CAPTION, color::text_dim(), 0.7);
    }
}

/// 命令面板覆层（§5.5）。
///
/// 三件事决定它的样子：
/// * **居中偏上**：眼睛先看那里，且不挡住下方的输入焦点；
/// * **只画前 `MAX_ROWS` 条**（候选多于展示条数是 `Palette` 的既有语义）；
/// * 选中项要有**明确的高亮**：键盘操作时没有鼠标指针做参照，画不出"当前在哪条"
///   就等于没法用。
fn draw_palette(
    buf: &mut [u32],
    w: usize,
    h: usize,
    p: &crate::palette::Palette,
    tr: Option<&TextRenderer>,
    // §5.5 鼠标点选：登记可见行的矩形 → **条目下标**（不是行号！行是过滤排序后的）
    hits: &mut Vec<(Rect, usize)>,
) {
    use crate::draw::theme::color;
    // 压暗背景：面板是模态的，视觉上也该如此
    fill_rect(
        buf,
        w,
        h,
        Rect { x: 0, y: 0, w: w as i32, h: h as i32 },
        color::scrim(),
        0.18,
    );
    let Some(tr) = tr else { return };
    let panel_w = 520.0f32.min(w as f32 - 80.0);
    let row_h = 34.0f32;
    let rows = p.visible().len();
    let panel_h = 56.0 + row_h * rows as f32;
    let px = (w as f32 - panel_w) / 2.0;
    let py = (h as f32 * 0.18).max(24.0);
    fill_rect(
        buf,
        w,
        h,
        Rect { x: px as i32, y: py as i32, w: panel_w as i32, h: panel_h as i32 },
        color::glass(),
        0.98,
    );

    // 输入行：与设置页的口令/ DNS 输入框同一视觉语言（掩码会藏内容，这里要明文）
    let input = if p.query.is_empty() {
        crate::i18n::tr("输入以搜索应用、设置与文件…", "Search apps, settings and files…").to_string()
    } else {
        format!("{}▏", p.query)
    };
    tr.draw_bold(
        buf, w, h, px + 18.0, py + 18.0, &input, font::BODY,
        if p.query.is_empty() { color::text_faint() } else { color::text() },
        if p.query.is_empty() { 0.6 } else { 0.95 },
    );

    // 结果行
    for (i, hit) in p.visible().iter().enumerate() {
        let y = py + 56.0 + row_h * i as f32;
        let it = &p.items[hit.item];
        let sel = i == p.selected;
        // 命中区：整行可点（含右侧的类别标签）—— 点了就执行这一条
        hits.push((
            Rect {
                x: px as i32 + 8,
                y: y as i32,
                w: panel_w as i32 - 16,
                h: row_h as i32 - 4,
            },
            hit.item,
        ));
        if sel {
            fill_rect(
                buf, w, h,
                Rect { x: px as i32 + 8, y: y as i32, w: panel_w as i32 - 16, h: row_h as i32 - 4 },
                color::surface_3(),
                0.95,
            );
        }
        tr.draw_bold(
            buf, w, h, px + 20.0, y + 8.0, &it.title, font::BODY,
            if sel { color::text() } else { color::text_dim() },
            if sel { 0.98 } else { 0.85 },
        );
        // 类别右对齐：一眼分清"这是应用还是文件"
        let hint = crate::i18n::pick(
            crate::i18n::lang(),
            it.kind.label(),
            match it.kind {
                crate::palette::Kind::App => "App",
                crate::palette::Kind::Setting => "Setting",
                crate::palette::Kind::File => "File",
            },
        );
        let hw = tr.measure(hint, font::CAPTION);
        tr.draw(
            buf, w, h, px + panel_w - 20.0 - hw, y + 11.0, hint, font::CAPTION,
            color::text_faint(), 0.7,
        );
    }

    if rows == 0 {
        tr.draw(
            buf, w, h, px + 20.0, py + 64.0,
            crate::i18n::tr("没有匹配项", "No matches"), font::BODY, color::text_dim(), 0.7,
        );
    }
}

/// 首次启动向导覆层（§5.8）。
///
/// 每一步都有**真按钮**（§5.8）—— 键盘与鼠标都走 `setup::options` 这张表；
/// 底部的按键提示仍然保留（键盘用户更快），但**按钮才是"看得见能做"的那一份**。
/// 提示文案来自 `setup::hint`，与按键映射同源（`setup::handle_key`），不会分叉。
fn draw_setup(
    buf: &mut [u32],
    w: usize,
    h: usize,
    s: &crate::setup::Setup,
    settings: &crate::settings::Settings,
    tr: Option<&TextRenderer>,
    mouse: (f32, f32),
    // §5.8 鼠标点选：登记"选项按钮的矩形 → 选项下标"。
    // **先画按钮、再登记命中区** —— 本项目零假控件：能点的东西必须看得见。
    hits: &mut Vec<(Rect, usize)>,
) {
    use crate::draw::theme::color;
    use crate::widgets;
    // 全屏压暗：开机第一屏，背后本来就没什么内容，但模态语义要一致
    fill_rect(
        buf,
        w,
        h,
        Rect { x: 0, y: 0, w: w as i32, h: h as i32 },
        color::scrim(),
        0.55,
    );
    let Some(tr) = tr else { return };
    let lang = crate::i18n::lang();
    let panel_w = 560.0f32.min(w as f32 - 80.0);
    let panel_h = 300.0f32;
    let px = (w as f32 - panel_w) / 2.0;
    let py = (h as f32 - panel_h) / 2.0;
    fill_rect(
        buf, w, h,
        Rect { x: px as i32, y: py as i32, w: panel_w as i32, h: panel_h as i32 },
        color::glass(),
        0.99,
    );

    let (zh, en) = crate::setup::title(s.step);
    tr.draw_bold(buf, w, h, px + 24.0, py + 22.0, crate::i18n::pick(lang, zh, en), font::TITLE, color::text(), 0.98);

    let (hz, he) = crate::setup::hint(s.step);
    tr.draw(buf, w, h, px + 24.0, py + 56.0, crate::i18n::pick(lang, hz, he), font::CAPTION, color::text_dim(), 0.9);

    // 当前值：让用户知道自己改成了什么（键盘驱动时尤其重要 —— 没有控件状态可看）
    let tz_h = settings.tz_offset_min / 60;
    let tz_m = (settings.tz_offset_min % 60).abs();
    let cur = format!(
        "{}：{}    {}：UTC{:+03}:{:02}    {}：{} / {}",
        crate::i18n::tr("语言", "Language"),
        if settings.locale == 1 { "English" } else { "中文" },
        crate::i18n::tr("时区", "Time zone"),
        tz_h,
        tz_m,
        crate::i18n::tr("外观", "Theme"),
        if settings.dark_mode { crate::i18n::tr("深色", "Dark") } else { crate::i18n::tr("浅色", "Light") },
        if settings.high_contrast { crate::i18n::tr("高对比", "High contrast") } else { crate::i18n::tr("标准", "Standard") },
    );
    tr.draw(buf, w, h, px + 24.0, py + 96.0, &cur, font::CAPTION, color::text_dim(), 0.85);

    // §5.8：每一步的**真按钮**（键盘与鼠标共用 `setup::options` 这张表）。
    // 画出来的每个按钮都登记命中区；点下去执行的动作就是表里写的那个。
    {
        let opts = crate::setup::options(s.step);
        let mut bx = px + 24.0;
        let by = py + 132.0;
        let bh = 34.0;
        for (i, (zh, en, _)) in opts.iter().enumerate() {
            let label = crate::i18n::pick(lang, zh, en);
            let bw = (tr.measure(label, font::BODY) + 36.0).max(96.0);
            // 超出面板就不画（也不登记命中区）—— 宁可少一个按钮，也不要画出界
            if bx + bw > px + panel_w - 16.0 {
                break;
            }
            let r = Rect { x: bx as i32, y: by as i32, w: bw as i32, h: bh as i32 };
            // 只有"这一步就一个按钮"时才用主操作样式（例如收尾步的「开始使用」）；
            // 多选一步（语言/时区/外观）里几个按钮是平级的
            let kind = if opts.len() == 1 {
                widgets::BtnKind::Primary
            } else {
                widgets::BtnKind::Secondary
            };
            widgets::button(buf, w, h, r, label, kind, widgets::States::at(r, mouse), Some(tr));
            hits.push((r, i));
            bx += bw + 12.0;
        }
    }

    // 步骤指示 + 操作提示
    let footer = format!(
        "{} {}/{}    {}    {}",
        crate::i18n::tr("第", "Step"),
        s.step + 1,
        crate::setup::STEP_COUNT,
        crate::i18n::tr("回车继续 · B 返回 · Esc 跳过", "Enter next · B back · Esc skip"),
        if s.is_last_step() {
            crate::i18n::tr("（回车即完成）", "(Enter finishes)")
        } else {
            ""
        },
    );
    tr.draw(buf, w, h, px + 24.0, py + panel_h - 44.0, &footer, font::CAPTION, color::text_dim(), 0.8);

    // 进度点
    for i in 0..crate::setup::STEP_COUNT {
        let x = px + 24.0 + i as f32 * 18.0;
        let y = py + panel_h - 22.0;
        fill_rect(
            buf, w, h,
            Rect { x: x as i32, y: y as i32, w: 10, h: 10 },
            if i == s.step { color::accent() } else { color::hairline() },
            0.95,
        );
    }
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
            dns: Default::default(),
            pass_form: crate::settings::PassForm::default(),
            users: Vec::new(),
            users_loaded: false,
            peripherals: Vec::new(),
            peripherals_loaded: false,
        lock: Default::default(),
        palette: Default::default(),
        policy: Default::default(),
        setup: Default::default(),
        store: Default::default(),
        time: Default::default(),
        snapshots: Default::default(),
        update: Default::default(),
        audit_query: String::new(),
        audit_editing: false,
        setup_hits: Vec::new(),
        palette_hits: Vec::new(),
        displays: Vec::new(),
        driven_index: 0,
        displays_loaded: false,
        removables: Vec::new(),
        removables_loaded: false,
        removable_pending: None,
        removable_message: None,
        space_count: crate::spaces::Spaces::<Win>::COUNT,
            settings_hits: Vec::new(),
            control_open: false,
            status_cluster: Rect { x: 0, y: 0, w: 0, h: 0 },
            control_panel: Rect { x: 0, y: 0, w: 0, h: 0 },
            control_scroll: 0,
            control_content_h: 0,
            bg_dirty: false,
            perf_text: None,
            notifs: crate::notifications::Notifications::new(),
            desktop_icons: Vec::new(),
            installer_close: Rect { x: 0, y: 0, w: 0, h: 0 },
            control_hits: Vec::new(),
            ime_on: false,
            dropdown: None,
            installer_rows: Vec::new(),
            file_cells: Vec::new(),
            file_up: Rect { x: 0, y: 0, w: 0, h: 0 },
            window_lights: Vec::new(),
            sidebar_hits: Vec::new(),
            music_hits: Vec::new(),
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
    /// 主题切换后**必须**调用：重建背景缓存并作废"背景+投影"合成层。
    ///
    /// 不重建的后果是壁纸停留在旧主题（缓存按行分帧生成，最多要 1800 帧才刷完）。
    /// P0 把壁纸改成静态后，整屏只是一次 lerp/行 —— 直接一次填满比拼帧快得多。
    pub fn invalidate_background(&mut self, w: usize, h: usize, t: f32) {
        if self.bg.len() < w * h {
            self.bg = vec![0u32; w * h];
        }
        draw_background(&mut self.bg, w, h, t, self.settings.wallpaper, self.settings.dark_mode);
        self.bg_row = h; // 背景已完整
        self.shadow_key = 0; // 合成层作废 → 下一帧重新烘焙（与 prepare_background 同一手法）
    }

    /// 整屏烘焙背景 + 投影（**走查/预览路径专用**；Linux 的 fbdev 循环在 `render_frame` 里
    /// 按 `BG_ROWS_PER_FRAME` 分帧生成，不走这里）。
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn prepare_background(&mut self, w: usize, h: usize, t: f32) {
        // 主题切换等让缓存作废的改动：这里统一补一次（调用方只需置 `bg_dirty`）
        if self.bg_dirty {
            self.invalidate_background(w, h, t);
            self.bg_dirty = false;
        }
        if self.bg.len() != w * h {
            self.bg = vec![0; w * h];
        }
        draw_background(&mut self.bg, w, h, t, self.settings.wallpaper, self.settings.dark_mode);
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
        // 每帧清一次裁剪区间：裁剪是「当帧」的语义，不跨帧泄漏（附录 DD/DI）。
        if let Some(t) = tr {
            t.set_clip(None);
        }
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
        // 主题切换：**整屏重建**，不分帧。P0 后壁纸是静态的，整屏只是一次 lerp/行；
        // 而分帧会在半秒里露出"上半新主题、下半旧主题"的横向接缝。
        if self.bg_dirty {
            self.invalidate_background(w, h, t);
            self.bg_dirty = false;
        }
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
            draw_background_rows(
                &mut self.bg,
                w,
                h,
                t,
                self.bg_row,
                y1,
                self.settings.wallpaper,
                self.settings.dark_mode,
            );
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
        self.music_hits.clear();
        self.crumb_hits.clear();
        self.settings_hits.clear();
        for (i, win) in desktop.wins.iter().enumerate() {
            draw_window(
                buf, w, h, win, i == desktop.active,
                PageCtx {
                    mouse: ui.mouse,
                    tr,
                    settings: self.settings,
                    dns: &self.dns,
                    pass: &self.pass_form,
                    users: &self.users,
                    users_loaded: self.users_loaded,
                    peripherals: &self.peripherals,
                    peripherals_loaded: self.peripherals_loaded,
            audit_query: &self.audit_query,
                    policy: &self.policy,
                    store: &self.store,
                    time: &self.time,
                    snapshots: &self.snapshots,
                    update: &self.update,
                    displays: &self.displays,
                    driven_index: self.driven_index,
                    displays_loaded: self.displays_loaded,
                    removables: &self.removables,
                    removables_loaded: self.removables_loaded,
                    removable_pending: self.removable_pending.as_deref(),
                    removable_message: self.removable_message.as_deref(),
                },
                t, &fv,
                &mut self.file_cells, &mut self.file_up, i, &mut self.window_lights,
                &mut self.sidebar_hits, &mut self.crumb_hits, &mut self.music_hits,
                self.settings_page, &mut self.settings_hits,
            );
        }
        mark!("windows");

        // 桌面图标（默认桌面无窗口，它们是主要入口）—— 画在壁纸之上、窗口之下
        self.draw_desktop_icons(buf, w, h, ui, tr);
        mark!("deskicons");
        self.draw_menubar(buf, w, h, ui, tr);
        mark!("menubar");
        // 控制中心（P4）：画在顶栏之上、其它窗口之下 —— 弹出面板的常规层级
        if !self.control_open {
            // 关掉就归零：下次打开不会停在上次滚到的位置
            self.control_scroll = 0;
        }
        if self.control_open {
            self.draw_control_center(buf, w, h, ui, tr);
            // 控制中心滚起来后内容会越过面板上边界（`tr.draw` 不裁剪）——
            // 把菜单栏那一条**重画一遍**盖住它。`draw_menubar` 是幂等的。
            // **只在真的滚了时做**：100% 下滚动量恒为 0，基线画面不变。
            if self.control_scroll > 0 {
                self.draw_menubar(buf, w, h, ui, tr);
            }
            mark!("control");
        }
        if ui.open_menu.is_some() {
            self.draw_dropdown(buf, w, h, ui, tr);
        }
        // 首次启动向导（§5.8）：**盖在命令面板之上**（开机第一次进来，
        // 向导是当前唯一该做的事），但仍在锁屏之下（锁屏必须最后画，见文件末尾注释）。
        if self.setup.open {
            self.setup_hits.clear();
        draw_setup(
            buf,
            w,
            h,
            &self.setup,
            &self.settings,
            tr,
            ui.mouse,
            &mut self.setup_hits,
        );
            mark!("setup");
        }
        // 命令面板（§5.5）：在普通浮层之上、锁屏之下。
        if self.palette.open {
            self.palette_hits.clear();
        draw_palette(buf, w, h, &self.palette, tr, &mut self.palette_hits);
            mark!("palette");
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

        if let Some(text) = self.perf_text.as_deref() {
            draw_perf_hud(buf, w, h, text, tr);
        }
        if let Some((msg, age)) = ui.toast {
            draw_toast(buf, w, h, msg, age, tr);
        }
        mark!("toast");

        // 锁屏遮罩：**最后一个画**（盖住 toast、回复气泡、控制中心、甚至安装向导）。
        // 理由不是"层级美学"，而是**隐私**：锁屏若留着通知可见，
        // 「张三给你发了消息」就直接外泄了 —— 边界会漏在最容易被忽略的地方。
        // 解锁后本块不执行，下一帧自然恢复（无残留、无需重绘全屏）。
        if self.lock.is_locked() {
            draw_lock_screen(buf, w, h, &self.lock, tr);
            mark!("lock");
        }
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

/// 可选壁纸的配色（**低饱和中性渐变**）。
///
/// 索引 0 = 跟随主题（与改动前的行为完全一致，所以默认值不会让任何人"被换壁纸"）；
/// 1..=4 = 固定色相的深浅渐变。全部是低饱和中性色 —— 本项目不做装饰性图案/动效，
/// 壁纸的作用是"让人一眼分清自己的机器"，不是"好看"。
///
/// 越界索引**回退到默认**而不是 panic：设置文件是可以被手工改的。
pub fn wallpaper_colors(index: u8, dark: bool) -> ([u8; 3], [u8; 3]) {
    match index {
        0 => (color::bg_top(), color::bg_bottom()),
        1 => {
            if dark { ([38, 46, 54], [24, 30, 36]) } else { ([236, 242, 246], [212, 226, 234]) }
        }
        2 => {
            if dark { ([50, 45, 40], [30, 27, 24]) } else { ([246, 242, 235], [232, 222, 210]) }
        }
        3 => {
            if dark { ([36, 48, 42], [22, 30, 27]) } else { ([235, 246, 239], [210, 232, 218]) }
        }
        4 => {
            if dark { ([46, 38, 52], [28, 24, 32]) } else { ([244, 238, 248], [226, 216, 238]) }
        }
        // 第 5 档（批次 L-1 自查补）：`WALLPAPER_COUNT` 是 6，但表里只有 0..=4 时，
        // 第 5 档会落到下面的兜底分支 —— 于是"选第 5 档"看起来和默认一样（选了像没选）。
        // 是单测的"各档必须互不相同"把它抓出来的。
        5 => {
            if dark { ([52, 43, 44], [32, 26, 27]) } else { ([248, 241, 241], [236, 221, 221]) }
        }
        _ => (color::bg_top(), color::bg_bottom()),
    }
}

/// 生成壁纸的 `[y0, y1)` 行。
///
/// **分帧生成的基础**：整屏逐像素生成要 0.4–0.6 秒，压在一帧里就是开机后
/// 一次肉眼可见的卡死（实测首帧 561ms）。切成行带后每帧只付 1/N 的成本，
/// 而且因为绘制目标是与屏幕分离的 `bg` 缓冲，未完成前屏幕上不会出现横向接缝。
pub fn draw_background_rows(
    buf: &mut [u32],
    w: usize,
    h: usize,
    _t: f32,
    y0: usize,
    y1: usize,
    wallpaper: u8,
    dark: bool,
) {
    let y1 = y1.min(h);
    if y0 >= y1 || buf.len() < w * h {
        return;
    }
    // 2026-09-29 P0：壁纸改为**静态**——低饱和中性 + 极浅的垂直层次。
    // 逐像素的极光/柔光团/颗粒/暗角移除；每行一次 lerp、整行同色，成本大幅下降。
    let (top, bottom) = wallpaper_colors(wallpaper, dark);
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
pub fn draw_background(buf: &mut [u32], w: usize, h: usize, t: f32, wallpaper: u8, dark: bool) {
    draw_background_rows(buf, w, h, t, 0, h, wallpaper, dark);
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
        // §3.5：菜单名按语言取（`menu_label` 取不到英文会退回中文，不会空白）
        for i in 0..strings::MENUS.len() {
            let menu = strings::menu_label(i, crate::i18n::lang());
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
        //
        // 2026-09-30 P4：**这块同时是控制中心的入口**（状态簇）。刻意不额外塞一个图标按钮 ——
        // "状态"与"操作"放同一处正是 Win11/macOS 状态簇的语义，也少一个视觉元素。
        // 走 i18n：`label_en()` 早就写好了，此前调用处只用了 `label()`（英文界面里
        // 状态栏仍是中文 —— 英文实拍发现，审计附录 DL）。
        let ai_label = crate::i18n::pick(
            crate::i18n::lang(),
            ui.ai_status.label(),
            ui.ai_status.label_en(),
        );
        let ai_w = tr.measure(ai_label, font::CAPTION);
        let ai_x = clock_x as f32 - 20.0 - ai_w;
        let cluster = Rect {
            x: ai_x as i32 - 18,
            y: 2,
            w: ai_w as i32 + 32,
            h: metric::MENUBAR_H - 4,
        };
        if cluster.contains(ui.mouse.0, ui.mouse.1) || self.control_open {
            // 悬停/展开时给一层底，用户才知道这里能点
            rounded_rect(buf, w, h, cluster, radius::SM, color::hairline(), state::hover());
        }
        rounded_rect(buf, w, h, Rect { x: ai_x as i32 - 12, y: 13, w: 6, h: 6 }, 3.0, ui.ai_status.color(), 0.95);
        draw_text(tr, buf, w, h, ai_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::CAPTION), ai_label, font::CAPTION, if self.control_open { color::text() } else { color::text_dim() }, 0.9);
        self.status_cluster = cluster;

        // 2026-09-29 P2：**删掉假电量图标**。它是画上去的固定 50% 填充，既没有
        // /sys/class/power_supply 数据源也不代表任何状态 —— 桌面上的假信息比没有更糟。
        // 真有电池节点时再按真实读数画（P3/P4 接 ACPI）。
    }

    /// 控制中心（P4）：顶栏状态簇点开的面板。
    ///
    /// **只放真实能用的东西**（本项目对假控件零容忍，P2 已删掉假电量与假搜索胶囊）：
    /// - AI 三态只读展示（数据来自 `ui.ai_status`）
    /// - 中文输入法开关 → `ime.toggle()`（主循环执行，`ime_on` 只负责显示同步）
    /// - 24 小时制开关 → 写 `settings` 并落盘，与设置中心**同一份数据**
    /// - 快捷跳转：设置 / 终端（复用 `open_app` 那条真实路径）
    ///
    /// **故意不做的**：音量与亮度。系统没有音频栈、也没有背光节点 —— 放两个拖不动的滑块
    /// 就是纯装饰。等真有 `aetherd`/`/sys/class/backlight` 能力再加。
    fn draw_control_center(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        ui: &UiState,
        tr: Option<&TextRenderer>,
    ) {
        use crate::widgets;
        const PANEL_W: i32 = 264;
        // 高度由行数推导（见 control_layout）：加行自动变高，不存在"忘了同步"
        const PANEL_H: i32 = control_layout::PANEL_H;
        // 右对齐到状态簇（并保证不出屏）
        let x = (self.status_cluster.x + self.status_cluster.w - theme::size::px(PANEL_W)).max(8);
        let panel = Rect {
            x,
            y: metric::MENUBAR_H as i32 + 6,
            w: PANEL_W,
            // 面板高度：跟着缩放，但**夹在屏幕可用高度内**（不让它跑出屏幕）。
            // 150% 下内容仍装不下（需滚动或两栏，见审计附录 CZ）——
            // 这里保证的是「面板本身不出屏」，不是「内容一定放得下」。
            h: theme::size::px(PANEL_H).min(
                (h as i32 - (metric::MENUBAR_H as i32 + theme::size::px(6)) - theme::size::px(8))
                    .max(theme::size::px(120)),
            ),
        };
        shadow(buf, w, h, panel, radius::MD, elevation::elev_2());
        rounded_rect(buf, w, h, panel, radius::MD, color::glass(), color::glass_alpha(true));
        rounded_outline(buf, w, h, panel, radius::MD, color::hairline(), 0.22);
        self.control_hits.clear();
        self.control_panel = panel;
        let Some(tr) = tr else { return };
        // 内容裁剪到面板内：滚起来时上半部分不该画到菜单栏上（审计附录 DD）
        tr.set_clip(Some((panel.x, panel.y, panel.x + panel.w, panel.y + panel.h)));

        // 行矩形。**完全在面板之外的行挪到屏幕外**（y = -1000）：
        // 渲染器会跳过屏外像素，于是文字不画、命中区也失效 ——
        // 看不见的行本来就不该能点。这样滚动时内容不会溢出到面板外面。
        let row = |y: i32| {
            // 行高必须 **≥ 步长**，否则相邻行重叠（实测：写死 34 而动作行步长是 32
            // → 100% 下就重叠 2px、150% 下 3px；几何断言报的 14 处就是它）。
            // 用步长常量本身，则「步长 ≥ 行高」对所有行成立：
            // 动作行 32 = 32（不重叠），开关行 36 > 32（留 4px 缝）。
            let h = theme::size::px(control_layout::ROW_H_ACTION);
            let visible = y + h > panel.y && y < panel.y + panel.h;
            Rect {
                x: panel.x + theme::size::px(4),
                y: if visible { y } else { -1000 },
                w: panel.w - theme::size::px(8),
                h,
            }
        };
        // 与设置中心同一手法：**只登记开关本身**，整行与开关都登记会让一次点击命中两个目标
        let knob = |r: Rect| Rect {
            x: r.x + r.w - theme::size::px(54),
            y: r.y + theme::size::px(7),
            w: theme::size::px(46),
            h: theme::size::px(20),
        };

        // 内容从「面板顶 + 留白 - 滚动量」开始：滚轮滚动的是内容，面板本身不动。
    let content_top = panel.y + theme::size::px(theme::size::TIGHT);
    // 夹取滚动量（内容高度是上一帧量出来的；首帧还不知道就别夹，免得把参数抹成 0）
    if self.control_content_h > 0 {
        let max_scroll = (self.control_content_h - panel.h).max(0);
        if self.control_scroll > max_scroll {
            self.control_scroll = max_scroll;
        }
        if self.control_scroll < 0 {
            self.control_scroll = 0;
        }
    }
    let mut y = content_top - self.control_scroll;
        // ① AI 三态（只读）
        rounded_rect(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::INSET_SM), y: y + theme::size::px(6), w: 8, h: 8 }, 4.0, ui.ai_status.color(), 0.95);
        tr.draw(buf, w, h, (panel.x + theme::size::px(32)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), crate::i18n::tr("AI 中枢", "AI core"), font::CAPTION, color::text(), 0.9);
        tr.draw(buf, w, h, (panel.x + theme::size::px(120)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), crate::i18n::pick(crate::i18n::lang(), ui.ai_status.label(), ui.ai_status.label_en()),
            font::CAPTION, color::text_dim(), 0.9);
        y += theme::size::px(26);
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - theme::size::px(24), h: 1 });
        y += 8;

        // ② 深色模式（默认关 = 白色；用户最常切的一项放最前）
        let r0 = row(y);
        widgets::row(buf, w, h, r0, crate::i18n::tr("深色模式", "Dark mode"), None, widgets::States::at(r0, ui.mouse), Some(tr));
        let s0 = knob(r0);
        widgets::switch(buf, w, h, s0, self.settings.dark_mode, widgets::States::at(s0, ui.mouse));
        self.control_hits.push((s0, ControlHit::ToggleDark));
        y += theme::size::px(control_layout::ROW_H_SWITCH);

        // ③ 中文输入法
        let r1 = row(y);
        widgets::row(buf, w, h, r1, crate::i18n::tr("中文输入法", "Chinese IME"), None, widgets::States::at(r1, ui.mouse), Some(tr));
        let s1 = knob(r1);
        widgets::switch(buf, w, h, s1, self.ime_on, widgets::States::at(s1, ui.mouse));
        self.control_hits.push((s1, ControlHit::ToggleIme));
        y += theme::size::px(control_layout::ROW_H_SWITCH);

        // ③ 24 小时制
        let r2 = row(y);
        widgets::row(buf, w, h, r2, crate::i18n::tr("24 小时制", "24-hour clock"), None, widgets::States::at(r2, ui.mouse), Some(tr));
        let s2 = knob(r2);
        widgets::switch(buf, w, h, s2, self.settings.clock_24h, widgets::States::at(s2, ui.mouse));
        self.control_hits.push((s2, ControlHit::ToggleClock24h));
        y += theme::size::px(control_layout::ROW_H_SWITCH);

        // ④ 快捷跳转
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y, crate::i18n::tr("快捷跳转", "Quick actions"), Some(tr));
        y += theme::size::px(20);
        // 「通知」带**真实未读数**（不是装饰性的小红点）：没有未读时不显示数字，
        // 因为"0 条未读"和"没有通知中心"在界面上应该长得不一样。
        let unread = self.notifs.unread();
        // 三元组：中文 / English / 命中。**两种语言写在一起**，因此不可能漏翻；
        // 并且有覆盖性测试盯着这张表（`every_control_center_entry_is_bilingual`）。
        for (zh, en, hit) in [
            ("打开设置", "Open Settings", ControlHit::OpenSettings),
            ("打开终端", "Open Terminal", ControlHit::OpenTerminal),
        ] {
            let r = row(y);
            let label = crate::i18n::pick(crate::i18n::lang(), zh, en);
            widgets::row(buf, w, h, r, label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, hit));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }
        // 「通知」单独走一支：它的文案**带未读数**（动态），
        // 而 `i18n::pick` 要求 `&'static str`（那是"两种语言写在调用点"这个设计的代价）。
        // 动态文案用语言分支写，同样两种语言并排 —— 只是换了个形式。
        {
            let label = if unread > 0 {
                if crate::i18n::lang() == crate::i18n::Lang::En {
                    format!("Notifications ({unread})")
                } else {
                    if crate::i18n::is_en() {
                        format!("Notifications ({unread} unread)")
                    } else {
                        format!("通知（{unread} 条未读）")
                    }
                }
            } else {
                crate::i18n::tr("通知", "Notifications").to_string()
            };
            let r = row(y);
            widgets::row(buf, w, h, r, &label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, ControlHit::Notifications));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }

        // ⑤ 音量（批次 K-2）
        //
        // **不可用时不画滑块**（本项目对假控件零容忍）：只显示一行如实的状态文案，
        // 且不登记命中区 —— "画一个拖不动的滑块"比"没有滑块"更糟。
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y, crate::i18n::tr("声音", "Sound"), Some(tr));
        y += theme::size::px(20);
        {
            let r = row(y);
            if ui.audio_enabled {
                widgets::row(buf, w, h, r, crate::i18n::tr("音量", "Volume"), Some(&format!("{}%", ui.audio_volume)),
                             widgets::States::at(r, ui.mouse), Some(tr));
                // 轨道：底槽 + 已填充部分。点击整条轨道即可设定（不需要拖拽，
                // 也就不需要给事件循环加"拖拽状态"——少一处状态就少一类 bug）
                let track = Rect { x: r.x + theme::size::px(theme::size::TIGHT), y: r.y + r.h - 12, w: r.w - 24, h: 6 };
                rounded_rect(buf, w, h, track, 3.0, color::surface_3(), 0.9);
                let fill_w = (track.w as f32 * ui.audio_volume as f32 / 100.0) as i32;
                if fill_w > 0 {
                    rounded_rect(buf, w, h, Rect { x: track.x, y: track.y, w: fill_w, h: track.h },
                                 3.0, color::accent(), 0.95);
                }
                self.control_hits.push((track, ControlHit::VolumeTrack));
            } else {
                widgets::row(buf, w, h, r, crate::i18n::tr("音量", "Volume"), Some(ui.audio_label),
                             widgets::States::at(r, ui.mouse), Some(tr));
                // **不登记命中区**：不可用时点不动，也就不该有可点击区域
            }
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }

        // ⑦ 虚拟桌面（§5.4）：把"切"之外的"增删"也放到手边
        //
        // 为什么放在控制中心而不是再占一个快捷键：`Super+1..9` 已经是"切到第 N 个"，
        // 再给增删各配一个组合键既难记又容易误触；而"给桌面加一个"是低频动作，
        // 收进控制中心正合适。行数由 `theme::size::px(control_layout::SPACE_ROWS)` 决定（面板自动变高）。
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(
            buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y,
            &format!(
                "{}（{}）",
                crate::i18n::tr("虚拟桌面", "Virtual desktops"),
                self.space_count
            ),
            Some(tr),
        );
        y += theme::size::px(20);
        for (zh, en, hit) in [
            ("新增桌面", "New desktop", ControlHit::AddSpace),
            ("移除当前桌面", "Remove this desktop", ControlHit::RemoveSpace),
        ] {
            let r = row(y);
            let label = crate::i18n::pick(crate::i18n::lang(), zh, en);
            widgets::row(buf, w, h, r, label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, hit));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }

        // ⑥ 电源（2026-10-02 功能批次 J-2）
        //
        // 放在最下面而不是最上面：关机/重启是**低频且不可逆**的动作，
        // 放在开关旁边会让人手滑点错（Windows/macOS 也把它收在菜单末尾）。
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y, crate::i18n::tr("电源", "Power"), Some(tr));
        y += theme::size::px(20);
        let power: [(&str, &str, ControlHit); 3] = [
            ("关机", "Power off", ControlHit::PowerOff),
            ("重启", "Restart", ControlHit::Reboot),
            ("注销", "Log out", ControlHit::Logout),
        ];
        for (zh, en, hit) in power {
            let r = row(y);
            let label = crate::i18n::pick(crate::i18n::lang(), zh, en);
            widgets::row(buf, w, h, r, label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, hit));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }
        // 量出内容实际高度（供滚动夹取）。页脚**钉在面板底**，不参与滚动。
        self.control_content_h =
            (y - content_top).max(0) + theme::size::px(control_layout::FOOTER);
        tr.draw(
            buf, w, h,
            (panel.x + theme::size::px(theme::size::INSET_SM)) as f32,
            (panel.y + panel.h - theme::size::px(22)) as f32,
            crate::i18n::tr("Esc 或点击别处关闭", "Esc or click elsewhere to close"),
            font::LABEL, color::text_dim(), 0.7,
        );
        tr.set_clip(None);
    }

    /// 向导里被点中的**选项下标**（§5.8）。没有命中给 `None`。
    ///
    /// 与 `palette_hit` 同一做法：命中区由绘制产生，
    /// 所以**只有真的画出来的按钮才能被点到**（画不下就不画、也就不登记）。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn setup_hit(&self, x: f32, y: f32) -> Option<usize> {
        self.setup_hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, idx)| *idx)
    }

    /// 命令面板里被点中的**条目下标**（§5.5）。没有命中给 `None`。
    ///
    /// 用 `palette_hits`（上一帧绘制时登记的）来判断 —— 这是"命中区由绘制产生"的
    /// 一贯做法（桌面图标、设置项、控制中心都这样），好处是**只有真的画出来的东西才能被点到**。
    /// 调用点在 Linux 的 fbdev 循环里；非 Linux 上只有测试用它 —— 按平台 allow。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn palette_hit(&self, x: f32, y: f32) -> Option<usize> {
        self.palette_hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, item)| *item)
    }

    /// 展开中的下拉菜单（登记各项命中区，悬停高亮）。
    fn draw_dropdown(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let Some(mi) = ui.open_menu else { return };
        let Some(label) = self.menubar_menus.get(mi) else { return };
        let Some(tr) = tr else { return };
        // §3.5：下拉项同样双语；**数量以中文表为准**（两张表形状有测试钉住）
        let lang = crate::i18n::lang();
        let items: Vec<&str> = (0..strings::menu_item_count(mi))
            .map(|j| strings::menu_item(mi, j, lang))
            .collect();
        let panel_w = items
            .iter()
            .map(|s| tr.measure(s, font::BODY))
            .fold(120.0f32, f32::max)
            + 44.0;
        let panel = Rect { x: label.x, y: metric::MENUBAR_H + theme::size::px(4), w: panel_w as i32, h: items.len() as i32 * theme::size::px(30) + theme::size::px(8) };
        shadow(buf, w, h, panel, radius::MD, elevation::elev_2());
        rounded_rect(buf, w, h, panel, radius::MD, color::surface_2(), 0.98);
        rounded_outline(buf, w, h, panel, radius::MD, color::hairline(), 0.12);

        let mut rects = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let ir = Rect { x: panel.x + 4, y: panel.y + theme::size::px(4) + i as i32 * theme::size::px(30), w: panel.w - 8, h: theme::size::px(theme::size::CTRL_H_SM) };
            if ir.contains(ui.mouse.0, ui.mouse.1) {
                rounded_rect(buf, w, h, Rect { x: ir.x + 4, y: ir.y + 3, w: ir.w - 8, h: ir.h - 6 }, radius::SM, color::surface_3(), 0.9);
            }
            draw_text(tr, buf, w, h, (ir.x + theme::size::px(theme::size::INSET_SM)) as f32, tr.vcenter(ir.y as f32, ir.h as f32, font::BODY), item, font::BODY, color::text(), 0.92);
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
    ctx: PageCtx,
    t: f32,
    fv: &FileView,
    cells: &mut Vec<(Rect, usize)>,
    up: &mut Rect,
    win_idx: usize,
    lights_out: &mut Vec<(usize, Rect, Rect, Rect)>,
    sidebar_hits: &mut Vec<(Rect, usize)>,
    crumb_hits: &mut Vec<(Rect, String)>,
    music_hits: &mut Vec<(Rect, usize)>,
    settings_page: usize,
    settings_hits: &mut Vec<(Rect, crate::settings::SettingsHit)>,
) {
    // 这里只解构窗口自身要用的两个：其余字段（设置/四个视图）是**原样转发**给
    // `draw_settings_content` 的，解出来只会得到五个"未使用变量"警告。
    let PageCtx { mouse, tr, .. } = ctx;
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

    // Windows 风格标题栏按钮（右上）：最小化 / 最大化 / 关闭。
    //
    // 2026-10-01：**换掉 macOS 红绿灯**（用户要求）。不只是审美：红绿灯圆点直径只有 10px，
    // 命中区必须外扩才好点、还容易误触；Windows 按钮是 46px × 标题栏高，命中区就是按钮本身。
    // 三键**全部接线**（最小化把窗口收进 Dock，靠 `Win.minimized`），没有装饰性按钮。
    let btn_w = metric::CAPTION_BTN_W;
    let close_rect = Rect { x: r.x + r.w - btn_w, y: r.y, w: btn_w, h: metric::TITLE_H };
    let zoom_rect = Rect { x: close_rect.x - btn_w, y: r.y, w: btn_w, h: metric::TITLE_H };
    let min_rect = Rect { x: zoom_rect.x - btn_w, y: r.y, w: btn_w, h: metric::TITLE_H };
    for (br, kind) in [(min_rect, 0u8), (zoom_rect, 1), (close_rect, 2)] {
        let hover = active && br.contains(mouse.0, mouse.1);
        // 悬停：关闭键是 Windows 的红底白叉，另两个是浅灰底
        if hover {
            let (bg, a) = if kind == 2 { (color::close_hover(), 1.0) } else { (color::hairline(), 0.16) };
            fill_rect(buf, w, h, br, bg, a);
        }
        let glyph = if hover && kind == 2 {
            color::HIGHLIGHT
        } else if active {
            color::text()
        } else {
            color::text_dim()
        };
        caption_glyph(buf, w, h, br, kind, glyph, if active { 0.9 } else { 0.45 });
    }
    lights_out.push((win_idx, min_rect, zoom_rect, close_rect));

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
        WinKind::Music => draw_music_content(buf, w, h, content, r, mouse, tr, music_hits),
        // Preview 暂时复用文件管理器的纸面底，真正的预览渲染在后续阶段接入
        WinKind::Files => draw_files_content(
            buf, w, h, content, r, mouse, tr, fv, cells, up, sidebar_hits, crumb_hits,
        ),
        WinKind::Preview => draw_preview_content(buf, w, h, content, win.preview.as_ref(), tr),
        WinKind::Wayland => draw_wayland_content(buf, w, h, content, win.wayland.as_ref()),
        WinKind::Settings => draw_settings_content(
            buf, w, h, content, ctx, settings_page, settings_hits,
        ),
    }
    // 正文画完立刻清裁剪：否则后面画的菜单栏/AI 气泡/Dock 会被一起裁掉（附录 DI）。
    // 外层 tr 是 Option，所以要 if let。
    if let Some(t) = tr {
        t.set_clip(None);
    }
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

/// 设置中心的内容（P3）：左侧分组导航 + 右侧页面。
///
/// 结构对齐 Windows 11 / macOS 的设置（左分组、右页面），但**只用真实能力**：
/// 能用的页给真控件，不能用的页在左栏置灰并标「待接入」—— 本项目对"点了没用的假控件"
/// 是零容忍（P2 刚删掉假电量与假搜索胶囊），所以宁可少画。
///
/// 绘制期**只登记命中区**（`hits`），不改任何状态：状态变更由事件循环按命中结果执行。
/// 画一段可能超宽的文字，并把**多占的高度补进 `ry`** —— 调用点折行的统一写法。
///
/// **为什么必须在调用点折行**：渲染层偷偷折行会让布局不知道多占了行，从而压住下一行
/// （附录 DO 的实测结论）。把"折行"与"补高度"绑在一起，就不会只改一半。
///
/// `max_w` 传内容区可用宽度；最多折 3 行，再放不下由 `wrap()` 自己截断补 `…`。
fn draw_wrapped(
    tr: &TextRenderer,
    buf: &mut [u32],
    w: usize,
    h: usize,
    x: f32,
    ry: &mut i32,
    text: &str,
    px: f32,
    rgb: [u8; 3],
    alpha: f32,
    max_w: f32,
) {
    let step = theme::size::px(20);
    let segs = tr.wrap(text, px, max_w, 3);
    for (i, seg) in segs.iter().enumerate() {
        tr.draw(buf, w, h, x, (*ry + i as i32 * step) as f32, seg, px, rgb, alpha);
    }
    *ry += (segs.len() as i32 - 1) * step;
}

fn draw_settings_content(
    buf: &mut [u32],
    w: usize,
    h: usize,
    r: Rect,
    ctx: PageCtx,
    page: usize,
    hits: &mut Vec<(Rect, crate::settings::SettingsHit)>,
) {
    // 解构成**与原来同名**的局部变量：这样函数体一行都不用改（重构风险最低的做法）
    let PageCtx {
        mouse,
        tr,
        settings: s,
        dns: dns_field,
        pass: pass_form,
        users,
        users_loaded,
        peripherals,
        peripherals_loaded,
        audit_query,
        policy,
        store,
        time,
        snapshots,
        update,
        displays,
        driven_index,
        displays_loaded,
        removables,
        removables_loaded,
        removable_pending,
        removable_message,
    } = ctx;
    use crate::settings::{SettingsHit, PAGES};
    use crate::widgets;
    const SIDEBAR_W: i32 = 168;

    // 左栏底：用 inset 与右侧页面区分（设置窗口是"两栏"，不是一整块纸）
    let side = Rect { x: r.x, y: r.y, w: SIDEBAR_W, h: r.h };
    fill_rect(buf, w, h, side, color::inset(), 0.55);
    fill_rect(buf, w, h, Rect { x: side.x + side.w - 1, y: side.y, w: 1, h: side.h }, color::hairline(), 0.08);

    let Some(tr) = tr else { return };

    // ---- 左栏：分组标题 + 页行 ----
    let mut y = r.y + theme::size::px(theme::size::TIGHT);
    let mut group = "";
    // 语言取一次即可（绘制期不变）。
    // 注意：这里必须写全限定 `crate::i18n::tr` —— 本文件的 `tr` 是**文本渲染器**的
    // 局部变量名，直接用 `tr(...)` 会被它遮蔽（编译报"not callable"）。
    let lang = crate::i18n::lang();
    // 左栏必须**放得下全部页**：页数一多，固定 30px 行高就会溢出，
    // 而溢出的那一页**点不到**（命中去跑到窗口外）—— 加页面的那次改动就踩了这条
    // （有测试盯着"命中区必须都在窗口内"，所以是测试先红，不是用户先发现）。
    // 按可用高度自适应行高，下限 20px：再小就不是给人点的了，那种情况才裁剪。
    let groups: i32 = {
        let mut n: i32 = 0;
        let mut last = "";
        for p in PAGES.iter() {
            if p.group != last {
                n += 1;
                last = p.group;
            }
        }
        n
    };
    // 正文裁剪到**窗口矩形**：折行/超长的行不该画到窗口外面去（附录 DI）。
    // 注意不能用 content —— 可滚动页面的 content 是完整内容高度，裁到它等于没裁。
    tr.set_clip(Some((r.x, r.y, r.x + r.w, r.y + r.h)));
    // 只读报表：记录正文里哪些文字超过内容区宽度（不改变绘制）
    tr.set_over_width(Some((r.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32));
    let chrome = 12 + groups * 20 + groups.saturating_sub(1) * 10;
    let row_h = (((r.h - chrome).max(0)) / PAGES.len().max(1) as i32).clamp(20, 30);
    for (i, p) in PAGES.iter().enumerate() {
        // **按原文比较**（`p.group`），只在绘制时翻译：拿翻译后的文本互相比较的话，
        // 同一分组会因为两次取到的文本不同而重复出标题。
        if p.group != group {
            if !group.is_empty() {
                y += 10;
            }
            widgets::group_header(buf, w, h, r.x + theme::size::px(theme::size::INSET_SM), y, p.group_in(lang), Some(tr));
            y += theme::size::px(20);
            group = p.group;
        }
        // 仍然放不下（页数极多）：如实裁剪而不是画到窗口外
        if y + row_h > r.y + r.h {
            break;
        }
        let row = Rect { x: r.x + theme::size::px(theme::size::PAD_SM), y, w: SIDEBAR_W - 16, h: row_h };
        let st = widgets::States {
            hover: p.enabled && row.contains(mouse.0, mouse.1),
            selected: i == page,
            disabled: !p.enabled,
            ..Default::default()
        };
        widgets::row(buf, w, h, row, p.title_in(lang), None, st, Some(tr));
        if p.enabled {
            hits.push((row, SettingsHit::Page(i)));
        } else {
            // 不可用的页如实标注，而不是让用户点进去发现是空壳
            let tag_text = crate::i18n::tr("待接入", "Coming soon");
            let tag = tr.measure(tag_text, font::LABEL);
            tr.draw(
                buf, w, h,
                (row.x + row.w - 8) as f32 - tag,
                tr.vcenter(row.y as f32, row.h as f32, font::LABEL),
                tag_text, font::LABEL, color::text_dim(), 0.5,
            );
        }
        y += theme::size::px(32);
        if y > r.y + r.h - 40 {
            break; // 窗口再矮也不越界
        }
    }

    // ---- 右侧页面 ----
    let content = Rect { x: r.x + SIDEBAR_W, y: r.y, w: r.w - SIDEBAR_W, h: r.h };
    // 大标题也要走 i18n：侧栏用的是 title_in(lang)，标题此前只取中文
    // —— 英文模式下同一屏里侧栏是 Appearance、标题却是「外观」（英文实拍发现，附录 DN）。
    // 注意 `title` 有**两个**用途：① 画大标题；② 下面的 `match` 用它**分发页面内容**。
    // 所以中文原文必须保留一份给 match —— 只翻译显示用的那一份（附录 DN 的教训）。
    let title_zh = PAGES.get(page).map(|p| p.title).unwrap_or("设置");
    let title = PAGES
        .get(page)
        .map(|p| p.title_in(lang))
        .unwrap_or_else(|| crate::i18n::pick(lang, "设置", "Settings"));
    tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, (content.y + 18) as f32, title, font::PAGE_TITLE, color::text(), 0.98);
    widgets::divider(buf, w, h, Rect { x: content.x + theme::size::px(theme::size::CONTENT_PAD), y: content.y + 54, w: (content.w - 48).max(0), h: 1 });

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
        let sw = Rect { x: row.x + row.w - 58, y: row.y + theme::size::px(theme::size::TIGHT), w: 46, h: 20 };
        widgets::switch(buf, w, h, sw, on, widgets::States::at(sw, mouse));
        hits.push((sw, id));
    }

    match title_zh {
        "权限与隐私" => {
            // §3.5：这一页的文案全部走 `pick(lang, …)` —— 它是整句说明最多的页面，
            // 也是安全相关的页面，中英文都要**说得准**。
            let lang = crate::i18n::lang();
            // 数据来源：aetherd 权限门的审计日志（`perm::Gate::audit` 追加写），
            // 解析与单测见 `audit.rs`。**只读展示** —— 这一页不改任何权限。
            //
            // 三条如实原则：① 没记录就说"暂无记录"，不显示空表；② 坏行被**计数并显示**，
            // 不静默丢弃（审计页最不该做的事就是假装没有记录）；③ 时间只说"多久以前"，
            // 不编造日历时间（Unix 秒转本地日期要一套历法换算，与这一页的目的无关）。
            // `now_utc_secs()` 是 i64（时钟可能被回拨成负数）；相对时间只需非负值。
            let now = crate::text::now_utc_secs().max(0) as u64;
            // §5.2：读满（与导出的上限一致）——以前只读 8 条，
            // 于是"导出里查得到的历史，界面上看不到"。
            let info = crate::audit::read(crate::audit::AUDIT_PATH, crate::audit::PAGE_MAX);
            // §5.2 导出按钮：把审计日志写成可贴到 issue 的纯文本。
            // 放在标题上方（列表下方会随条目数浮动，位置不稳定）。
            let export = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H_SM) };
            widgets::row(
                buf,
                w,
                h,
                export,
                crate::i18n::pick(lang, "导出审计日志", "Export audit log"),
                Some(crate::i18n::pick(
                    lang,
                    "写到 /var/log/aether/，并显示路径",
                    "Writes to /var/log/aether/ and shows the path",
                )),
                widgets::States::at(export, mouse),
                Some(tr),
            );
            hits.push((export, SettingsHit::ExportAudit));
            ry += theme::size::px(theme::size::ROW_TALL);
            widgets::group_header(
                buf,
                w,
                h,
                content.x + theme::size::px(theme::size::CONTENT_PAD),
                ry,
                crate::i18n::pick(lang, "审计日志", "Audit log"),
                Some(tr),
            );
            ry += theme::size::px(theme::size::LINE_GAP);
            // §5.2 搜索框：点一下进入编辑态（`audit_editing`），
            // 之后按键由事件循环送进 `audit_query`（与 DNS 输入框同一套做法）。
            let qrow = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: 34 };
            let qtext = if audit_query.is_empty() {
                crate::i18n::pick(
                    lang,
                    "搜索：工具名 / 参数 / 结论（点这里输入）",
                    "Search: tool / args / verdict (click to type)",
                )
                .to_string()
            } else {
                format!(
                    "{}{}▏",
                    crate::i18n::pick(lang, "搜索：", "Search: "),
                    audit_query
                )
            };
            widgets::row(
                buf, w, h, qrow, &qtext, None,
                widgets::States::at(qrow, mouse), Some(tr),
            );
            hits.push((qrow, crate::settings::SettingsHit::AuditQuery));
            ry += theme::size::px(42);
            if !info.exists {
                tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                        crate::i18n::pick(
                            lang,
                            "暂无审计记录（本会话还没发生工具调用）。",
                            "No audit records yet (no tool call has happened this session).",
                        ),
                        font::CAPTION, color::text_dim(), 0.9);
                ry += theme::size::px(theme::size::HINT_GAP);
            } else {
                // 有搜索词时如实说"命中几条 / 读了几条"——只报总数会让人以为列表被截断
                let idx = crate::audit::matching(&info.recent, audit_query);
                let mut line = if audit_query.trim().is_empty() {
                    // `format!` 的模板必须是字面量 → 按语言分支（i18n 文档里的既定做法）
                    if crate::i18n::is_en() {
                        format!(
                            "{} entries · {} denied · {} failed",
                            info.total, info.denied, info.failed
                        )
                    } else {
                        format!(
                            "共 {} 条 · 被拒绝 {} · 失败 {}",
                            info.total, info.denied, info.failed
                        )
                    }
                } else if crate::i18n::is_en() {
                    format!(
                        "{} matched / {} read · {} denied · {} failed",
                        idx.len(),
                        info.recent.len(),
                        info.denied,
                        info.failed
                    )
                } else {
                    format!(
                        "命中 {} / 已读 {} 条 · 被拒绝 {} · 失败 {}",
                        idx.len(),
                        info.recent.len(),
                        info.denied,
                        info.failed
                    )
                };
                if info.unparsed > 0 {
                    if crate::i18n::is_en() {
                        line.push_str(&format!(" · {} unparsable", info.unparsed));
                    } else {
                        line.push_str(&format!(" · 无法解析 {}", info.unparsed));
                    }
                }
                tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, &line, font::BODY, color::text(), 0.92);
                ry += theme::size::px(theme::size::GROUP_GAP);
                tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, &info.path, font::CAPTION, color::text_dim(), 0.7);
                ry += theme::size::px(theme::size::HINT_GAP);
                if idx.is_empty() {
                    tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                            crate::i18n::pick(
                                lang,
                                "没有匹配的记录 —— 换个词，或清空搜索框。",
                                "No matching records — try another word, or clear the search box.",
                            ),
                            font::CAPTION, color::text_dim(), 0.9);
                    ry += theme::size::px(theme::size::GROUP_GAP);
                }
                for i in &idx {
                    let e = &info.recent[*i];
                    let dot = if e.is_denied() {
                        color::close_hover() // 被拒绝：红（与标题栏关闭键同一支红，语义一致）
                    } else if e.is_failed() {
                        color::text_dim() // 失败：中性灰（不为它再造一个颜色）
                    } else {
                        color::accent()
                    };
                    // 圆点尺寸（6px）**故意不缩**（见 size 模块注释），但它的**位置**要跟着行高走
            rounded_rect(buf, w, h, Rect { x: content.x + theme::size::px(22), y: ry + theme::size::px(5), w: 6, h: 6 }, 3.0, dot, 0.95);
                    // §3.5：日志里存 token（L2），显示时按界面语言取词（旧日志里的人话名原样透传）
                    let level_txt = crate::audit::level_text(crate::i18n::lang(), &e.level);
                    // §3.5：verdict 也过显示函数（日志里存的是 token/码）
                    let verdict_txt =
                        crate::audit::verdict_text(crate::i18n::lang(), &e.verdict);
                    let head = format!("{} · {} · {}", level_txt, e.tool, verdict_txt);
                    tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::INDENT_SUB)) as f32, ry as f32, &head, font::CAPTION, color::text(), 0.9);
                    // §3.5：相对时间也有英文版（`audit.rs` 是数据层，不依赖 i18n）
                    let age = if crate::i18n::is_en() {
                        e.age_text_en(now)
                    } else {
                        e.age_text(now)
                    };
                    let aw = tr.measure(&age, font::CAPTION);
                    tr.draw(buf, w, h, (content.x + row_w + 10) as f32 - aw, ry as f32, &age, font::CAPTION, color::text_dim(), 0.8);
                    ry += theme::size::px(theme::size::GAP_STD);
                    if !e.args.is_empty() {
                        let a: String = e.args.chars().take(52).collect();
                        let a = if e.args.chars().count() > 52 { format!("{a}…") } else { a };
                        tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::INDENT_SUB)) as f32, ry as f32, &a, font::CAPTION, color::text_dim(), 0.6);
                        ry += 18;
                    }
                    if ry > content.y + content.h - 40 {
                        break;
                    }
                }
            }
            ry += theme::size::px(theme::size::GAP_TIGHT);
            // 长句按**量出来的宽度**截断：不然它会横穿出设置窗口（附录 DI，实拍发现）。
            // 长句**折行**（不是截掉）：实拍发现它会横穿出设置窗口（附录 DI）。
            // 这一行是该页最后一行，折行只往下多占一两行，不会顶掉别的内容。
            let lv = crate::i18n::pick(
                lang,
                "等级：L0 只读 / L1 需确认 / L2 高危需显式确认。改权限配置需要 IPC 与用户确认（下一步）。",
                "Levels: L0 read-only / L1 needs confirmation / L2 high-risk needs explicit confirmation. Changing the policy needs IPC and user confirmation (next step).",
            );
            let lv_w = (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32;
            for (li, seg) in tr.wrap(&lv, font::CAPTION, lv_w, 3).iter().enumerate() {
                tr.draw(
                    buf,
                    w,
                    h,
                    (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                    (ry + li as i32 * theme::size::px(18)) as f32,
                    seg,
                    font::CAPTION,
                    color::text_dim(),
                    0.7,
                );
            }
        }
        "应用商店" => {
            // §5.11：列出可安装包。内容来自 `Renderer::store`（进页面时才扫描）——
            // **绘制不读磁盘**：每帧扫一遍目录是自找的卡顿。
            use crate::store as st;
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("可安装的应用", "Available apps"), Some(tr),
            );
            ry += theme::size::px(theme::size::LINE_GAP);
            draw_wrapped(
                tr,
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                crate::i18n::tr(
                    crate::i18n::pick(lang, "点一行即安装（装之前会弹确认卡片，还会做依赖预检）。", "Click a row to install (a confirmation card pops up first, plus a dependency pre-check)."),
                    "Click a row to install (a confirmation card appears; dependencies are pre-checked).",
                ),
                font::CAPTION, color::text_dim(), 0.85,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            ry += theme::size::px(28);

            if let Some(err) = store.error() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    &format!("{}：{err}", crate::i18n::tr("读取失败", "Failed to load")),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            } else if !store.is_scanned() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("正在扫描…", "Scanning…"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            } else if store.is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("还没有可安装的应用", "No apps available yet"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            }

            let rows: Vec<(String, String)> = (0..store.items().len())
                .map(|i| (store.items()[i].name.clone(), store.row_hint(i, lang)))
                .collect();
            for (i, (name, hint)) in rows.iter().enumerate() {
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                // 「已安装」那一行**不给命中区**：它不该像个按钮一样能点
                let clickable = store.action_of(i).map(|a| a.actionable()).unwrap_or(false);
                widgets::row(
                    buf, w, h, row, name, Some(hint),
                    widgets::States { disabled: !clickable, ..widgets::States::at(row, mouse) },
                    Some(tr),
                );
                if clickable {
                    hits.push((row, SettingsHit::InstallApp(i)));
                }
                ry += theme::size::px(theme::size::ROW_TALL);
                if ry > content.y + content.h - 60 {
                    break;
                }
            }
            let _ = st::CATALOG_DIR;

            // ---- 已安装：可以卸载 ----
            ry += theme::size::px(theme::size::GAP_TIGHT);
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("已安装", "Installed"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            if store.installed().is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("还没有安装任何应用", "No apps installed yet"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            }
            // 显示的是 **id**（终端里敲的那个命令名），而不是显示名：
            // ① 这里的数据源是已装目录的清单，它只有 id/版本；
            // ② id 正是卸载与启动要用的标识，显示它用户能一眼对上终端里的命令。
            for (i, (id, ver)) in store.installed().iter().enumerate() {
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                let hint = if ver.is_empty() {
                    crate::i18n::tr("点击卸载（移入回收站，删错了还能捞回来）", "Click to uninstall (moved to trash, recoverable)").to_string()
                } else {
                    format!("{ver} · {}", crate::i18n::tr("点击卸载（移入回收站，删错了还能捞回来）", "Click to uninstall (moved to trash, recoverable)"))
                };
                widgets::row(
                    buf, w, h, row, id, Some(&hint),
                    widgets::States::at(row, mouse), Some(tr),
                );
                hits.push((row, SettingsHit::RemoveApp(i)));
                ry += theme::size::px(theme::size::ROW_TALL);
                if ry > content.y + content.h - 60 {
                    break;
                }
            }

            ry += 6;
            let act = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf, w, h, act,
                crate::i18n::tr("重新扫描", "Rescan"),
                Some(crate::i18n::tr("再读一遍商店目录", "Read the catalog directory again")),
                widgets::States::at(act, mouse), Some(tr),
            );
            hits.push((act, SettingsHit::RefreshStore));
        }
        "可移动设备" => {
            // §3.7 最小切片：**只读**挂载。界面上要把这条说清楚 ——
            // 用户点下去得到的是"能看里面的文件"，而不是"能往里写"。
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("检测到的设备", "Detected devices"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            let r_refresh = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H_SM) };
            widgets::row(
                buf, w, h, r_refresh,
                crate::i18n::tr("重新扫描", "Rescan"),
                None,
                widgets::States::at(r_refresh, mouse), Some(tr),
            );
            hits.push((r_refresh, SettingsHit::RefreshRemovables));
            // 步长跟着**缩放后的高度**走：写死的话 150% 下高度变大、步长不变，
            // 下一个元素会落在这一行里面（静态扫描 `scripts/scale-audit.py` 发现，附录 CO）。
            ry += theme::size::px(theme::size::ROW_H_SM) + 4;
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                crate::i18n::tr(
                    "挂载是只读的（ro,nodev,nosuid,noexec）：能看里面的文件，不能往盘里写",
                    "Mounted read-only (ro,nodev,nosuid,noexec): browse files, no writes",
                ),
                font::CAPTION, color::text_dim(), 0.75,
            );
            ry += theme::size::px(theme::size::HINT_GAP);

            if !removables_loaded {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("正在扫描…", "Scanning…"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            } else if removables.is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr(
                        "没有检测到可移动存储（插上 U 盘后点「重新扫描」）",
                        "No removable storage detected (plug in a drive and rescan)",
                    ),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            }
            if let Some(msg) = removable_message {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    msg,
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            }

            let devices: Vec<(String, String)> = removables
                .iter()
                .map(|d| {
                    let size = crate::text::human_bytes(d.bytes);
                    let pending = removable_pending == Some(d.dev.as_str());
                    let hint = if pending {
                        crate::i18n::pick(lang, "正在处理…", "working…").to_string()
                    } else {
                        match &d.mounted_at {
                            Some(mp) => format!(
                                "{size} · {mp} · {}",
                                crate::i18n::pick(lang, "点一下卸载", "click to unmount")
                            ),
                            None => format!(
                                "{size} · {}",
                                crate::i18n::pick(
                                    lang,
                                    "点一下只读挂载",
                                    "click to mount read-only",
                                )
                            ),
                        }
                    };
                    (d.dev.clone(), hint)
                })
                .collect();
            for (i, (dev, hint)) in devices.iter().enumerate() {
                if ry > content.y + content.h - 60 {
                    break;
                }
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                widgets::row(buf, w, h, row, dev, Some(hint), widgets::States::at(row, mouse), Some(tr));
                // 有操作在飞时不登记命中区：不给"点了没反应"的机会
                if removable_pending.is_none() {
                    hits.push((row, SettingsHit::ToggleRemovable(i)));
                }
                ry += theme::size::px(theme::size::ROW_TALL);
            }

            // ---- §3.7 外设：蓝牙 / 打印（**只读检测 + 如实报告**）----
            //
            // 与「显示输出」同一做法：能如实看到的就显示，做不到的明说做不到。
            // 配对、连接、提交打印作业**都不在本版范围** —— 那需要 BlueZ / CUPS 一整套。
            ry += theme::size::px(theme::size::GAP_WIDE);
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("外设（只读检测）", "Peripherals (read-only)"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            if !peripherals_loaded {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("正在检测…", "Detecting…"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                // 本分支之后不再用 ry（下面另起一节），所以不推进
            } else {
                // 蓝牙：一个适配器一行
                let bt: Vec<&aether_ipc::PeripheralWire> =
                    peripherals.iter().filter(|p| p.kind == "bluetooth").collect();
                if bt.is_empty() {
                    tr.draw(
                        buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                        crate::i18n::tr(
                            "蓝牙：未检测到适配器（/sys/class/bluetooth 为空）",
                            "Bluetooth: no adapter detected (/sys/class/bluetooth is empty)",
                        ),
                        font::CAPTION, color::text_dim(), 0.9,
                    );
                    ry += theme::size::px(theme::size::LINE_GAP);
                } else {
                    for a in bt {
                        let line = match &a.address {
                            Some(addr) => format!("{} · {}", a.name, addr),
                            // 地址读不到就如实说"未知"，不编一个
                            None => format!("{} · {}", a.name, crate::i18n::tr("地址未知", "address unknown")),
                        };
                        tr.draw(
                            buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                            &line, font::CAPTION, color::text(), 0.9,
                        );
                        ry += theme::size::px(theme::size::LINE_GAP);
                    }
                }
                // 打印：**如实三分**
                let pr = peripherals.iter().find(|p| p.kind == "printing");
                let text = match pr {
                    Some(p) if !p.cups => crate::i18n::tr(
                        "打印：未检测到打印子系统（没有 CUPS）",
                        "Printing: no printing subsystem detected (no CUPS)",
                    )
                    .to_string(),
                    Some(p) if p.queues.is_empty() => crate::i18n::tr(
                        "打印：CUPS 在，但没有配置任何队列",
                        "Printing: CUPS is present but no queue is configured",
                    )
                    .to_string(),
                    Some(p) => {
                        // 这句带数字，**不能用 `pick`** —— 它只收 `&'static str`；
                        // 与项目其它带参文案一样，按语言各写一遍。
                        if lang == crate::i18n::Lang::En {
                            format!(
                                "Printing: CUPS has {} queue(s): {}",
                                p.queues.len(),
                                p.queues.join(", ")
                            )
                        } else {
                            format!(
                                "打印：CUPS 已配置 {} 个队列：{}",
                                p.queues.len(),
                                p.queues.join("、")
                            )
                        }
                    }
                    None => crate::i18n::tr("打印：未检测到", "Printing: not detected").to_string(),
                };
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    &text, font::CAPTION, color::text(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
                // 边界：一句话说清这一节**不做**什么
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr(
                        "配对 / 连接 / 提交打印作业都不在本版范围（需 BlueZ / CUPS 一整套）",
                        "Pairing, connecting and submitting print jobs are out of scope (they need BlueZ / CUPS)",
                    ),
                    font::CAPTION, color::text_dim(), 0.75,
                );
                // 这里是本页最后一行，不再需要推进 ry（推了也没人读）
            }
        }
        "显示输出" => {
            // §3.8：**只读报告**。这里要说清两件不同的事：
            // ① 系统在绘制哪个输出；② 息屏会作用到哪些输出。
            // 笼统写成"支持多显示器"就是骗人。
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("检测到的输出", "Detected outputs"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            let r_refresh = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H_SM) };
            widgets::row(
                buf, w, h, r_refresh,
                crate::i18n::tr("重新读取", "Refresh"),
                None,
                widgets::States::at(r_refresh, mouse), Some(tr),
            );
            hits.push((r_refresh, SettingsHit::RefreshDisplays));
            ry += theme::size::px(theme::size::GAP_WIDE);

            if !displays_loaded {
                // 区分「还没读」与「读了确实没有」—— 这两件事在界面上不该长得一样
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("正在读取…", "Loading…"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            } else if displays.is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr(
                        "没有读到显示输出（这台机器上 /sys/class/graphics 里没有 fb*）",
                        "No display output found (no fb* under /sys/class/graphics)",
                    ),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            }
            let mut listed: Vec<(String, String)> = Vec::new();
            for o in displays.iter() {
                // 属性读不到就如实写 `?`，不替它编一个分辨率
                let res = if o.xres > 0 && o.yres > 0 {
                    format!("{}x{}", o.xres, o.yres)
                } else {
                    "?x?".to_string()
                };
                let bpp = if o.bpp > 0 { format!("{}", o.bpp) } else { "?".to_string() };
                let state = if o.index == driven_index {
                    crate::i18n::pick(lang, "系统正在绘制这个输出", "this is the output being drawn")
                } else {
                    crate::i18n::pick(lang, "未被绘制（只参与息屏）", "not drawn (blanking only)")
                };
                let name = if o.name.is_empty() {
                    crate::i18n::pick(lang, "（驱动没给名字）", "(driver gave no name)").to_string()
                } else {
                    o.name.clone()
                };
                listed.push((format!("fb{} · {name}", o.index), format!("{res}@{bpp} · {state}")));
            }
            for (title, hint) in &listed {
                if ry > content.y + content.h - 60 {
                    break;
                }
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                widgets::row(buf, w, h, row, title, Some(hint), widgets::States::at(row, mouse), Some(tr));
                ry += theme::size::px(theme::size::ROW_TALL);
            }

            ry += theme::size::px(theme::size::GAP_TIGHT);
            draw_wrapped(
                tr,
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                crate::i18n::tr(
                    "本系统只绘制主输出；多显示器需要布局与坐标体系改造（见缺口清单 §3.8）",
                    "Only the primary output is drawn; multi-monitor needs a layout/coordinate rework",
                ),
                font::CAPTION, color::text_dim(), 0.75,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
        }
        "系统更新" => {
            // §3.1：**离线更新包**。界面只负责"列出来 + 点一下"，
            // 版本比较、哈希校验、白名单全在 aetherd —— 界面不猜。
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("系统更新", "System update"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                &update.status_line(lang),
                font::CAPTION, color::text_dim(), 0.9,
            );
            ry += theme::size::px(theme::size::LINE_GAP);
            draw_wrapped(
                tr,
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                crate::i18n::tr(
                    crate::i18n::pick(lang, "把更新包放进 /var/lib/aether/update/incoming/ 后点刷新；暂存后重启生效", "Drop a bundle into /var/lib/aether/update/incoming/ and refresh; it applies on reboot"),
                    "Drop a bundle into /var/lib/aether/update/incoming/ then refresh; reboot to apply",
                ),
                font::CAPTION, color::text_dim(), 0.75,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            ry += theme::size::px(theme::size::HINT_GAP);

            let r_refresh = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H_SM) };
            widgets::row(
                buf, w, h, r_refresh,
                crate::i18n::tr("刷新列表", "Refresh list"),
                None,
                widgets::States::at(r_refresh, mouse), Some(tr),
            );
            hits.push((r_refresh, SettingsHit::RefreshUpdates));
            ry += theme::size::px(theme::size::GAP_WIDE);

            if let Some(err) = update.error() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    &format!("{}：{err}", crate::i18n::tr("有问题", "Problem")),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            } else if let Some(msg) = update.message() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    msg,
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            }

            if update.is_loaded() && update.packages().is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("投递目录里没有更新包", "No update bundle in the drop folder"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
            }
            let names: Vec<String> = update.packages().to_vec();
            for (i, name) in names.iter().enumerate() {
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                let hint = update.row_hint(i, lang);
                widgets::row(buf, w, h, row, name, Some(&hint), widgets::States::at(row, mouse), Some(tr));
                // 暂存中一律不登记命中区：**不给"点了没反应"的机会**
                if update.can_click() {
                    hits.push((row, SettingsHit::StageUpdate(i)));
                }
                ry += theme::size::px(theme::size::ROW_TALL);
                if ry > content.y + content.h - 40 {
                    break;
                }
            }
        }
        "快照与回滚" => {
            // §5.6：**配置**快照（不是盘镜像）。文案必须这么说，
            // 否则用户会以为"回滚能救回我删掉的照片"。
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("配置快照", "Config snapshots"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            let tip = crate::i18n::tr(
                    crate::i18n::pick(lang, "只包含 /etc 与 /var/lib/aether 下的几个配置文件；不含应用与用户数据。回滚需点两次确认。", "Contains only a few config files under /etc and /var/lib/aether; no apps or user data. Rolling back needs two clicks."),
                    "Covers a few config files only; no apps or user data. Rollback needs two clicks.",
                );
            draw_wrapped(
                tr,
                buf,
                w,
                h,
                (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                &mut ry,
                &tip,
                font::CAPTION,
                color::text_dim(),
                0.85,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            ry += theme::size::px(theme::size::HINT_GAP);

            if let Some(err) = snapshots.error() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    &format!("{}：{err}", crate::i18n::tr("读取失败", "Failed")),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            } else if !snapshots.is_loaded() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("正在读取…", "Loading…"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            }

            // 创建
            let r_new = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf, w, h, r_new,
                crate::i18n::tr("创建快照", "Create snapshot"),
                Some(crate::i18n::tr("把当前配置存一份", "Save the current configuration")),
                widgets::States::at(r_new, mouse), Some(tr),
            );
            hits.push((r_new, SettingsHit::CreateSnapshot));
            // 步长跟着**缩放后的高度**走：写死的话 150% 下高度变大、步长不变，
            // 下一个元素会落在这一行里面（静态扫描 `scripts/scale-audit.py` 发现，附录 CO）。
            ry += theme::size::px(theme::size::ROW_H) + 10;

            if snapshots.is_loaded() && snapshots.is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("还没有快照", "No snapshots yet"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::GROUP_GAP);
            }
            let rows: Vec<(String, String)> = (0..snapshots.items().len())
                .map(|i| (snapshots.items()[i].id.clone(), snapshots.row_hint(i, lang)))
                .collect();
            for (i, (id, hint)) in rows.iter().enumerate() {
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                let selected = snapshots.confirming() == Some(id.as_str());
                widgets::row(
                    buf, w, h, row, id, Some(hint),
                    widgets::States { selected, ..widgets::States::at(row, mouse) },
                    Some(tr),
                );
                hits.push((row, SettingsHit::RestoreSnapshot(i)));
                ry += theme::size::px(theme::size::ROW_TALL);
                if ry > content.y + content.h - 60 {
                    break;
                }
            }
            // 待确认时给一条"取消"（两步确认必须能退出来）
            if snapshots.confirming().is_some() {
                let r_cancel = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H_SM) };
                widgets::row(
                    buf, w, h, r_cancel,
                    crate::i18n::tr("取消回滚", "Cancel rollback"),
                    Some(crate::i18n::tr("保留当前配置", "Keep the current configuration")),
                    widgets::States::at(r_cancel, mouse), Some(tr),
                );
                hits.push((r_cancel, SettingsHit::CancelRestore));
            }
        }
        "工具权限" => {
            // §5.3：逐工具策略。内容来自传入的 `policy` 视图（由事件循环异步取回），
            // **绘制不自己发 IPC** —— 那样每帧都会发一次请求。
            use crate::policyview as pv;
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("每个工具一行", "One row per tool"), Some(tr),
            );
            ry += theme::size::px(theme::size::LINE_GAP);
            let tip = crate::i18n::tr(
                    crate::i18n::pick(lang, "点击一行循环切换：按等级 → 每次询问 → 允许 → 禁止。禁止优先于一切；L3 永不免确认。", "Click a row to cycle: by level → ask every time → allow → deny. Deny beats everything; L3 never skips confirmation."),
                    "Click a row to cycle: By level → Always ask → Allow → Deny. Deny wins over everything; L3 always confirms.",
                );
            draw_wrapped(
                tr,
                buf,
                w,
                h,
                (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                &mut ry,
                &tip,
                font::CAPTION,
                color::text_dim(),
                0.85,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            ry += theme::size::px(28);

            // 三种"没有行"的状态必须**分开说**：取不到 ≠ 一个工具都没有 ≠ 还没取
            if let Some(err) = policy.error() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    &format!("{}：{err}", crate::i18n::tr("读取失败", "Failed to load")),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            } else if !policy.is_loaded() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("正在读取…", "Loading…"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            } else if policy.is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("没有可配置的工具", "No tools to configure"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            }

            let rows = policy.entries().to_vec();
            for (i, e) in rows.iter().enumerate() {
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                let hint = policy.row_hint(e, lang);
                widgets::row(
                    buf, w, h, row, &e.tool, Some(&hint),
                    widgets::States::at(row, mouse), Some(tr),
                );
                hits.push((row, SettingsHit::CycleToolPolicy(i)));
                ry += theme::size::px(theme::size::ROW_TALL);
                // 列表超出可视区就停下（滚动不是这批的事，但画到框外更糟）
                if ry > content.y + content.h - 60 {
                    break;
                }
            }
            let _ = pv::level_label(0); // 引用一下模块，避免"未使用"（它在 row_hint 里用到）

            ry += 6;
            let act = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf, w, h, act,
                crate::i18n::tr("重新读取", "Reload"),
                Some(crate::i18n::tr(
                    crate::i18n::pick(lang, "从 aetherd 取一次最新的策略", "Fetch the latest policy from aetherd once"),
                    "Fetch the current policy from aetherd",
                )),
                widgets::States::at(act, mouse), Some(tr),
            );
            hits.push((act, SettingsHit::RefreshPolicy));
        }
        "快捷键" => {
            // §5.9：总览页。内容**只来自 `shortcuts::SHORTCUTS`** ——
            // 那张表的每条都带探针测试，所以这里画出来的每一条在源码里都有落点。
            // 不在绘制里另写一份文字：两份必然分叉，而分叉的那份就是"界面上写着但代码里没有"。
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("全部快捷键", "All shortcuts"), Some(tr),
            );
            ry += theme::size::px(theme::size::HINT_GAP);
            for sc in crate::shortcuts::SHORTCUTS {
                let row = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                // 标题 = 键位，副标题 = 说明（两种语言都有，且有测试保证非空）
                widgets::row(
                    buf, w, h, row, sc.keys, Some(sc.desc_in(lang)),
                    widgets::States::at(row, mouse), Some(tr),
                );
                ry += theme::size::px(theme::size::ROW_TALL);
            }
            ry += 6;
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                crate::i18n::tr(
                    crate::i18n::pick(lang, "以上每条都在源码里有落点（有测试逐条核对）", "Every entry above has a landing point in the source (checked one by one by tests)"),
                    "Every entry above is verified against the source by a test",
                ),
                font::CAPTION, color::text_dim(), 0.8,
            );
            // 注意：分支末尾不要再写 `ry += theme::size::px(theme::size::LINE_GAP);` —— 没有后续行读它，会被判为死赋值
            // （门禁把警告当失败）。
        }
        "网络" => {
            // §3.5：本页 chrome 双语。**注意**：DNS 校验失败的原因来自协议层
            // （`parse_dns_list` 返回中文句子），那一处仍会是中文 —— 见附录 AA 的计划，
            // 正确修法是协议层返回枚举、界面映射文案。不在这里假装已双语。
            let lang = crate::i18n::lang();
            // 真值来自内核（`/sys/class/net/*`、`/proc/net/route`、`/proc/net/fib_trie`、
            // `/etc/resolv.conf`）—— 解析逻辑与单测见 `net.rs`。
            //
            // **只在打开这一页时读**（几个小文件，µs 级），不做缓存、更不伪造：
            // 读不到就写"未发现网络接口"，绝不写"已连接"。
            let n = crate::net::read();
            widgets::group_header(
                buf,
                w,
                h,
                content.x + theme::size::px(theme::size::CONTENT_PAD),
                ry,
                crate::i18n::pick(lang, "状态", "Status"),
                Some(tr),
            );
            ry += theme::size::px(theme::size::LINE_GAP);
            tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, &n.summary(), font::BODY, color::text(), 0.92);
            // 这里必须用**内容行距**，不能借用侧栏的 `row_h` —— 侧栏行高是按窗口高度
            // 摊出来的（clamp 20..30）且**不随缩放变化**，150% 下会让状态行压住第一行（附录 DG）。
            // 与下面各行（4416 行）用同一个节奏常量。
            ry += theme::size::px(theme::size::LINE_GAP);
            let (name, mac, gw, up) = match &n.iface {
                Some(i) => (i.name.clone(), i.mac.clone(), i.gateway.clone(), i.up),
                None => (
                    crate::i18n::pick(lang, "（无）", "(none)").to_string(),
                    String::new(),
                    String::new(),
                    false,
                ),
            };
            let dns = if n.dns.is_empty() {
                crate::i18n::pick(lang, "（未配置）", "(not configured)").to_string()
            } else {
                n.dns.join("   ")
            };
            for (k, v) in [
                (crate::i18n::pick(lang, "接口", "Interface"), name),
                (
                    "MAC",
                    if mac.is_empty() {
                        crate::i18n::pick(lang, "（无）", "(none)").to_string()
                    } else {
                        mac
                    },
                ),
                (
                    crate::i18n::pick(lang, "链路", "Link"),
                    if up {
                        crate::i18n::pick(lang, "已连接", "connected").to_string()
                    } else {
                        crate::i18n::pick(lang, "断开", "down").to_string()
                    },
                ),
                (
                    crate::i18n::pick(lang, "网关", "Gateway"),
                    if gw.is_empty() {
                        crate::i18n::pick(lang, "（无）", "(none)").to_string()
                    } else {
                        gw
                    },
                ),
                ("DNS", dns),
            ] {
                tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 110) as f32, ry as f32, &v, font::CAPTION, color::text_dim(), 0.9);
                ry += theme::size::px(theme::size::LINE_GAP);
            }
            ry += 10;
            // 「修改 DNS」（§2.4）：唯一一个**能改系统配置**的设置项。
            //
            // 交互分两态，但**用同一个命中区**（点它就是进编辑态，已在编辑态时重点无害）：
            // 编辑态显示输入内容 + 光标 + 校验反馈，非编辑态显示当前值 + 提示。
            // 校验反馈**不需要等 aetherd**（用的是共享的纯函数校验器），所以打字时就有反应。
            let dns_act = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            if dns_field.editing {
                // 光标用「▏」表示（字体里没有独立的插入符绘制原语，用一个细竖线字符最省事）
                let shown = format!("{}▏", dns_field.text);
                let hint = dns_field
                    .feedback
                    .clone()
                    .unwrap_or_else(|| {
                        crate::i18n::pick(lang, "回车保存 · Esc 取消", "Enter to save · Esc to cancel")
                            .to_string()
                    });
                widgets::row(buf, w, h, dns_act, &shown, Some(&hint),
                             widgets::States::at(dns_act, mouse), Some(tr));
            } else {
                let hint = dns_field
                    .feedback
                    .clone()
                    .unwrap_or_else(|| {
                        crate::i18n::pick(
                            lang,
                            "点击后输入，例如 1.1.1.1, 8.8.8.8",
                            "Click to type, e.g. 1.1.1.1, 8.8.8.8",
                        )
                        .to_string()
                    });
                let label = if dns_field.text.is_empty() {
                crate::i18n::pick(lang, "修改 DNS", "Change DNS").to_string()
            } else if crate::i18n::is_en() {
                format!("Change DNS ({})", dns_field.text)
            } else {
                format!("修改 DNS（{}）", dns_field.text)
            };
                widgets::row(buf, w, h, dns_act, &label, Some(&hint),
                             widgets::States::at(dns_act, mouse), Some(tr));
            }
            hits.push((dns_act, SettingsHit::EditDns));
            // 步长跟着按钮高度走：写死 50 的话 150% 下按钮变高、步长不变，两行会压住
            // （几何断言发现，附录 CM）。`px(CTRL_H) + 6` 在 100% 下与 50 完全等价。
            ry += theme::size::px(theme::size::CTRL_H) + 6;

            // 唯一一个"重启服务"的动作（P4.4）：走 aetherd → init，不是本地假动作
            let act = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                        widgets::row(
                buf,
                w,
                h,
                act,
                crate::i18n::pick(lang, "重启网络服务", "Restart network service"),
                Some(crate::i18n::pick(
                    lang,
                    "断开后重新连接（经 aetherd 转交 init）",
                    "Disconnect and reconnect (handed to init via aetherd)",
                )),
                         widgets::States::at(act, mouse), Some(tr));
            hits.push((act, SettingsHit::RestartService("network")));
            // 步长跟着按钮高度走：写死 50 的话 150% 下按钮变高、步长不变，两行会压住
            // （几何断言发现，附录 CM）。`px(CTRL_H) + 6` 在 100% 下与 50 完全等价。
            ry += theme::size::px(theme::size::CTRL_H) + 6;
            tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::pick(
                    lang,
                    "配置项本身仍是只读；改动类操作一律经 aetherd 执行。",
                    "The config itself stays read-only; every change goes through aetherd.",
                ),
                    font::CAPTION, color::text_dim(), 0.75);
            if !n.note.is_empty() {
                ry += theme::size::px(theme::size::GAP_STD);
                tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, &n.note, font::CAPTION, color::text_dim(), 0.75);
            }
        }
        "应用" => {
            // §3.5：本页文案双语（页面 chrome）
            let lang = crate::i18n::lang();
            // 2026-10-01：把「应用」页从"待接入"变成**真能用的页**。
            //
            // 这里只列**内建应用**（每个都能真实打开）—— 不是偷懒：
            // 已装应用的列表要从 `renderer.installed_apps` 取，而本函数是被
            // `draw_window`（19 参自由函数）调用的，穿一个列表进去要动它的签名与调用链。
            // 与其塞一个空列表进去装样子，不如**如实说明已装应用在 Dock 上**。
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "内建应用", "Built-in apps"), Some(tr));
            ry += theme::size::px(theme::size::LINE_GAP);
            for (i, name, desc) in [
                (0usize, crate::i18n::pick(lang, "文件管理", "Files"), crate::i18n::pick(lang, "浏览文件与应用目录", "Browse files and app folders")),
                (1, crate::i18n::pick(lang, "终端", "Terminal"), crate::i18n::pick(lang, "真实 PTY，可运行市面软件", "A real PTY — runs off-the-shelf software")),
                (2, crate::i18n::pick(lang, "浏览器", "Browser"), crate::i18n::pick(lang, "复用文件管理器的纸面渲染", "Reuses the file manager's paper-like rendering")),
                (3, crate::i18n::pick(lang, "音乐", "Music"), crate::i18n::pick(lang, "本地曲库", "Local library")),
                (4, crate::i18n::pick(lang, "系统设置", "Settings"), crate::i18n::pick(lang, "即本窗口", "This very window")),
            ] {
                let r = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                let st = widgets::States::at(r, mouse);
                widgets::row(buf, w, h, r, name, Some(desc), st, Some(tr));
                // 点击 → 打开（事件循环执行；见 main.rs 的设置命中分支）
                hits.push((r, SettingsHit::OpenApp(i)));
                ry += theme::size::px(theme::size::GAP_WIDE);
            }
            ry += theme::size::px(theme::size::GAP_TIGHT);
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "已装应用", "Installed apps"), Some(tr));
            ry += theme::size::px(theme::size::GROUP_GAP);
            tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::pick(lang, "从宿主安装的程序显示在 Dock 上（数据来自 /var/apps）。", "Programs installed from the host show up in the Dock (data from /var/apps)."),
                    font::CAPTION, color::text_dim(), 0.85);
            ry += theme::size::px(theme::size::GAP_STD);
            draw_wrapped(tr, buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                    crate::i18n::pick(lang, "在设置里列出/卸载它们需要 IPC 能力（aetherd 代理），下一步接入。", "Listing/uninstalling them from Settings needs IPC capability (aetherd proxy); coming next."),
                    font::CAPTION, color::text_dim(), 0.75, (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,);
        }
        "外观" => {
            // §3.5：本页文案双语（页面 chrome）。注意"界面语言"那条提示里
            // 写着"正文尚未翻译"——等 §3.5 做完要一起改掉（别留一句过期的自述）。
            let lang = crate::i18n::lang();
            // 主题开关：**默认明亮（纯白）**，切换立即生效 —— 改全局 MODE 并重建背景缓存，
            // 由事件循环执行（绘制期不改状态）。
            let r1 = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::CTRL_H) };
            toggle_row(buf, w, h, mouse, tr, r1, crate::i18n::pick(lang, "深色模式", "Dark mode"), Some(crate::i18n::pick(lang, "默认关闭（明亮/纯白）；切换立即生效", "Off by default (bright / pure white); takes effect immediately")), s.dark_mode, SettingsHit::ToggleDark, hits);
            // §3.4 无障碍：高对比度。紧跟主题开关 —— 它是"看得清"这条线上的第二项。
            // 说明文字直接给出**可验证的依据**（WCAG AA），而不是"更清楚"这种主观话。
            let r_hc = Rect { x: r1.x, y: r1.y + theme::size::px(56), w: r1.w, h: r1.h };
            // §3.5 语言：**用该语言自己写语言名**（英文界面里出现"英语"两个字是半成品的样子）。
            // 开关语义 = "是否英文"，`Locale` 的值由当前状态算出。
            // §3.4 字号：界面里"存了不用"的又一个 —— 本批接上（缩放做在文本渲染器里）
            let r_scale = Rect { x: r1.x, y: r1.y + theme::size::px(154), w: r1.w, h: r1.h };
            let scale_hint = crate::i18n::tr(
                "只缩放文字；行高与边距仍是固定值（150% 会偏挤，见缺口清单 §3.4）",
                "Scales text only; row heights stay fixed (150% is tight)",
            );
            widgets::row(
                buf, w, h, r_scale,
                &format!(
                    "{}：{}%",
                    crate::i18n::tr("界面字号", "Text size"),
                    s.ui_scale_pct
                ),
                Some(scale_hint),
                widgets::States::at(r_scale, mouse), Some(tr),
            );
            hits.push((r_scale, SettingsHit::CycleUiScale));
            let r_lang = Rect { x: r1.x, y: r1.y + theme::size::px(112), w: r1.w, h: r1.h };
            let lang = crate::i18n::lang();
            let lang_title = crate::i18n::tr("界面语言", "Language");
            // 提示里带上**当前语言名**（用该语言自身拼写）——
            // 这比只给一个开关更能让人确认"我现在看的是哪种语言"。
            let lang_hint = format!(
                "{} · {}",
                lang.label(),
                crate::i18n::tr(
                    "切换后左栏与页标题立即生效；**部分页面的正文仍在翻译中**（见缺口清单 §3.5）",
                    "Sidebar and titles switch immediately; some page bodies are still being translated",
                )
            );
            let next_locale = if s.locale == 1 { 0 } else { 1 };
            toggle_row(buf, w, h, mouse, tr, r_lang, lang_title, Some(lang.label()),
                       s.locale == 1, SettingsHit::Locale(next_locale), hits);
            let _ = lang;

            let hc_hint = crate::i18n::pick(lang, "把正文与背景的对比度提到 WCAG AA（4.5:1）以上；与主题正交", "Raises text/background contrast above WCAG AA (4.5:1); orthogonal to the theme");
            toggle_row(buf, w, h, mouse, tr, r_hc, crate::i18n::pick(lang, "高对比度", "High contrast"),
                       Some(crate::i18n::pick(lang, "WCAG AA 4.5:1", "WCAG AA 4.5:1")),
                       s.high_contrast, SettingsHit::ToggleContrast, hits);
            // 第一组四行是用 `r1.y + 偏移` 定位的 —— `ry` 必须**手动推到它们下面**，
            // 否则后面的「当前配色 / 壁纸 / 语言 / 息屏」几节会画在这四行身上。
            // （由 `scripts/layout-check.py` 的几何断言发现：两个不同控件重叠 26 像素。）
            ry = r1.y + 154 + r1.h;
            // 长说明改用**下面单独一行**（行内放不下；改行高会动到那四行的固定偏移布局 ——
            // 附录 DT 试过三次都改坏，所以这里不动布局，改用页面已有的 ry 流水）。
            ry += theme::size::px(theme::size::GAP_TIGHT);
            draw_wrapped(
                tr, buf, w, h,
                (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                &mut ry,
                &lang_hint,
                font::CAPTION, color::text_dim(), 0.85,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            draw_wrapped(
                tr, buf, w, h,
                (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                &mut ry,
                hc_hint,
                font::CAPTION, color::text_dim(), 0.85,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            ry += theme::size::px(theme::size::GAP_HUGE);
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "当前配色", "Current colors"), Some(tr));
            ry += theme::size::px(theme::size::GROUP_GAP);
            // 壁纸色板（批次 L-1）：**真的能换** —— 点一下立即生效并落盘。
            // 色板用壁纸自身的配色画，所见即所得（不做"点了才知道什么颜色"的控件）。
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "壁纸", "Wallpaper"), Some(tr));
            ry += theme::size::px(theme::size::LINE_GAP);
            for i in 0..crate::settings::WALLPAPER_COUNT {
                let sw = Rect { x: content.x + theme::size::px(theme::size::CONTENT_PAD) + (i as i32) * 46, y: ry, w: 38, h: theme::size::px(theme::size::CTRL_H_SM) };
                let (top, bottom) = wallpaper_colors(i, s.dark_mode);
                // 竖直两段近似渐变：色板只是"这个壁纸长什么样"的缩略，不必逐像素
                let half = sw.h / 2;
                rounded_rect(buf, w, h, Rect { x: sw.x, y: sw.y, w: sw.w, h: half }, 3.0,
                             top, 1.0);
                rounded_rect(buf, w, h, Rect { x: sw.x, y: sw.y + half, w: sw.w, h: sw.h - half }, 3.0,
                             bottom, 1.0);
                // 当前选中：外框高亮（不是"更亮一点"这种看不清的差别）
                if i == s.wallpaper {
                    rounded_outline(buf, w, h, sw, 3.0, color::accent(), 0.95);
                } else {
                    rounded_outline(buf, w, h, sw, 3.0, color::hairline(), 0.35);
                }
                hits.push((sw, SettingsHit::Wallpaper(i)));
            }
            ry += theme::size::px(theme::size::GAP_WIDE);
            // 息屏档位（§3.6）：固定档位按钮，当前档高亮。
            // 0 = 永不 —— 演示与排障时需要它，所以它必须是一个**能选中的档**，
            // 而不是"设了 0 却显示成 5 分钟"这种含糊状态。
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "空闲息屏", "Idle blanking"), Some(tr));
            ry += theme::size::px(theme::size::LINE_GAP);
            // 档位按钮**按可用宽度**排 —— 不写死 84 像素步长。
            // 写死的话 4 档 × 84 = 336 会超出内容宽度（约 310），右边那档直接压出窗口
            // （由 `scripts/layout-check.py` 的几何断言发现，见审计附录 CL）。
            let n_opts = crate::settings::IDLE_BLANK_OPTIONS.len() as i32;
            let opt_gap = theme::size::px(theme::size::GAP_TIGHT);
            let opt_avail = row_w + 12; // 与同页其它行一致的内容宽度
            let opt_w = (opt_avail - opt_gap * (n_opts - 1)) / n_opts;
            for (i, min) in crate::settings::IDLE_BLANK_OPTIONS.iter().enumerate() {
                // 带参数 → 按语言分支（`format!` 的模板必须是字面量）
                let label = if *min == 0 {
                    crate::i18n::pick(lang, "永不", "Never").to_string()
                } else if crate::i18n::is_en() {
                    format!("{min} min")
                } else {
                    format!("{min} 分钟")
                };
                let b = Rect {
                    x: content.x + theme::size::px(theme::size::CONTROL_INSET)
                        + (i as i32) * (opt_w + opt_gap),
                    y: ry,
                    w: opt_w,
                    h: theme::size::px(theme::size::CTRL_H_SM),
                };
                let on = s.idle_blank_min == *min;
                widgets::row(buf, w, h, b, &label, None, widgets::States::at(b, mouse), Some(tr));
                if on {
                    rounded_outline(buf, w, h, b, 4.0, color::accent(), 0.95);
                }
                hits.push((b, SettingsHit::IdleBlank(*min)));
            }
            ry += theme::size::px(theme::size::ROW_TALL);
            // 性能 HUD（§5.10）：**默认关**。说明里写清"显示的是真实数字"，
            // 因为本项目对假数据零容忍 —— 用户有权知道这块面板值不值得信。
            let hud = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::CTRL_H) };
            let hud_hint = crate::i18n::pick(lang, "右下角显示真实帧时间与内存占用（排障用，默认关）", "Shows real frame time and memory in the bottom-right (for troubleshooting, off by default)");
            toggle_row(buf, w, h, mouse, tr, hud, crate::i18n::pick(lang, "性能 HUD", "Performance HUD"),
                       Some(crate::i18n::pick(lang, "右下角 · 排障用", "Troubleshooting")),
                       s.perf_hud, SettingsHit::TogglePerfHud, hits);
            ry = hud.y + hud.h + theme::size::px(theme::size::GAP_TIGHT);
            draw_wrapped(
                tr, buf, w, h,
                (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                &mut ry,
                hud_hint,
                font::CAPTION, color::text_dim(), 0.85,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            ry += theme::size::px(theme::size::GAP_HUGE);
            // 恢复默认（批次 L-1）：用户改坏了能回去。**真的重置**，不是只改显示。
            let reset = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(buf, w, h, reset, crate::i18n::pick(lang, "恢复默认外观", "Restore default appearance"),
                         Some(crate::i18n::pick(lang, "主题/壁纸/时钟/输入等恢复出厂值（时区保留）", "Resets theme / wallpaper / clock / input to factory values (time zone kept)")),
                         widgets::States::at(reset, mouse), Some(tr));
            hits.push((reset, SettingsHit::ResetDefaults));
            // 步长跟着**缩放后的高度**走：写死的话 150% 下高度变大、步长不变，
            // 下一个元素会落在这一行里面（静态扫描 `scripts/scale-audit.py` 发现，附录 CO）。
            ry += theme::size::px(theme::size::CTRL_H) + 8;
            for (k, v) in [
                (crate::i18n::pick(lang, "主色", "Accent"), crate::i18n::pick(lang, "teal（单一强调色，AI 元素同色）", "teal (single accent, same as AI elements)")),
                (crate::i18n::pick(lang, "圆角", "Radius"), "4 / 6 / 8"),
                (crate::i18n::pick(lang, "层次", "Depth"), crate::i18n::pick(lang, "两层轻影 + 1px 描边", "two soft shadows + 1px outline")),
            ] {
                tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 120) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += theme::size::px(theme::size::LINE_GAP);
            }
        }
        "时钟与时区" => {
            // §3.5：本页文案双语
            let lang = crate::i18n::lang();
            // 实时预览：改任何一项都能立刻看到结果（不用重启、不用"应用"按钮）
            let now = crate::text::now_utc_secs();
            let preview = crate::text::clock_fmt(now, s.tz_offset_min, s.clock_24h, s.clock_seconds);
            tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, &preview, font::HERO, color::text(), 0.98);
            let tz = if crate::i18n::is_en() {
                format!("{}　·　steps of 30 minutes", s.tz_label())
            } else {
                format!("{}　·　每次微调 30 分钟", s.tz_label())
            };
            tr.draw(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, (ry + 42) as f32, &tz, font::CAPTION, color::text_dim(), 0.9);
            ry += theme::size::px(78);

            let r1 = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::CTRL_H) };
            toggle_row(buf, w, h, mouse, tr, r1, crate::i18n::pick(lang, "24 小时制", "24-hour clock"), Some(crate::i18n::pick(lang, "关闭后显示为「下午 9:10」", "Off shows 9:10 PM")), s.clock_24h, SettingsHit::Clock24h, hits);
            // 步长跟着**缩放后的高度**走：写死的话 150% 下高度变大、步长不变，
            // 下一个元素会落在这一行里面（静态扫描 `scripts/scale-audit.py` 发现，附录 CO）。
            ry += theme::size::px(theme::size::CTRL_H) + 4;
            let r2 = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::CTRL_H) };
            toggle_row(buf, w, h, mouse, tr, r2, crate::i18n::pick(lang, "显示秒", "Show seconds"), Some(crate::i18n::pick(lang, "顶栏时钟精确到秒", "Top-bar clock shows seconds")), s.clock_seconds, SettingsHit::ClockSeconds, hits);
            // 步长跟着**缩放后的高度**走：写死的话 150% 下高度变大、步长不变，
            // 下一个元素会落在这一行里面（静态扫描 `scripts/scale-audit.py` 发现，附录 CO）。
            ry += theme::size::px(theme::size::CTRL_H) + 10;

            // 时区微调：用两个按钮而不是滑杆 —— 时区是离散值、且要能精确到半小时
            let back = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry + 6, w: theme::size::px(theme::size::CTRL_H_SM) + 10, h: theme::size::px(theme::size::CTRL_H_SM) };
            // 第二个按钮的位置**从第一个派生**：写死步长的话，宽度一变就会压住
            // （150% 下实测重叠 4 像素，几何断言发现，附录 CN）。
            let fwd = Rect { x: back.x + back.w + theme::size::px(theme::size::GAP_TIGHT), y: back.y, w: back.w, h: back.h };
            widgets::button(buf, w, h, back, "-1", widgets::BtnKind::Secondary, widgets::States::at(back, mouse), Some(tr));
            widgets::button(buf, w, h, fwd, "+1", widgets::BtnKind::Secondary, widgets::States::at(fwd, mouse), Some(tr));
            tr.draw(buf, w, h, (content.x + 124) as f32, tr.vcenter(ry as f32, theme::size::px(42) as f32, font::CAPTION), crate::i18n::pick(lang, "时区（半小时制）", "Time zone (30-min steps)"), font::CAPTION, color::text_dim(), 0.9);
            hits.push((back, SettingsHit::TzShift(-30)));
            hits.push((fwd, SettingsHit::TzShift(30)));

            // ---- §3.3 对时与系统时区 ----
            // 这里要分清两件事，否则用户会以为"我已经改好了"：
            //   上面那行是**系统当前**时区，下面那行是**待写入**的候选名。
            ry += theme::size::px(theme::size::GAP_TIGHT);
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("时间同步", "Time sync"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            let sync_line = time.sync_line(crate::i18n::lang());
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, &sync_line,
                font::CAPTION, color::text_dim(), 0.9,
            );
            ry += theme::size::px(theme::size::HINT_GAP);
            let r_sync = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf, w, h, r_sync,
                crate::i18n::tr("立即对时", "Sync now"),
                Some(crate::i18n::tr("向网络时间服务器查询一次（小幅平滑、大幅跳变）", "Query the network time server once")),
                widgets::States::at(r_sync, mouse), Some(tr),
            );
            hits.push((r_sync, SettingsHit::SyncTimeNow));
            ry += theme::size::px(theme::size::ROW_TALL);

            let sys_line = if time.system_tz.is_empty() {
                crate::i18n::tr("系统时区：未知（读不到 /etc/timezone）", "System time zone: unknown").to_string()
            } else {
                format!("{}：{}", crate::i18n::tr("系统时区", "System time zone"), time.system_tz)
            };
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, &sys_line,
                font::CAPTION, color::text_dim(), 0.9,
            );
            ry += theme::size::px(theme::size::LINE_GAP);
            let r_pick = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf, w, h, r_pick,
                &format!("{}：{}", crate::i18n::tr("待写入时区", "Time zone to write"), time.picked_name()),
                Some(crate::i18n::tr("点击切换候选（UTC / 上海 / 东京 / 新加坡 / 伦敦 / 巴黎 / 纽约 / 洛杉矶）",
                                     "Click to cycle candidates")),
                widgets::States::at(r_pick, mouse), Some(tr),
            );
            hits.push((r_pick, SettingsHit::CycleTimezoneName));
            ry += theme::size::px(theme::size::ROW_TALL);
            let r_write = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf, w, h, r_write,
                crate::i18n::tr("写入系统时区", "Write system time zone"),
                Some(if time.already_system_timezone() {
                    crate::i18n::tr("与系统当前一致（写入也是安全的）", "Already matches the system")
                } else {
                    crate::i18n::tr("写 /etc/localtime 与 /etc/timezone", "Writes /etc/localtime and /etc/timezone")
                }),
                widgets::States::at(r_write, mouse), Some(tr),
            );
            hits.push((r_write, SettingsHit::SetTimezoneNow));
        }
        "输入" => {
            // §3.5：本页文案双语
            let lang = crate::i18n::lang();
            let r1 = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::CTRL_H) };
            toggle_row(buf, w, h, mouse, tr, r1, crate::i18n::pick(lang, "默认启用中文输入法", "Chinese IME on by default"), Some(crate::i18n::pick(lang, "开机即生效，Ctrl+空格 可随时切换", "Active from boot; Ctrl+Space toggles it")), s.ime_default, SettingsHit::ImeDefault, hits);
            ry += theme::size::px(theme::size::GAP_HUGE);
            // 鼠标速度：合成器原先对相对位移**硬编码 ×2**，用户反馈在 VMware/真机上太快。
            // 默认 100（1:1），这里给三档；数值走 `SettingsHit::MouseSpeed`（真实写盘）。
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "鼠标速度", "Mouse speed"), Some(tr));
            ry += theme::size::px(theme::size::LINE_GAP);
            // 与同页其它行**同一起点与宽度** —— 原来写死 `w: 300` 且起点在 +24，
            // 而可用宽度只有约 295，右边那一段会压出窗口（几何断言发现，附录 CL）。
            let seg = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::CTRL_H_SM) };
            let active = match s.mouse_speed_pct {
                p if p < 80 => 0,
                p if p > 120 => 2,
                _ => 1,
            };
            // 段控件的三个标签也要跟着语言（数组不是 `format!`，可以直接分支）
            let speed_labels = if crate::i18n::is_en() {
                ["Slow", "Normal", "Fast"]
            } else {
                ["慢", "标准", "快"]
            };
            let rects = widgets::segmented(buf, w, h, seg, &speed_labels, active, mouse, Some(tr));
            for (i, rr) in rects.iter().enumerate() {
                hits.push((*rr, SettingsHit::MouseSpeed([60, 100, 150][i])));
            }
            tr.draw(
                buf, w, h,
                (content.x + 340) as f32,
                (ry + 7) as f32,
                &format!("{}%", s.mouse_speed_pct),
                font::CAPTION, color::text_dim(), 0.85,
            );
            ry += theme::size::px(theme::size::ROW_TALL);
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "键盘快捷键", "Keyboard shortcuts"), Some(tr));
            ry += theme::size::px(theme::size::GROUP_GAP);
            for (k, v) in [
                ("Alt+1 … Alt+4", crate::i18n::pick(lang, "切换窗口布局", "Switch window layout")),
                ("Ctrl+Space", crate::i18n::pick(lang, "中英文切换", "Toggle Chinese/English IME")),
                ("Ctrl+Shift+C / V", crate::i18n::pick(lang, "复制 / 粘贴", "Copy / paste")),
                ("Enter", crate::i18n::pick(lang, "命令栏发送", "Send from the command bar")),
                ("Esc", crate::i18n::pick(lang, "关闭菜单或命令栏", "Close menu or command bar")),
            ] {
                tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 190) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += theme::size::px(theme::size::LINE_GAP);
            }
        }
        "AI" => {
            // §3.5：本页文案双语
            let lang = crate::i18n::lang();
            widgets::group_header(buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry, crate::i18n::pick(lang, "推理通道", "Inference pipeline"), Some(tr));
            ry += theme::size::px(theme::size::LINE_GAP);
            for (k, v) in [
                (crate::i18n::pick(lang, "aetherd", "aetherd"), crate::i18n::pick(lang, "本地服务（socket / TCP 7311）", "Local service (socket / TCP 7311)")),
                (crate::i18n::pick(lang, "离线意图", "Offline intent"), crate::i18n::pick(lang, "内置规则，断网可用", "Built-in rules, works offline")),
                (crate::i18n::pick(lang, "动作执行", "Action execution"), crate::i18n::pick(lang, "布局 / 开应用 / 关窗口", "Layout / open app / close window")),
                (crate::i18n::pick(lang, "权限闸门", "Permission gate"), crate::i18n::pick(lang, "危险动作需确认卡片", "Risky actions need a confirmation card")),
            ] {
                tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 140) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += theme::size::px(theme::size::LINE_GAP);
            }
            ry += 10;
            draw_wrapped(tr, buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry, crate::i18n::pick(lang, "模型与密钥由 aetherd 管理，不经过合成器进程。", "The model and its key are managed by aetherd; they never pass through the compositor process."), font::CAPTION, color::text_dim(), 0.8, (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,);
            ry += theme::size::px(28);
            // 第二个"能改系统"的动作（P4.4）：AI 卡住时把 aetherd 重启一次
            let ai_act = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(buf, w, h, ai_act, crate::i18n::pick(lang, "重启 AI 服务", "Restart AI service"), Some(crate::i18n::pick(lang, "aetherd 重启后由 init 拉起（会话会中断）", "init starts aetherd again after the restart (the session is interrupted)")),
                         widgets::States::at(ai_act, mouse), Some(tr));
            hits.push((ai_act, SettingsHit::RestartService("aetherd")));
        }
        "关于" => {
            // §3.5：本页文案双语
            let lang = crate::i18n::lang();
            tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, "AetherOS", font::TITLE, color::text(), 0.98);
            ry += theme::size::px(36);
            for (k, v) in [
                (crate::i18n::pick(lang, "版本", "Version"), env!("CARGO_PKG_VERSION")),
                (crate::i18n::pick(lang, "许可证", "License"), "GPL-3.0-only"),
                (crate::i18n::pick(lang, "架构", "Architecture"), std::env::consts::ARCH),
                (crate::i18n::pick(lang, "内核", "Kernel"), crate::i18n::pick(lang, "Linux（Buildroot 2024.02.1）", "Linux (Buildroot 2024.02.1)")),
                (crate::i18n::pick(lang, "设置文件", "Settings file"), crate::settings::SETTINGS_PATH),
            ] {
                tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, k, font::CAPTION, color::text(), 0.9);
                tr.draw(buf, w, h, (content.x + 130) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += theme::size::px(theme::size::LINE_GAP);
            }

            // 电源入口（2026-10-02 功能批次 J-2）。
            //
            // 放在「关于」页而不是新造一页：关机/重启/注销是**低频**动作，
            // 为它们单开一页会让左栏更长、更难找常用的那几页（本项目不做空壳页）。
            // 控制中心里也有同一组入口（那边是常用路径）。
            ry += 18;
            tr.draw_bold(buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32, crate::i18n::pick(lang, "电源", "Power"), font::CAPTION, color::text(), 0.9);
            ry += theme::size::px(theme::size::HINT_GAP);
            for (label, hint, hit) in [
                (crate::i18n::pick(lang, "关机", "Power off"), crate::i18n::pick(lang, "停止所有服务并断电（需确认）", "Stops all services and cuts power (needs confirmation)"), SettingsHit::Power(aether_ipc::PowerAction::PowerOff)),
                (crate::i18n::pick(lang, "重启", "Reboot"), crate::i18n::pick(lang, "停止所有服务并重新引导（需确认）", "Stops all services and reboots (needs confirmation)"), SettingsHit::Power(aether_ipc::PowerAction::Reboot)),
                (crate::i18n::pick(lang, "注销", "Log out"), crate::i18n::pick(lang, "重启桌面（窗口会关闭，由 aether-init 拉起）", "Restarts the desktop (windows close; aether-init starts it again)"), SettingsHit::Logout),
            ] {
                let act = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
                widgets::row(buf, w, h, act, label, Some(hint), widgets::States::at(act, mouse), Some(tr));
                hits.push((act, hit));
                // 步长必须跟着**按钮高度**走：写死的话 150% 下按钮变高、步长不变，
                // 三个按钮会互相压住（几何断言发现，附录 CM）。
                ry += theme::size::px(theme::size::CTRL_H) + 4;
            }
        }
        "账户" => {
            // §2.3：**只读列表 + 改口令**。
            // 增删用户按项目决定留 M3（缺口清单 §2.3 写了这条是决定，不是遗漏）。
            let lang = crate::i18n::lang();
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("账户", "Accounts"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            let r_reload = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H_SM) };
            widgets::row(
                buf, w, h, r_reload,
                crate::i18n::tr("重新读取", "Reload"),
                None,
                widgets::States::at(r_reload, mouse), Some(tr),
            );
            hits.push((r_reload, SettingsHit::RefreshUsers));
            // 步长跟着**缩放后的高度**走：写死的话 150% 下高度变大、步长不变，
            // 下一个元素会落在这一行里面（静态扫描 `scripts/scale-audit.py` 发现，附录 CO）。
            ry += theme::size::px(theme::size::ROW_H_SM) + 4;
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                crate::i18n::tr(
                    "只列出人类账户（uid 1000–65533）；系统账户不在这里",
                    "Only human accounts (uid 1000–65533); system accounts are hidden",
                ),
                font::CAPTION, color::text_dim(), 0.75,
            );
            ry += theme::size::px(theme::size::HINT_GAP);

            if !users_loaded {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr("正在读取…", "Reading…"),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            } else if users.is_empty() {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::tr(
                        "没有人类账户（这台机器上还没有 uid 1000 起的账户）",
                        "No human accounts (no account starting at uid 1000 yet)",
                    ),
                    font::CAPTION, color::text_dim(), 0.9,
                );
                ry += theme::size::px(theme::size::LINE_GAP);
            } else {
                for u in users {
                    tr.draw_bold(
                        buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                        &u.name, font::CAPTION, color::text(), 0.9,
                    );
                    let detail = format!("uid {} · {}", u.uid, u.shell);
                    tr.draw(
                        buf, w, h, (content.x + 150) as f32, ry as f32,
                        &detail, font::CAPTION, color::text_dim(), 0.85,
                    );
                    ry += theme::size::px(theme::size::LINE_GAP);
                }
            }
            ry += 12;

            // ---- 改口令 ----
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("改口令", "Change password"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                crate::i18n::tr(
                    "旧口令先在本机校验（证明是你本人），新口令再交给 aetherd 写入",
                    "Your current password is checked locally first, then aetherd writes the new one",
                ),
                font::CAPTION, color::text_dim(), 0.75,
            );
            ry += theme::size::px(theme::size::HINT_GAP);

            // 三个框：标签一行、输入框一行。**显示的是圆点**（`PassField::display`）。
            let fields: [(usize, &str, &str, &crate::settings::PassField); 3] = [
                (0, "旧口令", "Current", &pass_form.old),
                (1, "新口令", "New", &pass_form.new),
                (2, "确认新口令", "Confirm", &pass_form.confirm),
            ];
            for (idx, zh, en, field) in fields {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    crate::i18n::pick(lang, zh, en),
                    font::CAPTION, color::text(), 0.85,
                );
                ry += theme::size::px(theme::size::GAP_STD);
                let r = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: 38 };
                // 编辑中在末尾加一个竖线当光标（与 DNS 字段同一做法）
                let shown = if field.editing {
                    format!("{}▏", field.display())
                } else {
                    field.display()
                };
                let hint = if field.editing {
                    crate::i18n::tr("输入中（遮显）· Tab 切换 · 回车提交", "Typing (masked) · Tab to move · Enter to submit")
                } else if field.text.is_empty() {
                    crate::i18n::tr("点击后输入", "Click to type")
                } else {
                    crate::i18n::tr("点击继续编辑", "Click to keep editing")
                };
                widgets::row(
                    buf, w, h, r, &shown, Some(hint),
                    widgets::States::at(r, mouse), Some(tr),
                );
                hits.push((r, SettingsHit::FocusPassField(idx as u8)));
                ry += theme::size::px(theme::size::GAP_WIDE);
            }

            let r_submit = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: 180, h: 38 };
            let submit_label = if pass_form.pending {
                crate::i18n::tr("提交中…", "Submitting…")
            } else {
                crate::i18n::tr("改口令", "Change password")
            };
            widgets::row(
                buf, w, h, r_submit, submit_label, None,
                widgets::States::at(r_submit, mouse), Some(tr),
            );
            hits.push((r_submit, SettingsHit::SubmitPassword));
            ry += theme::size::px(theme::size::GAP_WIDE);
            if let Some(fb) = &pass_form.feedback {
                tr.draw(
                    buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                    fb, font::CAPTION, color::text_dim(), 0.9,
                );
            }
        }
        "通知" => {
            // §3.5：本页文案双语
            let lang = crate::i18n::lang();
            // §3.2：通知历史。两个控件：是否记录 + 清空。
            let r1 = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::CTRL_H) };
            toggle_row(
                buf,
                w,
                h,
                mouse,
                tr,
                r1,
                crate::i18n::pick(lang, "记录通知历史", "Keep notification history"),
                Some(crate::i18n::pick(lang, "关掉后不再记录（提示条本身照常显示，只是不再进历史）", "When off, nothing is recorded (toasts still show, they just stay out of history)")),
                s.record_notifications,
                SettingsHit::ToggleRecordNotifications,
                hits,
            );
            ry += theme::size::px(theme::size::GAP_HUGE);
            // 清空：**动作类**，由事件循环执行（绘制期不改状态）
            let clear = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf,
                w,
                h,
                clear,
                crate::i18n::pick(lang, "清空通知历史", "Clear notification history"),
                Some(crate::i18n::pick(lang, "只清历史，不影响任何其它设置", "Clears history only; no other setting is touched")),
                widgets::States::at(clear, mouse),
                Some(tr),
            );
            hits.push((clear, SettingsHit::ClearNotifications));
            ry += theme::size::px(52);
            draw_wrapped(
                tr,
                buf,
                w,
                h,
                (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                &mut ry,
                crate::i18n::pick(lang, "通知中心在控制中心里打开（点顶栏状态簇）。", "The notification center opens from the control center (click the status cluster in the top bar)."),
                font::CAPTION,
                color::text_dim(),
                0.75,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
            );
            // §5.12 声音：开关 + 主题（这个开关此前**只有字段没有界面**）
            ry += theme::size::px(theme::size::GAP_TIGHT);
            widgets::group_header(
                buf, w, h, content.x + theme::size::px(theme::size::CONTENT_PAD), ry,
                crate::i18n::tr("提示音", "Sounds"), Some(tr),
            );
            ry += theme::size::px(theme::size::GROUP_GAP);
            let r_on = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            toggle_row(
                buf, w, h, mouse, tr, r_on,
                crate::i18n::tr("提示音", "Notification sounds"),
                Some(crate::i18n::tr(
                    crate::i18n::pick(lang, "AI 完成 / 需要确认 / 失败各用不同音高", "AI done / needs confirmation / failed each use a different pitch"),
                    "Different tones for done / confirm / error",
                )),
                s.notify_sound, SettingsHit::ToggleNotifySound, hits,
            );
            ry += theme::size::px(theme::size::ROW_TALL);
            let theme = crate::audio::SoundTheme::of(s.sound_theme);
            let r_theme = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(
                buf, w, h, r_theme,
                &format!(
                    "{}：{}",
                    crate::i18n::tr("提示音主题", "Sound theme"),
                    crate::i18n::pick(crate::i18n::lang(), theme.label(), theme.label_en())
                ),
                Some(crate::i18n::tr("点击切换并试听（经典 / 柔和 / 明亮）", "Click to cycle and preview")),
                widgets::States::at(r_theme, mouse), Some(tr),
            );
            hits.push((r_theme, SettingsHit::CycleSoundTheme));

        }
        _ => {}
    }
}

/// 音乐窗口内容：左侧曲目列表 + 右侧"正在播放"面板。
///
/// 存在的意义是"内容形态跟着窗口语义走"——满窗一模一样的文件夹图标，
/// 是"演示占位"最刺眼的信号。窗口够宽时右挂播放面板，窄时退化为纯列表。
fn draw_music_content(buf: &mut [u32], w: usize, h: usize, r: Rect, win: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>,
    music_hits: &mut Vec<(Rect, usize)>) {
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

    let mut y = list.y + theme::size::px(8);
    let mut i = 0usize;
    loop {
        // 按可用高度铺满：曲目用完后循环取用（演示内容，不是真实曲库）
        if y + theme::size::px(ROW_H) > list.y + list.h - theme::size::px(6) {
            break;
        }
        let (title, artist, dur) = strings::TRACKS[i % strings::TRACKS.len()];
        let row = Rect { x: list.x + theme::size::px(theme::size::PAD_SM), y, w: list.w - 16, h: theme::size::px(ROW_H) - theme::size::px(4) };
        // 登记行矩形：几何断言据此检查行与行是否重叠（本批只取证，不接点击）
        music_hits.push((row, i));
        let hovered = row.contains(mouse.0, mouse.1);
        let playing = i == 0;
        if hovered || playing {
            fill_clipped(buf, w, h, row, win, radius::LG, color::hairline(), if playing { 0.06 } else { state::hover() });
        }
        if playing {
            rounded_rect(buf, w, h, Rect { x: row.x, y: row.y + theme::size::px(9), w: 2, h: row.h - theme::size::px(18) }, 1.0, color::accent(), 0.95);
        }
        let (at, ab) = arts[i % arts.len()];
        gradient_tile(buf, w, Rect { x: row.x + theme::size::px(theme::size::TIGHT), y: row.y + theme::size::px(5), w: theme::size::px(ART), h: ART }, 6.0, at, ab, 0.95);

        let tx = (row.x + theme::size::px(theme::size::TIGHT) + theme::size::px(ART) + theme::size::px(12)) as f32;
        // 给时长让出位置，标题/艺人按剩余宽度截断（窄窗口下不会压到时长上）
        let dur_w = tr.measure(dur, font::LABEL) + 20.0;
        let text_w = (row.x + row.w) as f32 - tx - dur_w;
        let title_s = ellipsize(tr, title, font::BODY, text_w);
        let artist_s = ellipsize(tr, artist, font::LABEL, text_w);
        tr.draw_bold(
            buf, w, h, tx, tr.vcenter(row.y as f32 + theme::size::px(2) as f32, theme::size::px(20) as f32, font::BODY),
            &title_s, font::BODY,
            if playing { color::accent() } else { color::text() },
            0.96,
        );
        draw_text(
            tr, buf, w, h, tx, tr.vcenter(row.y as f32 + theme::size::px(22) as f32, theme::size::px(16) as f32, font::LABEL),
            &artist_s, font::LABEL, color::text_dim(), 0.92,
        );
        let dw = tr.measure(dur, font::LABEL);
        draw_text(
            tr, buf, w, h, (row.x + row.w - 12) as f32 - dw,
            tr.vcenter(row.y as f32 + theme::size::px(2) as f32, theme::size::px(ROW_H) as f32 - theme::size::px(4) as f32, font::LABEL),
            dur, font::LABEL, color::text_faint(), 0.85,
        );
        y += theme::size::px(ROW_H);
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
    let lab_w = tr.measure(crate::i18n::pick(crate::i18n::lang(), strings::NOW_PLAYING, strings::NOW_PLAYING_EN), font::LABEL);
    draw_text(
        tr, buf, w, h,
        panel.x as f32 + (panel.w as f32 - lab_w) / 2.0,
        tr.vcenter(panel.y as f32 + theme::size::px(6) as f32, theme::size::px(18) as f32, font::LABEL),
        crate::i18n::pick(crate::i18n::lang(), strings::NOW_PLAYING, strings::NOW_PLAYING_EN), font::LABEL, color::text_faint(), 0.9,
    );
    let cover = Rect {
        x: panel.x + (panel.w - cover_s) / 2,
        y: panel.y + theme::size::px(30),
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
    ty += theme::size::px(22) as f32;
    let a0 = strings::TRACKS[0].1;
    let aw = tr.measure(a0, font::LABEL);
    draw_text(tr, buf, w, h, cx - aw / 2.0, ty, a0, font::LABEL, color::text_dim(), 0.88);

    // 进度条 + 时间（面板底部）
    let bar_w = panel.w - 48;
    let bx = panel.x + theme::size::px(theme::size::CONTENT_PAD);
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
/// 悬停符号（macOS 红绿灯时期用）。
///
/// 2026-10-01 换成 Windows 三键（[`caption_glyph`]）后不再调用。
/// **保留一个版本周期**：用户若想换回 macOS 风格，改回一行即可；下个版本删除。
#[allow(dead_code)]
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
    // §3.5：本函数的文案双语
    let lang = crate::i18n::lang();
    let r = term_content_rect(win_rect);
    // 终端底色固定深色（理由见 term::ansi_rgb）：内容区之外也要铺满，
    // 否则窗口体（浅色）会从内边距里透出来，看起来像没画完
    fill_clipped(buf, w, h, r, win_rect, radius::LG, TERM_BG, 1.0);

    let Some(tr) = tr else { return };
    let Some(term) = term else {
        draw_text(
            tr, buf, w, h, (r.x + 6) as f32, (r.y + 4) as f32,
            crate::i18n::pick(lang, "（终端会话未建立）", "(no terminal session)"), font::MONO, color::text_faint(), 0.9,
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
            let msg = if crate::i18n::is_en() {
                    format!("Session ended (status {code}) · click Terminal in the Dock to open a new one")
                } else {
                    format!("会话已结束（状态 {code}）· 点 Dock 里的终端可再开一个")
                };
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
    // §3.5：本函数的文案双语
    let lang = crate::i18n::lang();
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
        let mut base = r.y + theme::size::px(theme::size::TIGHT);
        if !path_is_root(fv.cwd) {
            let row = Rect { x: r.x + theme::size::px(theme::size::PAD_SM), y: base, w: sidebar.w - 16, h: theme::size::px(theme::size::CTRL_H_XS) };
            let hovered = row.contains(mouse.0, mouse.1);
            if hovered {
                rounded_rect(buf, w, h, row, radius::SM - 2.0, color::hairline(), state::hover());
            }
            draw_text(
                tr, buf, w, h,
                (row.x + 14) as f32,
                tr.vcenter(row.y as f32, row.h as f32, font::BODY),
                crate::i18n::pick(lang, "← 上级目录", "← Parent folder"), font::BODY,
                if hovered { color::text() } else { color::text_dim() },
                if hovered { 0.98 } else { 0.88 },
            );
            *up = row;
            base += theme::size::px(30);
        }
        for (i, (label, path)) in fv.sidebar.iter().enumerate() {
            let y = base + i as i32 * theme::size::px(30);
            // 当前所在位置高亮：用户随时知道"我在哪一栏"（此前恒为 false，等于没有反馈）
            let selected = path == fv.cwd || fv.cwd.starts_with(&format!("{path}{}", std::path::MAIN_SEPARATOR));
            let row = Rect { x: r.x + theme::size::px(theme::size::PAD_SM), y, w: sidebar.w - 16, h: theme::size::px(theme::size::CTRL_H_XS) };
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
            Some(e) => {
            if crate::i18n::is_en() {
                format!("Cannot read directory: {e}")
            } else {
                format!("无法读取目录：{e}")
            }
        }
            None => {
                // 有滚动时给出**可见区间**（"1–8 / 76"），而不是只说"显示 8"——
                // 后者让用户不知道下面还有没有内容、也不知道自己看到哪儿了
                if fv.entries.len() > shown && shown > 0 {
                    if crate::i18n::is_en() {
                        format!(
                            "{}–{} / {} items",
                            fv.scroll + 1,
                            fv.scroll + shown,
                            fv.entries.len()
                        )
                    } else {
                        format!("{}–{} / {} 项", fv.scroll + 1, fv.scroll + shown, fv.entries.len())
                    }
                } else {
                    if crate::i18n::is_en() {
                        format!("{} items", fv.entries.len())
                    } else {
                        format!("{} 项", fv.entries.len())
                    }
                }
            }
        };
        let left_rgb = if fv.error.is_some() { color::danger_text() } else { color::text_faint() };
        draw_text(tr, buf, w, h, (status.x + theme::size::px(theme::size::TIGHT)) as f32, tr.vcenter(status.y as f32, status.h as f32, font::LABEL), &left, font::LABEL, left_rgb, 0.9);
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
            tr.vcenter(label_y as f32, theme::size::px(14) as f32, font::LABEL),
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
    // §3.5：本函数的文案双语
    let lang = crate::i18n::lang();
    fill_clipped(buf, w, h, r, r, radius::LG, color::surface_2(), 0.42);
    let Some(tr) = tr else { return };

    let Some(d) = data else {
        draw_text(tr, buf, w, h, (r.x + theme::size::px(theme::size::INSET_SM)) as f32, (r.y + 14) as f32, crate::i18n::pick(lang, "（无内容）", "(empty)"), font::BODY, color::text_faint(), 0.9);
        return;
    };

    if let Some(err) = &d.error {
        draw_text(tr, buf, w, h, (r.x + theme::size::px(theme::size::INSET_SM)) as f32, (r.y + 14) as f32, crate::i18n::pick(lang, "无法预览", "Cannot preview"), font::BODY, color::danger_text(), 0.95);
        // §3.5：错误原因来自数据层的枚举，文案在这里按语言渲染
        let msg = crate::draw::preview_error_text(crate::i18n::lang(), err);
        draw_text(tr, buf, w, h, (r.x + theme::size::px(theme::size::INSET_SM)) as f32, (r.y + 40) as f32, &msg, font::CAPTION, color::text_dim(), 0.9);
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
            crate::i18n::pick(lang, "… 空文件", "… empty file").to_string()
        } else if d.truncated {
            // 截断时 "共 N 行" 是**已载入的**行数，不是文件的真实行数 ——
            // 不加这句，用户会以为文件就这么长（改滚动时差点丢掉这个区分）
            if crate::i18n::is_en() {
                format!(
                    "… lines {}-{} / {total} loaded (file too long, not fully loaded)",
                    start + 1,
                    end
                )
            } else {
                format!(
                    "… 第 {}-{} 行 / 已载入 {total} 行（文件过长，未全部载入）",
                    start + 1,
                    end
                )
            }
        } else {
            if crate::i18n::is_en() {
            format!("… lines {}-{} / {total} total", start + 1, end)
        } else {
            format!("… 第 {}-{} 行 / 共 {total} 行", start + 1, end)
        }
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
        fill_rect(buf, w, h, Rect { x: x + theme::size::px(theme::size::PAD_SM), y: ly, w: size - 16 - i * 4, h: 1 }, line, 0.5);
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
    let bar = Rect { x: w as i32 / 2 - bar_w / 2, y: h as i32 - metric::BOTTOM_DOCK - theme::size::px(theme::size::AI_BAR_H) - theme::size::px(theme::size::INSET_SM), w: bar_w, h: theme::size::px(theme::size::AI_BAR_H) };
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
    let av = Rect { x: bar.x + theme::size::px(theme::size::TIGHT), y: bar.y + 10, w: 32, h: 32 };
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
        draw_text(
            tr,
            buf,
            w,
            h,
            text_x,
            text_y,
            crate::i18n::pick(crate::i18n::lang(), strings::AI_BAR_HINT, strings::AI_BAR_HINT_EN),
            font::BODY,
            color::text_dim(),
            0.85,
        );
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
            rounded_rect(buf, w, h, Rect { x: cx, y: bar.y + theme::size::px(23), w: 5, h: 5 }, 2.5, color::accent(), a);
        }
    // 用**传入的 `t`**，不用 `t_now()`：渲染必须是输入的纯函数，
        // 否则同一个状态会画出不同的画面（走查图与像素回归都做不了）。
        // 与本函数上面算焦点环脉动用的是同一个时钟。
        } else if ((t * 2.0) as i32) % 2 == 0 {
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
        x: bar.x + theme::size::px(58), // 与指令条里的文字左对齐
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
    let y = h as i32 - metric::BOTTOM_DOCK - theme::size::px(theme::size::AI_BAR_H) - theme::size::px(theme::size::INSET_SM) - 56;
    // 用户指令靠右、AI 与工具结果靠左：来源方向即身份
    let x = match kind {
        BubbleKind::User => w as i32 / 2 + 60 - bar_w,
        _ => w as i32 / 2 - 60,
    };
    let r = Rect { x, y, w: bar_w, h: theme::size::px(theme::size::ROW_H_SM) };
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
    /// 桌面图标：默认桌面**不开任何窗口**，图标是打开应用的主要入口（Windows/macOS 的桌面语义）。
    ///
    /// 只放**真实能打开**的三项；图形与 Dock 共用 [`draw_app_icon`]。
    /// 图标贴在左侧竖排（Windows 的桌面图标就是左侧竖列），顶部避开顶栏。
    fn draw_desktop_icons(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        ui: &UiState,
        tr: Option<&TextRenderer>,
    ) {
        const ICON: i32 = 48;
        const CELL_W: i32 = 96;
        const CELL_H: i32 = ICON + 34;
        self.desktop_icons.clear();
        let x = 26;
        let mut y = metric::MENUBAR_H as i32 + 20;
        // 序号与 `open_app` 的映射一致：0 文件 / 1 终端 / 4 设置
        for (idx, name) in [
            (0usize, strings::win_label(strings::WIN_FILES)),
            (1, strings::win_label(strings::WIN_TERM)),
            (4, strings::win_label(strings::WIN_SETTINGS)),
        ] {
            let cell = Rect { x: x - 12, y: y - 6, w: CELL_W, h: CELL_H };
            let tile = Rect { x, y, w: ICON, h: ICON };
            let hovered = cell.contains(ui.mouse.0, ui.mouse.1);
            // 悬停反馈：让"能点"这件事可见（浅底 + accent 描边）
            if hovered {
                rounded_rect(buf, w, h, cell, radius::SM, color::hairline(), 0.12);
                rounded_outline(buf, w, h, cell, radius::SM, color::accent(), 0.35);
            }
            draw_app_icon(buf, w, h, tile, idx, false, false, hovered, false);
            // 标签：深色文字（浅色壁纸上是白底黑字，深色主题下 color::text() 自动转浅）
            if let Some(tr) = tr {
                let tw = tr.measure(name, font::LABEL);
                let tx = tile.x + ((ICON as f32 - tw) / 2.0) as i32;
                tr.draw(buf, w, h, tx as f32, (y + ICON + 6) as f32, name, font::LABEL, color::text(), 0.95);
            }
            self.desktop_icons.push((cell, idx));
            y += CELL_H;
        }
    }

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
        fill_rect(buf, w, h, Rect { x: tray.x + theme::size::px(theme::size::INSET_SM), y: tray.y + 1, w: tray.w - 32, h: 1 }, color::HIGHLIGHT, 0.10);

        self.dock_icons.clear();
        for (i, name) in apps.iter().enumerate() {
            let ix = tray.x + metric::DOCK_PAD + i as i32 * (metric::DOCK_ICON + metric::DOCK_PAD);
            let iy = tray.y + metric::DOCK_PAD;
            let tile = Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON };
            self.dock_icons.push(tile);
            let hovered = tile.contains(ui.mouse.0, ui.mouse.1);
            let running = open_titles.contains(name);

            // 底座 + 白色几何符号：与**桌面图标共用**同一函数（两处各画一套必然走形）
            draw_app_icon(buf, w, h, tile, i, i >= DOCK_BUILTINS, *name == strings::INSTALLER, hovered, running);

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
    // §3.5：安装器界面文案双语
    let lang = crate::i18n::lang();
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
        tr.draw_bold(buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, tr.vcenter(win.y as f32, metric::TITLE_H as f32, font::TITLE), "安装 AetherOS", font::TITLE, color::text(), 0.95);
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + metric::TITLE_H, w: win.w - 2, h: 1 }, color::hairline(), 0.08);

        // 关闭按钮（右上角，与窗口标题栏三键同一套视觉）。
        // **安装进行中禁用**：写到一半取消是危险操作，不该给这个按钮。
        let cbtn = Rect {
            x: win.x + win.w - metric::CAPTION_BTN_W,
            y: win.y,
            w: metric::CAPTION_BTN_W,
            h: metric::TITLE_H,
        };
        let running = matches!(inst.phase, InstallerPhase::Running);
        let chover = !running && cbtn.contains(mouse.0, mouse.1);
        if chover {
            fill_rect(buf, w, h, cbtn, color::close_hover(), 1.0);
        }
        caption_glyph(
            buf, w, h, cbtn, 2,
            if chover {
                color::HIGHLIGHT
            } else if running {
                color::text_dim()
            } else {
                color::text()
            },
            if running { 0.35 } else { 0.9 },
        );
        self.installer_close = cbtn;

        // 提示行
        draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, (win.y + 52) as f32,
                  crate::i18n::pick(lang, "选择目标磁盘并确认。整盘覆盖，目标盘上的数据将丢失。", "Pick a target disk and confirm. The whole disk is erased; its data will be lost."), font::CAPTION, color::text_dim(), 0.9);

        // 磁盘行
        self.installer_rows.clear();
        let row_y0 = win.y + 78;
        for (i, (dev, mb)) in inst.disks.iter().enumerate() {
            let r = Rect { x: win.x + theme::size::px(theme::size::INSET_SM), y: row_y0 + i as i32 * 50, w: win.w - 32, h: theme::size::px(theme::size::CTRL_H) };
            if inst.selected == i {
                rounded_rect(buf, w, h, r, radius::MD, color::accent(), 0.16);
                rounded_outline(buf, w, h, r, radius::MD, color::accent(), 0.45);
                rounded_rect(buf, w, h, Rect { x: r.x + theme::size::px(theme::size::TIGHT), y: r.y + 16, w: 12, h: 12 }, 6.0, color::accent(), 0.95);
            } else {
                rounded_rect(buf, w, h, r, radius::MD, color::hairline(), 0.05);
                rounded_outline(buf, w, h, r, radius::MD, color::hairline(), 0.10);
                rounded_outline(buf, w, h, Rect { x: r.x + 11, y: r.y + 15, w: 14, h: 14 }, 7.0, color::text_dim(), 0.7);
            }
            tr.draw_bold(buf, w, h, (r.x + theme::size::px(theme::size::INDENT_SUB)) as f32, tr.vcenter(r.y as f32, r.h as f32, font::BODY), dev, font::BODY, color::text(), 0.92);
            let sz = if crate::i18n::is_en() {
                format!("{mb} MB free")
            } else {
                format!("{mb} MB 可用")
            };
            let sw = tr.measure(&sz, font::CAPTION);
            draw_text(tr, buf, w, h, (r.x + r.w - 16) as f32 - sw, tr.vcenter(r.y as f32, r.h as f32, font::CAPTION), &sz, font::CAPTION, color::text_dim(), 0.8);
            self.installer_rows.push((r, dev.clone(), *mb));
        }

        // 阶段信息行
        let msg_y = (win.y + win_h - 104) as f32;
        match (inst.phase, inst.message) {
            (InstallerPhase::Running, _) => {
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, crate::i18n::pick(lang, "正在写入磁盘，请勿关机…", "Writing to disk, do not power off…"), font::BODY, color::accent(), 0.95);
            }
            (InstallerPhase::Done, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, &shown, font::CAPTION, color::success(), 0.95);
            }
            (InstallerPhase::Failed, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, &shown, font::CAPTION, color::danger(), 0.95);
            }
            _ => {
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, crate::i18n::pick(lang, "就绪。也可以选中磁盘后按 Enter 开始。", "Ready. You can also pick a disk and press Enter to start."), font::CAPTION, color::text_dim(), 0.85);
            }
        }

        // 主按钮（四状态：默认 / 悬停 / 按下 / 禁用；禁用以状态色而非灰表达语义）
        let button = Rect { x: win.x + win.w / 2 - 160, y: win.y + win_h - 62, w: 320, h: theme::size::px(theme::size::CTRL_H) };
        // 四状态按钮：先取 (中文, English)，再按语言挑 —— 比写两遍 match 短，
        // 也不会出现"新增一个阶段却只加了一种语言"
        let label = {
            let (zh, en) = match inst.phase {
                InstallerPhase::Idle => ("开始安装（整盘覆盖）", "Install (erase disk)"),
                InstallerPhase::Running => ("安装中…", "Installing…"),
                InstallerPhase::Done => ("完成 · 重启后从磁盘引导", "Done · boots from disk after reboot"),
                InstallerPhase::Failed => ("重试安装", "Retry install"),
            };
            crate::i18n::pick(lang, zh, en)
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
/// 工具名 → 给人看的中文/英文名（§3.5）。
///
/// 收 `en: bool` 而不是在内部读全局：绘制工具函数保持**纯**，
/// 语言由调用方决定 —— 与 `audit::age_text_en` 同一套做法。
fn tool_label(tool: &str, en: bool) -> &str {
    match (tool, en) {
        ("install_disk", false) => "整盘安装到磁盘",
        ("install_disk", true) => "Install to disk (erase)",
        ("desktop", false) => "桌面操作",
        ("desktop", true) => "Desktop actions",
        ("read_file", false) => "读取文件",
        ("read_file", true) => "Read file",
        ("sys_probe", false) => "运行系统探针",
        ("sys_probe", true) => "Run system probe",
        ("sys_info", false) => "查询系统状态",
        ("sys_info", true) => "Query system state",
        _ => tool,
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
        // 高度由「数量 × 每行高度 + 固定块」算出：**每一项都要跟着缩放**，
    // 否则 150% 下参数行长大、窗口不长 → 内容被切（附录 DA）。
    let win_h = theme::size::px(104)
        + args.len() as i32 * theme::size::px(22)
        + theme::size::px(54)
        + echo_block
        + theme::size::px(68);
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
        let badge_text = if crate::i18n::is_en() {
            format!(
                "L{} {}",
                c.level,
                if c.level >= 3 { "danger" } else { "sensitive write" }
            )
        } else {
            format!("L{} {}", c.level, if c.level >= 3 { "危险" } else { "敏感写" })
        };
        let bw = tr.measure_bold(&badge_text, font::LABEL) + 18.0;
        let badge = Rect { x: win.x + theme::size::px(theme::size::PAD_LG), y: win.y + 20, w: bw as i32, h: 22 };
        // 底色压到 0.12：再浓就会把文字对比度拖到 AA 以下（P2-20 同类）
        rounded_rect(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.12);
        rounded_outline(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.7);
        draw_text(tr, buf, w, h, (badge.x + 9) as f32, tr.vcenter(badge.y as f32, badge.h as f32, font::LABEL), &badge_text, font::LABEL, badge_text_rgb, 1.0);
        tr.draw_bold(
            buf, w, h,
            (badge.x + badge.w + 12) as f32,
            tr.vcenter(win.y as f32 + theme::size::px(18) as f32, theme::size::px(26) as f32, 17.0),
            tool_label(c.tool, crate::i18n::is_en()), 17.0, color::text(), 0.96,
        );
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + theme::size::px(58), w: win.w - 2, h: 1 }, color::hairline(), 0.08);

        // 参数明文（逐项原样展示：确认的前提是看清对象）
        let mut y = win.y + 74;
        draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::PAD_LG)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION),
            crate::i18n::pick(crate::i18n::lang(), "工具", "Tool"), font::CAPTION, color::text_faint(), 0.9);
        draw_text(tr, buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::BODY), c.tool, font::BODY, color::text_dim(), 0.95);
        y += theme::size::px(22);
        for (k, v) in &args {
            draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::PAD_LG)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), k, font::CAPTION, color::text_faint(), 0.9);
            tr.draw_bold(buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::BODY), v, font::BODY, color::text(), 0.96);
            y += theme::size::px(22);
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
        draw_text(tr, buf, w, h, (win.x + 42) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), &shown, font::CAPTION, color::text(), 0.95);
        y += theme::size::px(34);

        // L3 回显确认：必须原样输入目标，防"手滑点确认"
        if let Some(target) = c.echo_required {
            let tip = if crate::i18n::is_en() {
            format!("L3 dangerous action: type {target} exactly to confirm")
        } else {
            format!("L3 危险操作：请原样输入 {target} 以确认")
        };
            draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::PAD_LG)) as f32, y as f32, &tip, font::CAPTION, color::text_dim(), 0.9);
            y += theme::size::px(22);
            let field = Rect { x: win.x + theme::size::px(theme::size::PAD_LG), y, w: win.w - 40, h: 38 };
            let ok = c.echo_ok();
            rounded_rect(buf, w, h, field, radius::SM, color::inset(), 0.9);
            rounded_outline(buf, w, h, field, radius::SM, if ok { color::success() } else { color::hairline() }, if ok { 0.75 } else { 0.18 });
            let fty = tr.vcenter(field.y as f32, field.h as f32, font::BODY);
            if c.echo_input.is_empty() {
                let hint = format!("输入 {target}");
                draw_text(tr, buf, w, h, (field.x + theme::size::px(theme::size::TIGHT)) as f32, fty, &hint, font::BODY, color::text_faint(), 0.8);
            } else {
                draw_text(tr, buf, w, h, (field.x + theme::size::px(theme::size::TIGHT)) as f32, fty, c.echo_input, font::BODY, color::text(), 0.96);
            }
            let tw = tr.measure(c.echo_input, font::BODY);
            if ((t * 2.0) as i32) % 2 == 0 {
                fill_rect(buf, w, h, Rect { x: (field.x + theme::size::px(theme::size::TIGHT)) as i32 + tw as i32, y: field.y + 10, w: 2, h: 18 }, color::accent(), 0.9);
            }
            self.confirm_echo = field;
            y += theme::size::px(50);
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
        y: h as i32 - metric::BOTTOM_DOCK - theme::size::px(theme::size::AI_BAR_H) - theme::size::px(theme::size::INSET_SM) - 60,
        w: tw as i32,
        h: theme::size::px(theme::size::ROW_H),
    };
    shadow(buf, w, h, r, radius::SM, alpha * elevation::elev_2());
    rounded_rect(buf, w, h, r, 20.0, color::surface_2(), alpha * 0.92);
    rounded_outline(buf, w, h, r, 20.0, color::hairline(), alpha * 0.15);
    fill_rect(buf, w, h, Rect { x: r.x + theme::size::px(theme::size::CONTROL_INSET), y: r.y + 17, w: 6, h: 6 }, color::accent(), alpha);
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
        Rect { x: 10, y: 20, w: 100, h: theme::size::px(theme::size::ROW_H) }
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
        let rr = Rect { x: 0, y: 0, w: 200, h: theme::size::px(theme::size::AI_BAR_H) };
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
            (Rect { x: 10, y: 20, w: 100, h: theme::size::px(theme::size::ROW_H) }, 12.0f32),
            (Rect { x: 0, y: 0, w: 200, h: theme::size::px(theme::size::AI_BAR_H) }, 26.0),
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
            (Rect { x: 0, y: 0, w: 200, h: theme::size::px(theme::size::AI_BAR_H) }, 26.0), // 胶囊
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
            (Rect { x: 30, y: 10, w: 100, h: theme::size::px(theme::size::ROW_H) }, Rect { x: 0, y: 0, w: 240, h: 140 }, 26.0),
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
        // 按标题定位「时钟与时区」——**不要用 `first_enabled_page()`**：外观页启用后它已是第一页，
        // 而本测试要验的是时钟页那几个控件。
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "时钟与时区")
            .expect("时钟与时区页必须存在");
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page, &mut hits,
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

        // 3) 不可用的页不能出现在命中区里（点了不该有反应）。
        //    2026-10-02：三页全部接入后已无不可用页 —— 有才断言，没有就跳过。
        if let Some(disabled) = crate::settings::PAGES.iter().position(|p| !p.enabled) {
            assert!(
                !hits.iter().any(|(_, x)| matches!(x, H::Page(i) if *i == disabled)),
                "不可用页不该登记命中区"
            );
        }

        // 4) 两栏结构：左栏底（inset 0.55）应比右侧页面（保持底色）暗
        let lum = |p: u32| ((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF);
        let y = 590; // 左栏页行在 ~390 之前结束，取靠下的空白处更稳
        let left = lum(buf[y * w + 80]);
        let right = lum(buf[y * w + 700]);
        assert!(
            left < right,
            "左栏应比右侧页面暗（两栏结构）：left={left} right={right}"
        );

        // 「外观」页必须登记深色模式开关 —— 这是"默认白、用户可切换"的界面入口
        let appear = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "外观")
            .expect("外观页必须存在");
        let mut hits2 = Vec::new();
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), appear, &mut hits2,
        );
        assert!(
            hits2.iter().any(|(_, x)| matches!(x, H::ToggleDark)),
            "外观页缺少深色模式开关"
        );
        assert!(
            hits2.iter().all(|(rr, _)| rr.w > 0 && rr.h > 0),
            "外观页的命中区不能为空"
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
            draw_background(&mut whole, w, h, 1.2, 0, false);
            let mut y = 0;
            while y < h {
                let y1 = (y + 8).min(h);
                draw_background_rows(&mut banded, w, h, 1.2, y, y1, 0, false);
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
        draw_background_rows(&mut buf, w, h, 1.0, h, h + 5, 0, false); // y0 == h
        draw_background_rows(&mut buf, w, h, 1.0, 100, 200, 0, false); // 完全越界
        draw_background_rows(&mut buf, w, h, 1.0, 5, 2, 0, false); // 反向区间
        assert!(buf.iter().all(|&p| p == 0), "越界行带不应写入任何像素");
    }
    /// §2.4：网络页必须有一个**可点**的 DNS 编辑入口。
    ///
    /// 这条测试盯的是"控件真的登记了命中区"——只画出来但没登记命中，
    /// 用户点下去毫无反应，而这正是本项目最不能接受的那种"假控件"。
    #[test]
    fn network_page_offers_a_clickable_dns_field() {
        // 本模块没有 import 这两个名字，用函数内 `use` 最不容易互相干扰
        use crate::draw::{draw_settings_content, Rect};
        // 与同文件其它绘制测试一致：**没有字体就跳过**。
        // 注意必须传 `Some(&tr)` —— `tr = None` 时绘制会提前返回，
        // 一个命中区都不会登记（第一次写这条测试时就是栽在这里：
        // 测试失败，但原因是"没画"，不是"控件没登记"）。
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let w = 420;
        let h = 320;
        let mut buf = vec![0u32; w * h];
        let mut hits = Vec::new();
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "网络")
            .expect("应当存在「网络」设置页");
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page,
            &mut hits,
        );
        assert!(
            hits.iter().any(|(_, x)| matches!(x, crate::settings::SettingsHit::EditDns)),
            "网络页必须登记 DNS 编辑入口的命中区，否则就是「点了没用」的假控件"
        );
        // 每个命中区都要有面积（零面积区域点不到）
        for (rect, _) in &hits {
            assert!(rect.w > 0 && rect.h > 0, "命中区不能是零面积：{rect:?}");
        }
    }

    /// §3.4 无障碍：**用 WCAG 对比度客观验证**，而不是靠肉眼说「更清楚了」。
    ///
    /// 用纯函数（收 `light` 参数）而不是全局状态：测试并行执行，
    /// 全局原子量会被其它用例改掉，断言就成了掷骰子。
    #[test]
    fn high_contrast_raises_wcag_ratios_in_both_themes() {
        use crate::draw::theme::color::{contrast_ratio, push_bg, push_fg};
        // 基色取自色板（与 color:: 里的字面量一致）
        let cases: Vec<(&str, bool, [u8; 3], [u8; 3])> = vec![
            ("浅色/主文字", true, [26, 28, 34], [255, 255, 255]),
            ("浅色/次要文字", true, [94, 98, 108], [255, 255, 255]),
            ("浅色/占位文字", true, [146, 150, 160], [255, 255, 255]),
            ("深色/主文字", false, [240, 241, 246], [46, 49, 60]),
            ("深色/次要文字", false, [180, 182, 192], [46, 49, 60]),
            ("深色/占位文字", false, [136, 139, 150], [46, 49, 60]),
        ];
        for (name, light, fg, bgground) in cases {
            let before = contrast_ratio(fg, bgground);
            let after = contrast_ratio(push_fg(fg, light), push_bg(bgground, light));
            assert!(
                after > before,
                "{name}：高对比度**没有**提升对比度（{before:.2} → {after:.2}）"
            );
            assert!(
                after >= 4.5,
                "{name}：高对比度下仍低于 WCAG AA 正文 4.5（实得 {after:.2}）"
            );
        }
    }

    /// 最弱的一档（占位文字）在正常模式下**本来就不达标** —— 这条记录现状，
    /// 也说明高对比度不是"锦上添花"而是必需：它把这档从 ~3 提到 ≥4.5。
    #[test]
    fn faint_text_is_the_one_that_actually_needs_high_contrast() {
        use crate::draw::theme::color::{contrast_ratio, push_fg};
        let normal = contrast_ratio([146, 150, 160], [255, 255, 255]);
        assert!(normal < 4.5, "占位文字在正常模式下就不该达标（实得 {normal:.2}）");
        let hc = contrast_ratio(push_fg([146, 150, 160], true), [255, 255, 255]);
        assert!(hc >= 4.5, "高对比度必须把它拉过 AA（实得 {hc:.2}）");
    }

    /// 变换的方向性：**浅色主题往前景更暗、深色主题更亮**，绝不能反。
    ///
    /// 这条是防"帮倒忙"的：如果只写"推向极端"，浅色主题下的灰字会被推**亮**，
    /// 与白底对比更差 —— 那是把无障碍做成了反效果。
    #[test]
    fn push_direction_depends_on_theme() {
        use crate::draw::theme::color::push_fg;
        let grey = [146u8, 150, 160];
        let light_hc = push_fg(grey, true);
        let dark_hc = push_fg(grey, false);
        // 用 u32 求和：u8 相加会溢出（146+150+160 = 456 > 255）
        let sum = |c: [u8; 3]| c.iter().map(|v| *v as u32).sum::<u32>();
        assert!(
            sum(light_hc) < sum(grey),
            "浅色主题应把前景推得更暗：{grey:?} → {light_hc:?}"
        );
        assert!(
            sum(dark_hc) > sum(grey),
            "深色主题应把前景推得更亮：{grey:?} → {dark_hc:?}"
        );
    }

    /// §3.4：「外观」页必须登记高对比度开关 —— 无障碍功能**只有真的能点到**才算做了。
    #[test]
    fn appearance_page_registers_the_high_contrast_switch() {
        // 本模块没有 import 这两个名字，用函数内 use 最不容易互相干扰
        use crate::draw::{draw_settings_content, Rect};
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "外观")
            .expect("应存在「外观」页");
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page, &mut hits,
        );
        use crate::settings::SettingsHit as H;
        assert!(
            hits.iter().any(|(_, x)| matches!(x, H::ToggleContrast)),
            "外观页缺少高对比度开关"
        );
        // 命中区不能是零面积（零面积 = 点不到）
        for (rect, x) in &hits {
            if matches!(x, H::ToggleContrast) {
                assert!(rect.w > 0 && rect.h > 0, "高对比度开关命中区是零面积：{rect:?}");
            }
        }
    }

    /// §3.5：「外观」页必须登记**语言开关**。
    ///
    /// 此前 `locale` 字段存在、能落盘，却**既改不了也不生效** ——
    /// 这条测试盯的就是"存了不用"这种半拉子状态不许再出现。
    #[test]
    fn appearance_page_registers_the_language_switch() {
        // 本模块没有 import Rect，用函数内 use
        use crate::draw::Rect;
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "外观")
            .expect("应存在「外观」页");
        crate::draw::draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page, &mut hits,
        );
        use crate::settings::SettingsHit as H;
        let locale_hits: Vec<_> = hits.iter().filter(|(_, x)| matches!(x, H::Locale(_))).collect();
        assert!(
            !locale_hits.is_empty(),
            "外观页缺少语言开关（`locale` 又会变成「存了不用」）"
        );
        // 默认中文下，点它应当切到 English（下标 1）
        assert!(
            locale_hits.iter().any(|(_, x)| matches!(x, H::Locale(1))),
            "默认中文时，语言开关应指向 English"
        );
        for (rect, _) in locale_hits {
            assert!(rect.w > 0 && rect.h > 0, "语言开关命中区是零面积：{rect:?}");
        }
    }

    /// 冒烟测试：**每个可用的设置页都要能画出来而不 panic**。
    ///
    /// 新加一页（如 §5.9 的快捷键页）时最容易忘的就是"能画"——
    /// 而这类问题只在真机打开那一页时才暴露。这条测试一次覆盖所有页。
    #[test]
    fn every_enabled_settings_page_draws_without_panicking() {
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let r = crate::draw::Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        for (i, p) in crate::settings::PAGES.iter().enumerate() {
            if !p.enabled {
                continue;
            }
            let mut buf = vec![0u32; w * h];
            let mut hits = Vec::new();
            crate::draw::draw_settings_content(
                &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), i, &mut hits,
            );
            assert!(
                hits.iter().any(|(_, x)| matches!(x, crate::settings::SettingsHit::Page(_))),
                "页「{}」没有登记左栏导航命中区",
                p.title
            );
        }
    }

    /// §5.11：应用商店页要**分别登记**「可安装」与「已安装」两类命中。
    ///
    /// 这条测试盯的是"点了没反应/点错了"这类问题：可安装的那行必须是 `InstallApp`、
    /// 已安装的那行必须是 `RemoveApp`，而**已安装**与**未安装**的判断依据是商店
    /// 自己读到的版本（P-1 曾因为把它当成"名字"而全部判成「重新安装」）。
    #[test]
    fn app_store_page_registers_install_and_remove_hits() {
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        // 造两个临时目录：可安装目录里有两个包，已装目录里只有一个（版本一致）
        let root = std::env::temp_dir().join(format!(
            "aether-storeui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let (cat, inst) = (root.join("cat"), root.join("inst"));
        for (base, id, name, ver) in [
            (&cat, "alpha", "甲", "1.0"),
            (&cat, "beta", "乙", "2.0"),
            (&inst, "alpha", "甲", "1.0"),
        ] {
            std::fs::create_dir_all(base.join(id)).unwrap();
            std::fs::write(
                base.join(id).join("app.json"),
                format!(r#"{{"id":"{id}","name":"{name}","version":"{ver}"}}"#),
            )
            .unwrap();
        }
        let mut view = crate::store::StoreView::default();
        view.refresh(&cat, &inst);
        assert_eq!(view.installed().len(), 1, "应读到 1 个已装应用");
        assert_eq!(view.action_of(0), Some(crate::store::StoreAction::UpToDate));
        assert_eq!(view.action_of(1), Some(crate::store::StoreAction::Install));

        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        let r = crate::draw::Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "应用商店")
            .expect("应用商店页必须存在");
        crate::draw::draw_settings_content(
            &mut buf, w, h, r,
            crate::draw::test_ctx_with_store(Some(&tr), &view),
            page, &mut hits,
        );
        use crate::settings::SettingsHit as H;
        let installs: Vec<usize> = hits
            .iter()
            .filter_map(|(_, x)| match x {
                H::InstallApp(i) => Some(*i),
                _ => None,
            })
            .collect();
        let removes: Vec<usize> = hits
            .iter()
            .filter_map(|(_, x)| match x {
                H::RemoveApp(i) => Some(*i),
                _ => None,
            })
            .collect();
        // 已安装的 alpha（下标 0）不该像按钮一样能点 → 只有 beta 是可点的安装项
        assert_eq!(installs, vec![1], "只有未安装的 beta 该登记安装命中：{installs:?}");
        assert_eq!(removes, vec![0], "已安装的 alpha 该登记卸载命中：{removes:?}");
        // 命中区不能在窗口外（否则点不到）
        for (rr, id) in &hits {
            assert!(
                rr.x >= r.x && rr.y >= r.y && rr.x + rr.w <= r.x + r.w && rr.y + rr.h <= r.y + r.h,
                "命中区越界：{id:?} {rr:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// §3.4：字号缩放必须是**纯函数**可测的（不碰全局状态 → 并行测试不会互相干扰）。
    #[test]
    fn font_scaling_is_pure_and_snaps_to_steps() {
        use crate::draw::theme::ui::font_at;
        assert_eq!(font_at(100, 14.0), 14.0);
        assert_eq!(font_at(125, 14.0), 17.5);
        assert_eq!(font_at(150, 14.0), 21.0);
        // 越界/奇怪的值要吸附到最近档位（设置文件可以被手改）
        assert_eq!(font_at(0, 20.0), 20.0, "0 应当吸附到 100%");
        // 注意：**112 才是更接近 100 的那个**（113 更接近 125）——
        // 这条语义在 `settings.rs` 的 `snap_scale` 测试里已经写死了，这里与它保持一致。
        assert_eq!(font_at(112, 20.0), 20.0, "112 更接近 100");
        assert_eq!(font_at(113, 20.0), 25.0, "113 更接近 125");
        assert_eq!(font_at(120, 20.0), 25.0, "120 更接近 125");
        assert_eq!(font_at(999, 10.0), 15.0, "999 吸附到最大档 150%");
        // 永远为正（0 字号会让渲染器除零/画不出东西）
        assert!(font_at(100, 0.0) >= 1.0);
    }

    /// **护栏**：最大档位下，关键字号必须仍然**小于它所在的固定行高** ——
    /// 不缩布局就放大字号，最容易出的问题就是文字互相压住。
    ///
    /// 这条测试的价值在于：以后有人调大 `font::TITLE`/`BODY` 或调小 `metric::` 高度，
    /// 它会立刻红 —— 而不是等到用户在 150% 下看到错位的界面。
    #[test]
    fn the_largest_step_still_fits_the_fixed_row_heights() {
        use crate::draw::theme::ui::font_at;
        use crate::draw::theme::{font, metric};
        let max = crate::settings::UI_SCALE_STEPS.iter().copied().max().unwrap_or(150);
        // 标题栏要装得下 TITLE
        assert!(
            font_at(max, font::TITLE) <= metric::TITLE_H as f32,
            "最大档 {max}% 下标题字号 {} 超过了标题栏高 {}",
            font_at(max, font::TITLE),
            metric::TITLE_H
        );
        // 顶栏要装得下 BODY
        assert!(
            font_at(max, font::BODY) <= metric::MENUBAR_H as f32,
            "最大档 {max}% 下正文字号 {} 超过了顶栏高 {}",
            font_at(max, font::BODY),
            metric::MENUBAR_H
        );
        // 设置页的行高（40）要装得下正文 + 说明两行里的较大者
        assert!(
            font_at(max, font::BODY) <= 40.0,
            "最大档 {max}% 下正文超过了设置行高 40"
        );
    }

    /// §3.5：AI 三态的英文标签必须齐全，且与中文不同。
    #[test]
    fn ai_status_has_english_labels_for_every_state() {
        for st in [
            crate::draw::AiStatus::Local,
            crate::draw::AiStatus::Cloud,
            crate::draw::AiStatus::Offline,
        ] {
            assert!(!st.label().is_empty(), "中文标签不能为空");
            assert!(!st.label_en().is_empty(), "英文标签不能为空");
            assert_ne!(st.label(), st.label_en(), "两种语言不该一模一样");
        }
    }

    /// §3.5：**控制中心的双语必须真的写在源码里**（探针式核对）。
    ///
    /// 为什么用源码扫描而不是画一遍看：绘制结果没法断言"这行字是英文"，
    /// 而这里要守的性质很具体 —— 每一处文案都**同时**写了中文和英文。
    /// 有人把某处改回硬编码中文时，这条测试立刻红
    /// （与快捷键总览那套"探针"同一思路）。
    #[test]
    fn control_center_strings_are_bilingual_in_the_source() {
        let src = include_str!("draw.rs");
        let pairs = [
            ("AI 中枢", "AI core"),
            ("深色模式", "Dark mode"),
            ("中文输入法", "Chinese IME"),
            ("24 小时制", "24-hour clock"),
            ("快捷跳转", "Quick actions"),
            ("打开设置", "Open Settings"),
            ("打开终端", "Open Terminal"),
            ("通知", "Notifications"),
            ("声音", "Sound"),
            ("音量", "Volume"),
            ("电源", "Power"),
            ("关机", "Power off"),
            ("重启", "Restart"),
            ("注销", "Log out"),
            ("Esc 或点击别处关闭", "Esc or click elsewhere to close"),
        ];
        for (zh, en) in pairs {
            let probe = format!("(\"{zh}\", \"{en}\"");
            assert!(
                src.contains(&probe),
                "控制中心的「{zh}」没有配对英文「{en}」——它被改回硬编码中文了？"
            );
        }
        // 带未读数的动态文案走语言分支，单独核对两条分支都在
        assert!(src.contains("Notifications ({unread})"), "动态文案缺英文分支");
        assert!(src.contains("通知（{unread} 条未读）"), "动态文案缺中文分支");
    }

    /// §5.5：命令面板的**可见行都要能点**，且登记的是**条目下标**（不是行号）。
    ///
    /// 这条盯的是"点了没反应 / 点错了"：如果没有登记命中区，`palette_hit` 永远是 `None`；
    /// 如果登记成了行号，点了第一条会执行到别的条目（行是过滤排序后的，两者不同）。
    #[test]
    fn palette_registers_a_clickable_row_for_every_visible_result() {
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (800usize, 600usize);
        let mut p = crate::palette::Palette::default();
        p.open_with(crate::palette::build_items(
            &["a", "b"],
            &[("设置：外观", "Settings: Appearance"), ("设置：网络", "Settings: Network")],
            &[],
        ));
        let mut buf = vec![0u32; w * h];
        let mut hits = Vec::new();
        crate::draw::draw_palette(&mut buf, w, h, &p, Some(&tr), &mut hits);
        assert_eq!(
            hits.len(),
            p.visible().len(),
            "每一条可见结果都该登记命中区"
        );
        assert!(!hits.is_empty(), "至少要有一条可见结果，否则这条测试没有意义");
        // 命中区要有真实大小，且登记的必须是**条目下标**
        for (i, (r, item)) in hits.iter().enumerate() {
            assert!(r.w > 0 && r.h > 0, "命中区不能是空的：{r:?}");
            assert_eq!(
                *item, p.visible()[i].item,
                "第 {i} 行登记的必须是它对应的条目下标"
            );
        }
    }

    /// §3.5：预览失败原因的**两套文案**，四种原因全覆盖。
    ///
    /// 这条测试能存在，正因为文案被抽成了纯函数（`preview_error_text`）——
    /// 之前文案住在 `read_preview` 里、又只有中文，既不可测也没法翻译。
    #[test]
    fn preview_error_text_covers_all_reasons_in_both_languages() {
        use crate::i18n::Lang;
        use crate::draw::{preview_error_text, PreviewError};
        let cases: Vec<PreviewError> = vec![
            PreviewError::IsDir,
            PreviewError::TooLarge("12.3 MB".into()),
            PreviewError::Binary,
        ];
        for e in &cases {
            let zh = preview_error_text(Lang::Zh, e);
            let en = preview_error_text(Lang::En, e);
            assert!(!zh.is_empty() && !en.is_empty(), "{e:?} 两种语言都要有文案");
            assert!(
                !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "英文文案里有汉字：{en}"
            );
        }
        // 带参数的那种：大小文字必须出现在两种文案里（否则用户不知道到底多大）
        let big = PreviewError::TooLarge("12.3 MB".into());
        assert!(preview_error_text(Lang::Zh, &big).contains("12.3 MB"));
        assert!(preview_error_text(Lang::En, &big).contains("12.3 MB"));
        // Io / Message 原样透传：它们不是本模块的话，不该被改写
        let io = PreviewError::Io("permission denied".into());
        assert_eq!(preview_error_text(Lang::Zh, &io), "permission denied");
        assert_eq!(preview_error_text(Lang::En, &io), "permission denied");
        let m = PreviewError::Message("界面自己取好的话".into());
        assert_eq!(preview_error_text(Lang::En, &m), "界面自己取好的话");
    }

}
