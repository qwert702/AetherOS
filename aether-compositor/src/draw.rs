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

/// "Essence" 色板：中性底 + 单点缀。
pub mod palette {
    /// 壁纸顶
    pub const BG_TOP: [u8; 3] = [19, 21, 30];
    /// 壁纸底
    pub const BG_BOTTOM: [u8; 3] = [36, 40, 55];
    /// 面板深灰（窗体）
    pub const PANEL: [u8; 3] = [44, 45, 52];
    /// 面板更深（菜单栏/Dock）
    pub const PANEL_DEEP: [u8; 3] = [28, 28, 34];
    /// 面板内凹区域
    pub const INSET: [u8; 3] = [34, 35, 42];
    /// 主文字
    pub const TEXT: [u8; 3] = [242, 242, 247];
    /// 次要文字
    pub const TEXT_DIM: [u8; 3] = [152, 154, 164];
    /// 唯一点缀色：静水青
    pub const ACCENT: [u8; 3] = [72, 192, 204];
    /// 发丝描边
    pub const HAIRLINE: [u8; 3] = [255, 255, 255];
    /// 窗控三色（克制版红绿灯）
    pub const CLOSE: [u8; 3] = [255, 96, 88];
    pub const MIN: [u8; 3] = [254, 188, 46];
    pub const ZOOM: [u8; 3] = [40, 200, 110];
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

/// 大而柔的多层投影（近似大半径高斯）。
/// 层数与半径是性能关键：每层都是一次全区域 SDF 扫描。
pub fn shadow(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, strength: f32) {
    for i in 0..5 {
        let grow = i as f32 * 5.0;
        let s = Rect {
            x: r.x - grow as i32,
            y: r.y - grow as i32 + 6,
            w: r.w + grow as i32 * 2,
            h: r.h + grow as i32 * 2,
        };
        rounded_rect(buf, w, h, s, radius + grow, [0, 0, 0], strength * (1.0 - i as f32 / 5.0) * 0.20);
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

/// 每帧的 UI 瞬态（由 main.rs 组装）。
pub struct UiState<'a> {
    pub snap: Option<Rect>,
    pub toast: Option<(&'a str, f32)>,
    pub ai_input: &'a str,
    pub ai_reply: Option<(&'a str, f32)>,
    pub mouse: (f32, f32),
    pub open_menu: Option<usize>,
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
            rounded_rect(buf, w, h, z, 12.0, palette::ACCENT, 0.08);
            rounded_outline(buf, w, h, z, 12.0, palette::ACCENT, 0.5);
        }

        for (i, win) in desktop.wins.iter().enumerate() {
            draw_window(buf, w, h, win.rect, win.title, i == desktop.active, t, tr);
        }

        self.draw_menubar(buf, w, h, ui, tr);
        if ui.open_menu.is_some() {
            self.draw_dropdown(buf, w, h, ui, tr);
        }
        if let Some((reply, age)) = ui.ai_reply {
            draw_reply(buf, w, h, reply, age, tr);
        }
        self.draw_ai_bar(buf, w, h, ui.ai_input, tr);

        let open_titles: Vec<&str> = desktop.wins.iter().map(|x| x.title).collect();
        self.draw_dock(buf, w, h, &open_titles, tr);

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
    let drift = t * 0.05;
    // 三道极低饱和的柔光：青（左上）、灰紫（右下）、暖灰（右上），缓慢漂移
    let washes: [(f32, f32, f32, f32, [u8; 3], f32); 3] = [
        (
            w as f32 * (0.20 + 0.02 * drift.sin()),
            h as f32 * 0.15,
            w as f32 * 0.50,
            h as f32 * 0.55,
            [38, 104, 112],
            0.35,
        ),
        (
            w as f32 * (0.80 - 0.02 * drift.cos()),
            h as f32 * 0.90,
            w as f32 * 0.55,
            h as f32 * 0.50,
            [82, 72, 128],
            0.30,
        ),
        (
            w as f32 * 0.88,
            h as f32 * 0.12,
            w as f32 * 0.35,
            h as f32 * 0.40,
            [110, 78, 84],
            0.14,
        ),
    ];

    for y in 0..h {
        let vgrad = y as f32 / h as f32;
        for x in 0..w {
            let mut acc = [0f32; 3];
            for c in 0..3 {
                acc[c] = lerp(palette::BG_TOP[c] as f32, palette::BG_BOTTOM[c] as f32, vgrad);
            }
            for (cx, cy, rx, ry, rgb, s) in &washes {
                let g = wash(x as f32, y as f32, *cx, *cy, *rx, *ry) * s;
                for c in 0..3 {
                    acc[c] += (rgb[c] as f32 - acc[c]) * g;
                }
            }
            // 轻微暗角
            let nx = x as f32 / w as f32 - 0.5;
            let ny = y as f32 / h as f32 - 0.5;
            let vig = 1.0 - (nx * nx + ny * ny) * 0.35;
            buf[y * w + x] = ((acc[0] * vig) as u32) << 16
                | ((acc[1] * vig) as u32) << 8
                | (acc[2] * vig) as u32;
        }
    }
}

// ---------------------------------------------------------------------------
// 菜单栏：发丝底 + 品牌 + 菜单（可点击）+ 搜索胶囊 + 电池 + 时钟
// ---------------------------------------------------------------------------

pub const MENUBAR_H: i32 = 32;

impl Renderer {
    fn draw_menubar(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let bar = Rect { x: 0, y: 0, w: w as i32, h: MENUBAR_H };
        fill_rect(buf, w, h, bar, palette::PANEL_DEEP, 0.72);
        fill_rect(buf, w, h, Rect { x: 0, y: MENUBAR_H - 1, w: w as i32, h: 1 }, palette::HAIRLINE, 0.10);

        let Some(tr) = tr else { return };

        // 品牌：小三角徽标 + 粗体 Aether
        let (bx, by) = (12i32, 8i32);
        for row in 0..16 {
            let half = row / 2;
            for col in 0..(half + 1) {
                let px = bx + half - col;
                let py = by + 15 - row;
                if px >= 0 && py >= 0 {
                    blend_pixel(buf, py as usize * w + px as usize, palette::ACCENT, 0.95);
                }
            }
        }
        let brand_x = 32.0;
        tr.draw_bold(buf, w, h, brand_x, 8.0, strings::BRAND, 14.0, palette::TEXT, 0.98);
        let mut mx = brand_x + tr.measure_bold(strings::BRAND, 14.0) + 18.0;

        // 菜单标签（登记命中区；打开中的菜单高亮）
        self.menubar_menus.clear();
        for (i, menu) in strings::MENUS.iter().enumerate() {
            let mw = tr.measure(menu, 13.0);
            let hit = Rect { x: mx as i32 - 8, y: 0, w: mw as i32 + 16, h: MENUBAR_H };
            let hovered = hit.contains(ui.mouse.0, ui.mouse.1);
            let opened = ui.open_menu == Some(i);
            if opened || hovered {
                rounded_rect(buf, w, h, Rect { x: hit.x + 2, y: 4, w: hit.w - 4, h: MENUBAR_H - 8 }, 6.0, palette::HAIRLINE, if opened { 0.14 } else { 0.07 });
            }
            draw_text(tr, buf, w, h, mx, 9.0, menu, 13.0, palette::TEXT, if opened { 1.0 } else { 0.85 });
            self.menubar_menus.push(hit);
            mx += mw + 16.0;
        }

        // 右侧：电池、AI 状态、搜索胶囊、时钟
        let clock = crate::text::clock_str();
        let clock_w = tr.measure_bold(&clock, 14.0);
        let clock_x = w as f32 - 16.0 - clock_w;
        tr.draw_bold(buf, w, h, clock_x, 8.0, &clock, 14.0, palette::TEXT, 0.95);

        let pill = Rect { x: clock_x as i32 - 208, y: 5, w: 192, h: 22 };
        rounded_rect(buf, w, h, pill, 11.0, palette::HAIRLINE, 0.08);
        rounded_outline(buf, w, h, pill, 11.0, palette::HAIRLINE, 0.14);
        draw_text(tr, buf, w, h, (pill.x + 12) as f32, 7.0, "搜索", 12.0, palette::TEXT_DIM, 0.8);
        let key = Rect { x: pill.x + pill.w - 22, y: 8, w: 16, h: 16 };
        rounded_rect(buf, w, h, key, 4.0, palette::HAIRLINE, 0.12);
        draw_text(tr, buf, w, h, (key.x + 4) as f32, 9.0, "K", 11.0, palette::TEXT_DIM, 0.85);
        self.search_pill = pill;

        let ai_w = tr.measure(strings::LOCAL_AI, 12.0);
        let ai_x = pill.x as f32 - 16.0 - ai_w;
        fill_rect(buf, w, h, Rect { x: ai_x as i32 - 12, y: 13, w: 6, h: 6 }, palette::ZOOM, 0.9);
        draw_text(tr, buf, w, h, ai_x, 9.0, strings::LOCAL_AI, 12.0, palette::TEXT_DIM, 0.85);

        let batt = Rect { x: ai_x as i32 - 56, y: 10, w: 26, h: 12 };
        rounded_outline(buf, w, h, batt, 3.5, palette::TEXT_DIM, 0.55);
        fill_rect(buf, w, h, Rect { x: batt.x + batt.w, y: 13, w: 2, h: 6 }, palette::TEXT_DIM, 0.55);
        fill_rect(buf, w, h, Rect { x: batt.x + 2, y: batt.y + 2, w: 16, h: 8 }, palette::ZOOM, 0.85);
    }

    /// 展开中的下拉菜单（登记各项命中区，悬停高亮）。
    fn draw_dropdown(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let Some(mi) = ui.open_menu else { return };
        let Some(label) = self.menubar_menus.get(mi) else { return };
        let Some(tr) = tr else { return };
        let items = strings::MENU_ITEMS[mi];
        let panel_w = items
            .iter()
            .map(|s| tr.measure(s, 13.0))
            .fold(120.0f32, f32::max)
            + 44.0;
        let panel = Rect { x: label.x, y: MENUBAR_H + 4, w: panel_w as i32, h: items.len() as i32 * 30 + 8 };
        shadow(buf, w, h, panel, 8.0, 0.9);
        rounded_rect(buf, w, h, panel, 10.0, palette::PANEL, 0.97);
        rounded_outline(buf, w, h, panel, 10.0, palette::HAIRLINE, 0.12);

        let mut rects = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let ir = Rect { x: panel.x + 4, y: panel.y + 4 + i as i32 * 30, w: panel.w - 8, h: 30 };
            if ir.contains(ui.mouse.0, ui.mouse.1) {
                rounded_rect(buf, w, h, Rect { x: ir.x + 4, y: ir.y + 3, w: ir.w - 8, h: ir.h - 6 }, 6.0, palette::ACCENT, 0.22);
            }
            draw_text(tr, buf, w, h, (ir.x + 16) as f32, (ir.y + 8) as f32, item, 13.0, palette::TEXT, 0.92);
            rects.push(ir);
        }
        self.dropdown = Some((rects, items.to_vec()));
    }
}

// ---------------------------------------------------------------------------
// 窗口：磨砂深灰 + 发丝描边 + 红绿灯（左）+ 居中标题
// ---------------------------------------------------------------------------

const TITLE_H: i32 = 36;

fn draw_window(buf: &mut [u32], w: usize, h: usize, r: Rect, title: &str, active: bool, t: f32, tr: Option<&TextRenderer>) {
    shadow(buf, w, h, r, 14.0, if active { 1.0 } else { 0.55 });
    rounded_rect(buf, w, h, r, 12.0, palette::PANEL, 0.86);
    // 顶部内高光（玻璃厚度感）
    fill_rect(buf, w, h, Rect { x: r.x + 8, y: r.y + 1, w: r.w - 16, h: 1 }, palette::HAIRLINE, 0.07);
    rounded_outline(buf, w, h, r, 12.0, palette::HAIRLINE, if active { 0.16 } else { 0.09 });

    // 标题栏分隔发丝线
    fill_rect(buf, w, h, Rect { x: r.x + 1, y: r.y + TITLE_H, w: r.w - 2, h: 1 }, palette::HAIRLINE, 0.07);

    // 红绿灯（左）
    let lights = [palette::CLOSE, palette::MIN, palette::ZOOM];
    for (i, c) in lights.iter().enumerate() {
        let dot = Rect { x: r.x + 14 + (i as i32) * 20, y: r.y + 12, w: 12, h: 12 };
        rounded_rect(buf, w, h, dot, 6.0, *c, if active { 0.95 } else { 0.30 });
    }

    // 居中标题（粗体、层级分明）
    if let Some(tr) = tr {
        let tw = tr.measure_bold(title, 13.0);
        let tx = r.x as f32 + r.w as f32 / 2.0 - tw / 2.0;
        tr.draw_bold(buf, w, h, tx, (r.y + 10) as f32, title, 13.0, palette::TEXT, if active { 0.9 } else { 0.4 });
    }

    let content = Rect { x: r.x + 1, y: r.y + TITLE_H + 1, w: r.w - 2, h: r.h - TITLE_H - 2 };
    if title == strings::WIN_TERM {
        draw_term_content(buf, w, h, content, t, tr);
    } else {
        draw_files_content(buf, w, h, content, tr);
    }
}

fn draw_term_content(buf: &mut [u32], w: usize, h: usize, r: Rect, t: f32, tr: Option<&TextRenderer>) {
    fill_rect(buf, w, h, r, palette::INSET, 0.9);
    let Some(tr) = tr else { return };
    let lines = [
        ("aether@localhost ~ $", palette::ACCENT, 0.95),
        ("uname -a", palette::TEXT, 0.9),
        ("AetherOS 0.1.0 aether-kernel x86_64", palette::TEXT_DIM, 0.85),
        ("aether@localhost ~ $", palette::ACCENT, 0.95),
    ];
    let mut y = r.y + 12;
    for (line, rgb, a) in lines {
        draw_text(tr, buf, w, h, (r.x + 14) as f32, y as f32, line, 13.0, rgb, a);
        y += 23;
    }
    if (t * 2.0) as i32 % 2 == 0 {
        fill_rect(buf, w, h, Rect { x: r.x + 14, y: y + 2, w: 8, h: 14 }, palette::TEXT, 0.7);
    }
}

fn draw_files_content(buf: &mut [u32], w: usize, h: usize, r: Rect, tr: Option<&TextRenderer>) {
    // 侧栏
    let sidebar = Rect { x: r.x, y: r.y, w: 138, h: r.h };
    fill_rect(buf, w, h, sidebar, palette::PANEL_DEEP, 0.45);
    fill_rect(buf, w, h, Rect { x: sidebar.x + sidebar.w, y: r.y, w: 1, h: r.h }, palette::HAIRLINE, 0.06);
    if let Some(tr) = tr {
        for (i, item) in ["文档", "图片", "音乐", "项目"].iter().enumerate() {
            let y = r.y + 14 + i as i32 * 34;
            let selected = i == 0;
            if selected {
                rounded_rect(buf, w, h, Rect { x: r.x + 8, y: y - 5, w: 122, h: 28 }, 6.0, palette::ACCENT, 0.20);
            }
            draw_text(tr, buf, w, h, (r.x + 20) as f32, y as f32, item, 13.0, if selected { palette::TEXT } else { palette::TEXT_DIM }, if selected { 0.95 } else { 0.75 });
        }
    }
    // 文件卡片网格
    let grid_x = r.x + 152;
    for row in 0..2 {
        for col in 0..4 {
            let card = Rect { x: grid_x + col * 98, y: r.y + 14 + row * 98, w: 86, h: 86 };
            if card.w <= 0 || card.x + card.w > r.x + r.w {
                continue;
            }
            rounded_rect(buf, w, h, card, 10.0, palette::PANEL_DEEP, 0.55);
            rounded_outline(buf, w, h, card, 10.0, palette::HAIRLINE, 0.07);
            gradient_tile(buf, w, h, Rect { x: card.x + 21, y: card.y + 14, w: 44, h: 34 }, 7.0, [58, 120, 210], [44, 92, 180], 0.85);
        }
    }
}

// ---------------------------------------------------------------------------
// AI 指令条：悬浮胶囊（Spotlight 气质），可真实输入
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_ai_bar(&mut self, buf: &mut [u32], w: usize, h: usize, input: &str, tr: Option<&TextRenderer>) {
    let bar_w = 560i32;
    let bar = Rect { x: w as i32 / 2 - bar_w / 2, y: h as i32 - crate::layout::BOTTOM_DOCK - 52 - 16, w: bar_w, h: 52 };
    shadow(buf, w, h, bar, 12.0, 0.9);
    rounded_rect(buf, w, h, bar, 26.0, palette::PANEL_DEEP, 0.78);
    rounded_outline(buf, w, h, bar, 26.0, palette::HAIRLINE, 0.14);

    let Some(tr) = tr else { return };

    // 渐变圆形徽标
    let av = Rect { x: bar.x + 12, y: bar.y + 10, w: 32, h: 32 };
    gradient_tile(buf, w, h, av, 16.0, palette::ACCENT, [86, 84, 168], 0.95);
    tr.draw_bold(buf, w, h, (av.x + 10) as f32, (av.y + 7) as f32, "A", 16.0, palette::TEXT, 0.98);

    let text_x = (bar.x + 58) as f32;
    let text_y = (bar.y + 15) as f32;
    if input.is_empty() {
        draw_text(tr, buf, w, h, text_x, text_y, strings::AI_BAR_HINT, 14.0, palette::TEXT_DIM, 0.85);
    } else {
        // 从左截断，保证光标所在的内容始终可见
        let max_w = bar.x as f32 + bar.w as f32 - 96.0 - text_x;
        let mut shown: String = input.to_string();
        while tr.measure(&shown, 14.0) > max_w && shown.chars().count() > 1 {
            shown.remove(0);
        }
        draw_text(tr, buf, w, h, text_x, text_y, &shown, 14.0, palette::TEXT, 0.95);
    }

    // 输入光标
    let caret_x = text_x + tr.measure(if input.is_empty() { "" } else { input_tail_visible(tr, input, text_x, bar.x as f32 + bar.w as f32 - 96.0) }, 14.0) + 4.0;
    if ((t_now() * 2.0) as i32) % 2 == 0 {
        fill_rect(buf, w, h, Rect { x: caret_x as i32, y: bar.y + 14, w: 2, h: 20 }, palette::ACCENT, 0.9);
    }

    // 右侧快捷键胶囊
    let hint = "Enter";
    let hw = tr.measure(hint, 11.0) + 12.0;
    let key = Rect { x: bar.x + bar.w - hw as i32 - 12, y: bar.y + 15, w: hw as i32, h: 22 };
    rounded_rect(buf, w, h, key, 6.0, palette::HAIRLINE, 0.10);
    draw_text(tr, buf, w, h, (key.x + 6) as f32, (key.y + 4) as f32, hint, 11.0, palette::TEXT_DIM, 0.8);
    }
}

fn input_tail_visible<'a>(tr: &TextRenderer, input: &'a str, x: f32, max_right: f32) -> &'a str {
    let mut s = input;
    while tr.measure(s, 14.0) > max_right - x {
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

/// AI 的回复气泡：出现在指令条上方。
fn draw_reply(buf: &mut [u32], w: usize, h: usize, text: &str, age: f32, tr: Option<&TextRenderer>) {
    let Some(tr) = tr else { return };
    let alpha = ((6.0 - age) / 0.4).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let max_w = 720.0f32;
    let tw = tr.measure(text, 14.0).min(max_w);
    let r = Rect {
        x: w as i32 / 2 - (tw + 36.0) as i32 / 2,
        y: h as i32 - crate::layout::BOTTOM_DOCK - 52 - 16 - 56,
        w: (tw + 36.0) as i32,
        h: 36,
    };
    shadow(buf, w, h, r, 10.0, alpha * 0.9);
    rounded_rect(buf, w, h, r, 18.0, palette::PANEL_DEEP, alpha * 0.92);
    rounded_outline(buf, w, h, r, 18.0, palette::ACCENT, alpha * 0.4);
    // 左截断显示
    let mut shown: String = text.to_string();
    while tr.measure(&shown, 14.0) > max_w && shown.chars().count() > 1 {
        shown.remove(0);
    }
    draw_text(tr, buf, w, h, (r.x + 18) as f32, (r.y + 9) as f32, &shown, 14.0, palette::TEXT, alpha);
}

// ---------------------------------------------------------------------------
// Dock：磨砂托盘 + 渐变图标 + 运行指示点
// ---------------------------------------------------------------------------

const DOCK_ICON: i32 = 46;
const DOCK_PAD: i32 = 10;

impl Renderer {
    /// Dock：磨砂托盘 + 渐变图标 + 运行指示点（真实反映已打开窗口）。
    fn draw_dock(&mut self, buf: &mut [u32], w: usize, h: usize, open_titles: &[&str], tr: Option<&TextRenderer>) {
        let apps: [(&str, [u8; 3], [u8; 3]); 5] = [
            (strings::WIN_FILES, [64, 150, 235], [42, 108, 205]),
            (strings::WIN_TERM, [62, 62, 72], [36, 36, 44]),
            (strings::WIN_BROWSER, [64, 200, 208], [36, 152, 168]),
            (strings::WIN_MUSIC, [255, 122, 150], [225, 85, 125]),
            (strings::WIN_SETTINGS, [128, 132, 142], [92, 96, 106]),
        ];
        let n = apps.len() as i32;
        let tray_w = n * DOCK_ICON + (n + 1) * DOCK_PAD;
        let tray_h = DOCK_ICON + 2 * DOCK_PAD;
        let tray = Rect { x: w as i32 / 2 - tray_w / 2, y: h as i32 - crate::layout::BOTTOM_DOCK, w: tray_w, h: tray_h };
        shadow(buf, w, h, tray, 10.0, 0.8);
        rounded_rect(buf, w, h, tray, 20.0, palette::PANEL_DEEP, 0.60);
        rounded_outline(buf, w, h, tray, 20.0, palette::HAIRLINE, 0.12);

        self.dock_icons.clear();
        for (i, (name, top, bottom)) in apps.iter().enumerate() {
            let ix = tray.x + DOCK_PAD + i as i32 * (DOCK_ICON + DOCK_PAD);
            let iy = tray.y + DOCK_PAD;
            let tile = Rect { x: ix, y: iy, w: DOCK_ICON, h: DOCK_ICON };
            self.dock_icons.push(tile);
            gradient_tile(buf, w, h, tile, 11.0, *top, *bottom, 0.95);
            rounded_outline(buf, w, h, tile, 11.0, palette::HAIRLINE, 0.18);

            // 图标 glyph
            match i {
                0 => {
                    fill_rect(buf, w, h, Rect { x: ix + 10, y: iy + 14, w: 14, h: 6 }, palette::TEXT, 0.95);
                    rounded_rect(buf, w, h, Rect { x: ix + 10, y: iy + 18, w: 26, h: 16 }, 3.0, palette::TEXT, 0.95);
                }
                1 => {
                    if let Some(tr) = tr {
                        tr.draw_bold(buf, w, h, (ix + 8) as f32, (iy + 12) as f32, ">_", 15.0, palette::TEXT, 0.95);
                    }
                }
                2 => {
                    rounded_outline(buf, w, h, Rect { x: ix + 8, y: iy + 8, w: 30, h: 30 }, 15.0, palette::TEXT, 0.95);
                    fill_rect(buf, w, h, Rect { x: ix + 20, y: iy + 20, w: 6, h: 6 }, palette::TEXT, 0.95);
                }
                3 => {
                    fill_rect(buf, w, h, Rect { x: ix + 26, y: iy + 12, w: 3, h: 20 }, palette::TEXT, 0.95);
                    rounded_rect(buf, w, h, Rect { x: ix + 16, y: iy + 26, w: 13, h: 10 }, 5.0, palette::TEXT, 0.95);
                }
                _ => {
                    rounded_outline(buf, w, h, Rect { x: ix + 10, y: iy + 10, w: 26, h: 26 }, 13.0, palette::TEXT, 0.95);
                    fill_rect(buf, w, h, Rect { x: ix + 21, y: iy + 21, w: 4, h: 4 }, palette::TEXT, 0.95);
                    for (dx, dy) in [(0, -14), (0, 12), (-14, 0), (12, 0)] {
                        fill_rect(buf, w, h, Rect { x: ix + 22 + dx, y: iy + 21 + dy, w: 3, h: 3 }, palette::TEXT, 0.8);
                    }
                }
            }

            // 运行指示点：真实反映打开的窗口
            if open_titles.contains(name) {
                fill_rect(buf, w, h, Rect { x: ix + DOCK_ICON / 2 - 2, y: iy + DOCK_ICON + 5, w: 4, h: 4 }, palette::TEXT, 0.85);
            }
        }
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
    let tw = tr.measure(msg, 14.0) + 40.0;
    let r = Rect {
        x: w as i32 / 2 - tw as i32 / 2,
        y: h as i32 - crate::layout::BOTTOM_DOCK - 52 - 16 - 60,
        w: tw as i32,
        h: 40,
    };
    shadow(buf, w, h, r, 8.0, alpha);
    rounded_rect(buf, w, h, r, 20.0, palette::PANEL_DEEP, alpha * 0.92);
    rounded_outline(buf, w, h, r, 20.0, palette::HAIRLINE, alpha * 0.15);
    fill_rect(buf, w, h, Rect { x: r.x + 18, y: r.y + 17, w: 6, h: 6 }, palette::ACCENT, alpha);
    draw_text(tr, buf, w, h, (r.x + 34) as f32, (r.y + 12) as f32, msg, 14.0, palette::TEXT, alpha);
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
