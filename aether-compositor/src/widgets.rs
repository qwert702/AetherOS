//! 控件原语（P1）：把反复手写的"按钮/开关/滑杆/分段/列表行/滚动条/分组标题/徽标"
//! 收成一个模块，交互状态统一为六态。
//!
//! ## 为什么要有这一层
//!
//! 调研（2026-09-29）结论：UI 里的控件几乎为零 —— 开关/滑杆/分段/滚动条/焦点环
//! **一个都不存在**，全是各处在 `render_frame` 里手画的矩形。设置中心需要十几个控件，
//! 再手画一遍必然风格漂移，所以先把原语抽出来。
//!
//! ## 设计约束（与既有架构一致，不引入新范式）
//!
//! - **纯函数**：只拿 `buf` 与几何，**不持有状态**；状态由调用方（`Desktop`/`UiState`）持有。
//! - **命中区由调用方登记**：这里只算几何与绘制；判定统一走 [`HitTable`]，
//!   避免两套事件循环各写一遍命中逻辑（这是本仓库已踩过的坑）。
//! - **颜色/圆角/间距一律取 `theme::*` 令牌**，禁止硬编码 —— P0 的教训是硬编码会让
//!   整体视觉失控（壁纸/强调色就是绕过令牌长出来的）。
//! - 不引入第三方依赖，不用图片资源；图标继续走几何绘制。

// P1 把原语先立起来：除 `scroll_apply`（已接滚轮）外，其余控件要等设置中心（P3）
// 才有调用点。整个模块暂时允许 dead_code —— **P3 完成后必须删掉这一行**，
// 否则就变成"永久挂账"。
#![allow(dead_code)]

use crate::draw::{
    blend_pixel, fill_rect, rounded_outline, rounded_rect,
    theme::{color, font, radius, state},
    Rect,
};
use crate::text::TextRenderer;

/// 滚轮一格滚动多少"行"。
pub const WHEEL_LINES: usize = 3;

/// 控件交互状态（六态）。用链式构造，调用点读起来接近自然语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct States {
    pub hover: bool,
    pub pressed: bool,
    pub focus: bool,
    pub disabled: bool,
    pub selected: bool,
}

impl States {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn hover(mut self) -> Self {
        self.hover = true;
        self
    }
    pub fn pressed(mut self) -> Self {
        self.pressed = true;
        self
    }
    pub fn focus(mut self) -> Self {
        self.focus = true;
        self
    }
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }
    pub fn selected(mut self) -> Self {
        self.selected = true;
        self
    }

    /// 由鼠标位置推 hover（最常用的一档）。
    pub fn at(r: Rect, mouse: (f32, f32)) -> Self {
        Self {
            hover: r.contains(mouse.0, mouse.1),
            ..Self::default()
        }
    }

    /// 同上，但**禁用态不显示 hover** —— 否则用户会以为还能点。
    pub fn at_maybe_disabled(r: Rect, mouse: (f32, f32), disabled: bool) -> Self {
        Self {
            hover: !disabled && r.contains(mouse.0, mouse.1),
            disabled,
            ..Self::default()
        }
    }
}

/// 按钮语义色。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BtnKind {
    /// 主操作（放行、安装、应用）
    Primary,
    /// 次要操作（取消、返回）
    Secondary,
    /// 危险操作（拒绝、删除）
    Danger,
}

/// 按钮。返回命中区（调用方拿去登记或直接判定）。
pub fn button(
    buf: &mut [u32],
    w: usize,
    h: usize,
    r: Rect,
    label: &str,
    kind: BtnKind,
    st: States,
    tr: Option<&TextRenderer>,
) -> Rect {
    let (bg, fg) = match kind {
        BtnKind::Primary => (color::accent_strong(), color::HIGHLIGHT),
        BtnKind::Secondary => (color::surface_3(), color::text()),
        BtnKind::Danger => (color::danger(), color::HIGHLIGHT),
    };
    let body_alpha = if st.disabled { state::DISABLED } else { 1.0 };
    rounded_rect(buf, w, h, r, radius::SM, bg, body_alpha);
    if st.hover && !st.disabled {
        rounded_rect(buf, w, h, r, radius::SM, color::hairline(), state::hover());
    }
    if st.pressed && !st.disabled {
        rounded_rect(buf, w, h, r, radius::SM, color::hairline(), state::pressed());
    }
    // 焦点环：键盘导航必须看得见（此前全库没有焦点态）
    rounded_outline(buf, w, h, r, radius::SM, color::hairline(), if st.focus { 0.45 } else { 0.14 });
    if let Some(tr) = tr {
        let tw = tr.measure(label, font::BODY);
        let x = r.x as f32 + (r.w as f32 - tw) / 2.0;
        let y = tr.vcenter(r.y as f32, r.h as f32, font::BODY);
        tr.draw(buf, w, h, x, y, label, font::BODY, fg, if st.disabled { 0.55 } else { 0.98 });
    }
    r
}

