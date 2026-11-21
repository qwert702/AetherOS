//! 窗口与内容绘制：窗口骨架（标题栏/边框/缩放提示）、终端/音乐/文件/预览/Wayland 内容面、图标网格与布局几何。
use super::*;

/// 窗口几何与激活态的指纹（FNV-1a，无依赖）：变了才重算投影烘焙层。
pub fn desktop_key(d: &Desktop) -> u64 {
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
pub fn draw_window(
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

/// 音乐窗口内容：左侧曲目列表 + 右侧"正在播放"面板。
///
/// 存在的意义是"内容形态跟着窗口语义走"——满窗一模一样的文件夹图标，
/// 是"演示占位"最刺眼的信号。窗口够宽时右挂播放面板，窄时退化为纯列表。
pub fn draw_music_content(buf: &mut [u32], w: usize, h: usize, r: Rect, win: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>,
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

/// 终端内容：提示符分色 + 命令与输出 + 末尾光标。
/// 密度做足——空荡的终端窗口看起来像没做完。
pub fn draw_term_content(
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
pub fn draw_files_content(
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
pub(crate) const GRID_ICON: i32 = 46;
pub(crate) const GRID_CELL_W: i32 = 96;
pub(crate) const GRID_CELL_H: i32 = 84;
pub(crate) const GRID_GAP_X: i32 = 10;
pub(crate) const GRID_GAP_Y: i32 = 12;
pub(crate) const GRID_PAD: i32 = 14;

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
pub(crate) const TERM_BG: [u8; 3] = [18, 20, 26];

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
pub fn draw_icon_grid(buf: &mut [u32], w: usize, h: usize, area: Rect, win: Rect, mouse: (f32, f32), tr: Option<&TextRenderer>, entries: &[FsEntry], selected: Option<usize>, scroll: usize, cells: &mut Vec<(Rect, usize)>) -> usize {
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

/// 文本预览窗口：行号栏 + 内容行。
///
/// 出错时**在窗口内显示原因**（而不是空白）—— "打不开"和"文件是空的"必须能区分，
/// 否则用户只会看到一个空窗口，不知道该重试还是该换个文件。
pub fn draw_preview_content(buf: &mut [u32], w: usize, h: usize, r: Rect, data: Option<&PreviewData>, tr: Option<&TextRenderer>) {
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
pub fn draw_wayland_content(
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

