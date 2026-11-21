//! 图标与光标：应用图标、窗口标题栏符号、箭头/灯形、文件夹/文件图标、鼠标光标。
use super::*;

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
pub fn caption_glyph(buf: &mut [u32], w: usize, h: usize, r: Rect, kind: u8, rgb: [u8; 3], alpha: f32) {
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

/// 三角箭头（上一个/下一个）：按列扫描填充，纯几何不依赖字体字形。
/// `dir` = -1 左指 / +1 右指；`cx` 为三角尖端所在列。
pub fn arrow_glyph(buf: &mut [u32], w: usize, h: usize, cx: i32, cy: i32, size: i32, dir: i32, rgb: [u8; 3], alpha: f32) {
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
pub fn light_symbol(buf: &mut [u32], w: usize, h: usize, cx: i32, cy: i32, kind: usize) {
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

/// 文件夹图标：图形本身即图标（不套底色块），渐变填充 + 页签 + 顶部高光。
/// 饱和度刻意压低——一屏几十个图标，每个都艳就是灾难。
pub fn draw_folder_icon(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, size: i32) {
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
pub fn draw_file_icon(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, size: i32) {
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