/// 开关（Win11 风格胶囊）。返回命中区。
pub fn switch(buf: &mut [u32], w: usize, h: usize, r: Rect, on: bool, st: States) -> Rect {
    let pill = r.h as f32 / 2.0;
    let track = if on { color::accent() } else { color::surface_3() };
    rounded_rect(buf, w, h, r, pill, track, if st.disabled { state::DISABLED } else { 1.0 });
    if st.hover && !st.disabled {
        rounded_rect(buf, w, h, r, pill, color::hairline(), state::hover());
    }
    rounded_outline(buf, w, h, r, pill, color::hairline(), if st.focus { 0.45 } else { 0.20 });
    let kr = (r.h as f32 - 6.0) / 2.0;
    let kx = if on {
        r.x as f32 + r.w as f32 - kr - 3.0
    } else {
        r.x as f32 + kr + 3.0
    };
    let ky = r.y as f32 + r.h as f32 / 2.0;
    circle(buf, w, h, kx, ky, kr, color::HIGHLIGHT, if st.disabled { 0.75 } else { 1.0 });
    r
}

/// 滑杆。`value` ∈ [0,1]。返回命中区。
pub fn slider(buf: &mut [u32], w: usize, h: usize, r: Rect, value: f32, st: States) -> Rect {
    let cy = r.y + r.h / 2;
    let v = value.clamp(0.0, 1.0);
    let track = Rect {
        x: r.x,
        y: cy - 2,
        w: r.w,
        h: 4,
    };
    rounded_rect(buf, w, h, track, 2.0, color::surface_3(), if st.disabled { state::DISABLED } else { 1.0 });
    let fill_w = (r.w as f32 * v) as i32;
    if fill_w > 0 {
        rounded_rect(
            buf,
            w,
            h,
            Rect {
                x: r.x,
                y: cy - 2,
                w: fill_w,
                h: 4,
            },
            2.0,
            color::accent(),
            if st.disabled { state::DISABLED } else { 1.0 },
        );
    }
    let kr = if st.hover || st.pressed { 7.5 } else { 6.5 };
    let kx = r.x as f32 + r.w as f32 * v;
    circle(buf, w, h, kx, cy as f32 + 0.5, kr, color::surface_2(), 1.0);
    circle(buf, w, h, kx, cy as f32 + 0.5, kr, color::hairline(), 0.18);
    r
}

/// 由鼠标 x 反算滑杆值（拖动时用）。纯函数，便于测试。
pub fn slider_value_at(r: Rect, x: f32) -> f32 {
    if r.w <= 0 {
        return 0.0;
    }
    ((x - r.x as f32) / r.w as f32).clamp(0.0, 1.0)
}

/// 分段控件（Win11 Segmented）。返回每段的命中区。
pub fn segmented(
    buf: &mut [u32],
    w: usize,
    h: usize,
    r: Rect,
    labels: &[&str],
    active: usize,
    mouse: (f32, f32),
    tr: Option<&TextRenderer>,
) -> Vec<Rect> {
    let n = labels.len().max(1) as i32;
    let seg_w = r.w / n;
    let mut out = Vec::with_capacity(n as usize);
    rounded_rect(buf, w, h, r, radius::SM, color::surface_3(), 0.7);
    for (i, label) in labels.iter().enumerate() {
        let idx = i as i32;
        let sr = Rect {
            x: r.x + idx * seg_w,
            y: r.y,
            w: seg_w,
            h: r.h,
        };
        out.push(sr);
        if i == active {
            rounded_rect(buf, w, h, sr, radius::SM, color::surface_2(), 1.0);
            rounded_outline(buf, w, h, sr, radius::SM, color::hairline(), 0.16);
        } else if sr.contains(mouse.0, mouse.1) {
            rounded_rect(buf, w, h, sr, radius::SM, color::hairline(), state::hover());
        }
        if let Some(tr) = tr {
            let tw = tr.measure(label, font::CAPTION);
            let x = sr.x as f32 + (seg_w as f32 - tw) / 2.0;
            let y = tr.vcenter(sr.y as f32, sr.h as f32, font::CAPTION);
            let (c, a) = if i == active {
                (color::text(), 0.98)
            } else {
                (color::text_dim(), 0.9)
            };
            tr.draw(buf, w, h, x, y, label, font::CAPTION, c, a);
        }
    }
    out
}

