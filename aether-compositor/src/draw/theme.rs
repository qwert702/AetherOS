//! "Essence" 设计令牌：颜色 / 字阶 / 圆角 / 阴影 / 交互态 / 尺寸。
//!
//! 2026-11-13 从 draw.rs 抽出（E1 第一步），模块路径仍是 `draw::theme`。

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
    let src = include_str!("../draw.rs");
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
