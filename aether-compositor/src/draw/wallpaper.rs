//! 壁纸：低饱和中性垂直渐变，分帧生成行带。
use super::*;

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