/// 设置项行：标题 +（可选）副标题；右侧留给开关/下拉/按钮。
/// 选中态带左侧 3px 色条（沿用文件管理器侧栏的既有语言）。
pub fn row(
    buf: &mut [u32],
    w: usize,
    h: usize,
    r: Rect,
    title: &str,
    subtitle: Option<&str>,
    st: States,
    tr: Option<&TextRenderer>,
) -> Rect {
    if st.selected {
        rounded_rect(buf, w, h, r, radius::SM, color::accent(), 0.12);
        fill_rect(
            buf,
            w,
            h,
            Rect {
                x: r.x,
                y: r.y + 3,
                w: 3,
                h: (r.h - 6).max(0),
            },
            color::accent(),
            0.9,
        );
    } else if st.hover && !st.disabled {
        rounded_rect(buf, w, h, r, radius::SM, color::hairline(), state::hover());
    }
    if let Some(tr) = tr {
        let (ty, sy) = if subtitle.is_some() {
            (
                tr.vcenter(r.y as f32, r.h as f32 * 0.56, font::BODY) - 1.0,
                r.y as f32 + r.h as f32 * 0.56,
            )
        } else {
            (
                tr.vcenter(r.y as f32, r.h as f32, font::BODY),
                r.y as f32 + r.h as f32,
            )
        };
        tr.draw(
            buf,
            w,
            h,
            (r.x + 12) as f32,
            ty,
            title,
            font::BODY,
            color::text(),
            if st.disabled { 0.5 } else { 0.95 },
        );
        if let Some(sub) = subtitle {
            tr.draw(
                buf,
                w,
                h,
                (r.x + 12) as f32,
                sy,
                sub,
                font::CAPTION,
                color::text_dim(),
                if st.disabled { 0.4 } else { 0.85 },
            );
        }
    }
    r
}

/// 分组标题（设置页的小节标题）。
pub fn group_header(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, text: &str, tr: Option<&TextRenderer>) {
    if let Some(tr) = tr {
        tr.draw(buf, w, h, x as f32, y as f32, text, font::CAPTION, color::text_dim(), 0.85);
    }
}

/// 1px 分隔线。
pub fn divider(buf: &mut [u32], w: usize, h: usize, r: Rect) {
    fill_rect(
        buf,
        w,
        h,
        Rect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: 1,
        },
        color::hairline(),
        0.10,
    );
}

/// 徽标（状态/计数）。返回命中区。
pub fn badge(
    buf: &mut [u32],
    w: usize,
    h: usize,
    x: i32,
    y: i32,
    text: &str,
    rgb: [u8; 3],
    tr: Option<&TextRenderer>,
) -> Rect {
    let Some(tr) = tr else {
        return Rect { x, y, w: 0, h: 0 };
    };
    let tw = tr.measure(text, font::LABEL);
    let r = Rect {
        x,
        y,
        w: tw as i32 + 14,
        h: 18,
    };
    rounded_rect(buf, w, h, r, 9.0, rgb, 0.16);
    rounded_outline(buf, w, h, r, 9.0, rgb, 0.40);
    tr.draw(
        buf,
        w,
        h,
        (x + 7) as f32,
        tr.vcenter(y as f32, 18.0, font::LABEL),
        text,
        font::LABEL,
        rgb,
        0.95,
    );
    r
}

