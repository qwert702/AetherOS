//! 绘制图元：颜色混合、圆角矩形/描边/渐变/投影、矩形几何与 BMP/dirty-row 工具。
//!
//! 本模块为纯软件光栅图元，不持有状态；由 `draw.rs` 统一再导出。
use super::*;

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
pub fn darken_pixel(buf: &mut [u32], idx: usize, alpha: f32) {
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

    /// 两个矩形的交集；宽高为非正即表示不相交（空矩形）。
    #[inline]
    pub fn intersect(&self, o: Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }

    /// `o` 是否完整落在 `self` 内部（含边界）。
    #[inline]
    pub fn contains_rect(&self, o: Rect) -> bool {
        o.x >= self.x
            && o.y >= self.y
            && o.x + o.w <= self.x + self.w
            && o.y + o.h <= self.y + self.h
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
pub fn sdf_round_rect(r: Rect, radius: f32, x: f32, y: f32) -> f32 {
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
pub fn row_inner_span(r: Rect, radius: f32, y: f32) -> (i32, i32) {
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
pub fn fill_clipped(buf: &mut [u32], w: usize, h: usize, area: Rect, shape: Rect, shape_radius: f32, rgb: [u8; 3], alpha: f32) {
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
pub fn gradient_stops(buf: &mut [u32], w: usize, r: Rect, radius: f32, stops: &[(f32, [u8; 3])], alpha: f32) {
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
pub fn sample_stops(stops: &[(f32, [u8; 3])], t: f32) -> [u8; 3] {
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
pub fn gradient_tile(buf: &mut [u32], w: usize, r: Rect, radius: f32, top: [u8; 3], bottom: [u8; 3], alpha: f32) {
    gradient_stops(buf, w, r, radius, &[(0.0, top), (1.0, bottom)], alpha);
}
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
