//! 覆层与辅助绘制：锁屏、命令面板、首启向导、折行、输入法候选、回复气泡、提示条、性能 HUD。
use super::*;

/// 帧渲染器：持有跨帧缓存（背景层等）与可点击区域登记（供命中测试）。
/// 性能 HUD（§5.10）：右下角一块半透明小面板，显示**真实**帧时间与内存。
///
/// 三条自我约束：
/// 1. **只显示真数据** —— 拿不到就写"不可用"，绝不编一个数字（本项目对假数据零容忍）。
/// 2. **默认关**（设置里的 `perf_hud`）—— 它是排障工具，不是常显装饰。
/// 3. 放在**右下角**且不拦截点击：HUD 不该影响正常操作（它没有登记任何命中区）。
pub fn draw_perf_hud(buf: &mut [u32], w: usize, h: usize, text: &str, tr: Option<&TextRenderer>) {
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

/// 锁屏遮罩（§2.3）。
///
/// 设计要点：
/// * **整屏压暗**：不透明就不叫锁屏 —— 但纯黑会让人以为死机，用高不透明度而非全黑；
/// * **不画任何界面内容**（窗口标题、dock、通知都不画）—— 锁屏的意义就是「看不到」；
/// * 提示语如实显示：拒绝理由、退避倒计时、以及「本镜像未设口令」这种**必须让人知道**的事。
pub fn draw_lock_screen(
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
pub fn draw_palette(
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
pub fn draw_setup(
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
pub fn draw_wrapped(
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
    // `ry` 的约定是"**下一个元素的 y**"（单行绘制后调用方加 `LINE_GAP` 就是行距）。
    // 只前进到最后一行的**开头**的话，调用方再加一个 `GAP_TIGHT` 就会**压在最后一行上**
    // —— 判据 ⑪「文字带区重叠」抓到页0/页16 共 12 处，全是这一个根因（附录 GG）。
    *ry += (segs.len() as i32 - 1) * step + theme::size::px(theme::size::LINE_GAP);
}

/// 输入法候选框（2.3）：画在指令条上方。
///
/// 每项带数字前缀 —— 数字键选词是最快的路径，界面上要看得见这个可能性。
pub fn draw_ime_candidates(
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
pub fn visible_window<'a>(tr: &TextRenderer, text: &'a str, caret: usize, max_w: f32) -> (&'a str, &'a str) {
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
pub fn draw_reply(buf: &mut [u32], w: usize, h: usize, text: &str, kind: BubbleKind, age: f32, tr: Option<&TextRenderer>) {
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

/// 工具 ID → 用户可读的操作名（确认卡片标题）。
/// 工具名 → 给人看的中文/英文名（§3.5）。
///
/// 收 `en: bool` 而不是在内部读全局：绘制工具函数保持**纯**，
/// 语言由调用方决定 —— 与 `audit::age_text_en` 同一套做法。
pub fn tool_label(tool: &str, en: bool) -> &str {
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

pub fn draw_toast(buf: &mut [u32], w: usize, h: usize, msg: &str, age: f32, tr: Option<&TextRenderer>) {
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
    draw_text(tr, buf, w, h, (r.x + theme::size::px(34)) as f32, tr.vcenter(r.y as f32, r.h as f32, font::BODY), msg, font::BODY, color::text(), alpha);
}

// ---------------------------------------------------------------------------
// BMP 导出（自检/截图模式）
// ---------------------------------------------------------------------------