/// 滚动条。返回（轨道, 滑块）。`visible`/`total` 是"可见项数 / 总项数"。
///
/// 内容装得下时滑块高度为 0（调用方据此不画/不命中），与 Win11 的自动隐藏一致。
pub fn scrollbar(
    buf: &mut [u32],
    w: usize,
    h: usize,
    track: Rect,
    total: usize,
    visible: usize,
    offset: usize,
) -> (Rect, Rect) {
    if visible == 0 || total <= visible || track.h <= 4 || track.w <= 0 {
        return (track, Rect { x: track.x, y: track.y, w: track.w, h: 0 });
    }
    rounded_rect(buf, w, h, track, track.w as f32 / 2.0, color::surface_3(), 0.55);
    let frac = visible as f32 / total as f32;
    let knob_h = (track.h as f32 * frac).max(20.0).min(track.h as f32);
    let max_off = (total - visible) as f32;
    let t = if max_off <= 0.0 {
        0.0
    } else {
        (offset as f32 / max_off).clamp(0.0, 1.0)
    };
    let ky = track.y as f32 + (track.h as f32 - knob_h) * t;
    let knob = Rect {
        x: track.x,
        y: ky as i32,
        w: track.w,
        h: knob_h as i32,
    };
    rounded_rect(buf, w, h, knob, track.w as f32 / 2.0, color::text_dim(), 0.75);
    (track, knob)
}

/// 滚轮/拖动后的新 offset。**`dy > 0`（向上）→ 往列表开头走**。
///
/// 纯函数：夹取逻辑只有这一份，两个事件循环都调它（此前这类夹取散在多处）。
pub fn scroll_apply(offset: usize, dy: i32, total: usize, visible: usize, lines: usize) -> usize {
    let max_off = total.saturating_sub(visible) as i64;
    let next = offset as i64 - dy as i64 * lines as i64;
    next.clamp(0, max_off) as usize
}

/// 实心圆。`draw.rs` 只有圆角矩形，圆点（开关滑块/滑杆手柄）此前各处手画。
fn circle(buf: &mut [u32], w: usize, h: usize, cx: f32, cy: f32, r: f32, rgb: [u8; 3], alpha: f32) {
    let x0 = (cx - r - 1.0).max(0.0) as usize;
    let x1 = (cx + r + 1.0).max(0.0) as usize;
    let y0 = (cy - r - 1.0).max(0.0) as usize;
    let y1 = (cy + r + 1.0).max(0.0) as usize;
    for y in y0..=y1.min(h.saturating_sub(1)) {
        for x in x0..=x1.min(w.saturating_sub(1)) {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (r - d + 0.5).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_pixel(buf, y * w + x, rgb, alpha * cov);
            }
        }
    }
}

/// 本帧登记的命中表：**后登记（更晚绘制 = 更上层）优先**。
///
/// 为什么要它：现在两个事件循环各自用一堆 `*_hits: Vec<Rect>` 字段判命中，
/// 顺序即优先级、插错位置会遮住已有控件。新控件统一登记到一张表，判定逻辑只有一份。
#[derive(Default)]
pub struct HitTable {
    entries: Vec<(Rect, u32)>,
}

impl HitTable {
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn push(&mut self, r: Rect, id: u32) {
        self.entries.push((r, id));
    }
    pub fn hit(&self, mouse: (f32, f32)) -> Option<u32> {
        self.entries
            .iter()
            .rev()
            .find(|(r, _)| r.contains(mouse.0, mouse.1))
            .map(|(_, id)| *id)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> Vec<u32> {
        vec![0u32; 400 * 200]
    }

    #[test]
    fn scroll_apply_clamps_both_ends() {
        // 100 项、可见 10 → offset 上限 90
        assert_eq!(scroll_apply(0, 1, 100, 10, 3), 0, "已在顶部，向上不越界");
        assert_eq!(scroll_apply(0, -1, 100, 10, 3), 3, "向下滚 3 行");
        assert_eq!(scroll_apply(89, -5, 100, 10, 3), 90, "底部夹取");
        assert_eq!(scroll_apply(90, 1, 100, 10, 3), 87, "向上滚");
        // 内容装得下 → 永远 0
        assert_eq!(scroll_apply(0, -3, 5, 10, 3), 0);
        assert_eq!(scroll_apply(7, 9, 3, 10, 3), 0);
    }

    #[test]
    fn slider_value_maps_endpoints() {
        let r = Rect { x: 100, y: 0, w: 200, h: 20 };
        assert_eq!(slider_value_at(r, 100.0), 0.0);
        assert_eq!(slider_value_at(r, 300.0), 1.0);
        assert!((slider_value_at(r, 200.0) - 0.5).abs() < 1e-6);
        assert_eq!(slider_value_at(r, -50.0), 0.0, "越界夹取");
        assert_eq!(slider_value_at(r, 999.0), 1.0);
        // 退化矩形不能除零
        assert_eq!(slider_value_at(Rect { x: 0, y: 0, w: 0, h: 0 }, 5.0), 0.0);
    }

    #[test]
    fn hit_table_prefers_topmost() {
        let mut t = HitTable::default();
        t.push(Rect { x: 0, y: 0, w: 100, h: 100 }, 1);
        t.push(Rect { x: 50, y: 50, w: 100, h: 100 }, 2);
        assert_eq!(t.hit((10.0, 10.0)), Some(1));
        assert_eq!(t.hit((60.0, 60.0)), Some(2), "重叠处应命中后登记（更上层）的");
        assert_eq!(t.hit((300.0, 300.0)), None);
        assert_eq!(t.len(), 2);
        t.clear();
        assert!(t.is_empty());
    }

    #[test]
    fn disabled_suppresses_hover() {
        let r = Rect { x: 0, y: 0, w: 10, h: 10 };
        assert!(States::at(r, (5.0, 5.0)).hover);
        assert!(!States::at(r, (50.0, 5.0)).hover);
        assert!(
            !States::at_maybe_disabled(r, (5.0, 5.0), true).hover,
            "禁用控件不该显示 hover"
        );
        assert!(States::at_maybe_disabled(r, (5.0, 5.0), false).hover);
        assert!(States::none().disabled == false);
    }

    #[test]
    fn segmented_splits_evenly_and_covers() {
        let mut b = buf();
        let r = Rect { x: 10, y: 10, w: 301, h: 24 };
        let segs = segmented(&mut b, 400, 200, r, &["浅色", "深色", "跟随"], 0, (0.0, 0.0), None);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].x, 10);
        assert_eq!(segs[0].w, 100, "301/3 向下取整");
        // 三段首尾相接、不超出（最后一段可能少 1px，属预期）
        assert_eq!(segs[1].x, segs[0].x + segs[0].w);
        assert!(segs[2].x + segs[2].w <= r.x + r.w);
        // 空标签不 panic
        assert!(segmented(&mut b, 400, 200, r, &[], 0, (0.0, 0.0), None).is_empty());
    }

    #[test]
    fn scrollbar_hides_when_content_fits() {
        let mut b = buf();
        let track = Rect { x: 390, y: 10, w: 4, h: 180 };
        let (_, knob) = scrollbar(&mut b, 400, 200, track, 10, 10, 0);
        assert_eq!(knob.h, 0, "装得下时不显示滑块");
        let (_, knob) = scrollbar(&mut b, 400, 200, track, 100, 10, 0);
        assert!(knob.h >= 20, "滑块有最小高度，太短抓不住");
        assert_eq!(knob.y, track.y, "在顶部时滑块贴顶");
        let (_, knob_bottom) = scrollbar(&mut b, 400, 200, track, 100, 10, 90);
        assert!(knob_bottom.y > knob.y, "滚到底时滑块下移");
    }

    #[test]
    fn widgets_draw_without_panicking_on_degenerate_rects() {
        // 外部输入（窗口被拖到极小）会给出 0/负尺寸矩形：控件必须安全降级
        let mut b = buf();
        let zeros = Rect { x: 0, y: 0, w: 0, h: 0 };
        let neg = Rect { x: 5, y: 5, w: -20, h: -20 };
        for r in [zeros, neg] {
            let st = States::at(r, (1.0, 1.0));
            button(&mut b, 400, 200, r, "确定", BtnKind::Primary, st, None);
            switch(&mut b, 400, 200, r, true, st);
            slider(&mut b, 400, 200, r, 0.5, st);
            segmented(&mut b, 400, 200, r, &["a", "b"], 1, (0.0, 0.0), None);
            row(&mut b, 400, 200, r, "标题", Some("副标题"), st, None);
            divider(&mut b, 400, 200, r);
            let _ = badge(&mut b, 400, 200, 0, 0, "x", color::accent(), None);
            let _ = scrollbar(&mut b, 400, 200, r, 100, 10, 5);
        }
    }
}
