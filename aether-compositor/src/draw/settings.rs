//! 设置中心各页绘制（2026-11-13 从 draw.rs 抽出，E1 第二步）。
use super::*;
pub fn draw_settings_content(
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
    // 只读报表：正文里哪些文字超过**实际可用宽度**（不改变绘制）。
    // 可用宽度 = 从文字起点（`content.x + PAD`）到**窗口右沿**；
    // 因为 `content.x = r.x + SIDEBAR_W`、`content.w = r.w - SIDEBAR_W`，
    // 它就等于 `r.w - SIDEBAR_W - PAD`。
    // 曾经用 `r.w - 2*PAD` —— 对**缩进绘制**（值列）会高估可用宽度，
    // 于是判据 ⑨ 漏报（页9 那行被裁而 ⑨ 报 0，见附录 FW）。
    tr.set_over_width(Some(
        (r.w - SIDEBAR_W - theme::size::px(theme::size::CONTENT_PAD)) as f32,
    ));
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

    // 只读报表：记录正文里哪些文字超过**实际可用宽度**（不改变绘制）。
    // 可用宽度 = 从文字起点（`content.x + PAD`）到**窗口右沿**。
    // 曾经用 `r.w - 2*PAD` —— 那对**缩进绘制**（如值列）会高估可用宽度，
    // 于是判据 ⑨ 漏报（页9 那行被裁而 ⑨ 报 0，见附录 FW）。
    tr.set_over_width(Some(
        (r.x + r.w - content.x - theme::size::px(theme::size::CONTENT_PAD)) as f32,
    ));
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
    let mut ry = content.y + theme::size::px(70);

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
                            "No audit records yet this session.",
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
            draw_wrapped(tr, 
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                crate::i18n::tr(
                    "挂载是只读的（ro,nodev,nosuid,noexec）：能看里面的文件，不能往盘里写",
                    "Mounted read-only (ro,nodev,nosuid,noexec): browse files, no writes",
                ),
                font::CAPTION, color::text_dim(), 0.75,
            (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32);
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
                    buf, w, h, row, sc.keys_in(lang), Some(sc.desc_in(lang)),
                    widgets::States::at(row, mouse), Some(tr),
                );
                ry += theme::size::px(theme::size::ROW_TALL);
            }
            ry += 6;
            tr.draw(
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, ry as f32,
                crate::i18n::tr(
                    crate::i18n::pick(lang, "以上每条在源码里都有落点", "Every entry above is verified against the source"),
                    "Every entry above is verified by a test",
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
                tr.draw(buf, w, h, (content.x + theme::size::px(110)) as f32, ry as f32, &v, font::CAPTION, color::text_dim(), 0.9);
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
                    "Disconnect and reconnect",
                )),
                         widgets::States::at(act, mouse), Some(tr));
            hits.push((act, SettingsHit::RestartService("network")));
            // 步长跟着按钮高度走：写死 50 的话 150% 下按钮变高、步长不变，两行会压住
            // （几何断言发现，附录 CM）。`px(CTRL_H) + 6` 在 100% 下与 50 完全等价。
            ry += theme::size::px(theme::size::CTRL_H) + 6;
            draw_wrapped(tr, buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                    crate::i18n::pick(
                    lang,
                    "配置项本身仍是只读；改动类操作一律经 aetherd 执行。",
                    "The config itself stays read-only; every change goes through aetherd.",
                ),
                    font::CAPTION, color::text_dim(), 0.75, (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32);
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
            draw_wrapped(tr, buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                    crate::i18n::pick(lang, "从宿主安装的程序显示在 Dock 上（数据来自 /var/apps）。", "Programs installed from the host show up in the Dock (data from /var/apps)."),
                    font::CAPTION, color::text_dim(), 0.85, (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,);
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
            toggle_row(buf, w, h, mouse, tr, r1, crate::i18n::pick(lang, "深色模式", "Dark mode"), Some(crate::i18n::pick(lang, "默认关闭（明亮/纯白）", "Off by default (bright / pure white)")), s.dark_mode, SettingsHit::ToggleDark, hits);
            // §3.4 无障碍：高对比度。紧跟主题开关 —— 它是"看得清"这条线上的第二项。
            // 说明文字直接给出**可验证的依据**（WCAG AA），而不是"更清楚"这种主观话。
            let r_hc = Rect { x: r1.x, y: r1.y + theme::size::px(56), w: r1.w, h: r1.h };
            // §3.5 语言：**用该语言自己写语言名**（英文界面里出现"英语"两个字是半成品的样子）。
            // 开关语义 = "是否英文"，`Locale` 的值由当前状态算出。
            // §3.4 字号：界面里"存了不用"的又一个 —— 本批接上（缩放做在文本渲染器里）
            let r_scale = Rect { x: r1.x, y: r1.y + theme::size::px(154), w: r1.w, h: r1.h };
            let scale_hint = crate::i18n::tr(
                "只缩放文字；行高与边距仍是固定值",
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
                    "切换后左栏与页标题立即生效",
                    "Sidebar and titles switch immediately",
                )
            );
            let next_locale = if s.locale == 1 { 0 } else { 1 };
            toggle_row(buf, w, h, mouse, tr, r_lang, lang_title, Some(lang.label()),
                       s.locale == 1, SettingsHit::Locale(next_locale), hits);
            let _ = lang;

            let hc_hint = crate::i18n::pick(lang, "对比度提到 WCAG AA 以上", "Raises contrast to WCAG AA");
            toggle_row(buf, w, h, mouse, tr, r_hc, crate::i18n::pick(lang, "高对比度", "High contrast"),
                       Some(crate::i18n::pick(lang, "WCAG AA 4.5:1", "WCAG AA 4.5:1")),
                       s.high_contrast, SettingsHit::ToggleContrast, hits);
            // 第一组四行是用 `r1.y + 偏移` 定位的 —— `ry` 必须**手动推到它们下面**，
            // 否则后面的「当前配色 / 壁纸 / 语言 / 息屏」几节会画在这四行身上。
            // （由 `scripts/layout-check.py` 的几何断言发现：两个不同控件重叠 26 像素。）
            ry = r1.y + theme::size::px(154) + r1.h;
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
                         Some(crate::i18n::pick(lang, "主题/壁纸/时钟/输入等恢复出厂值", "Resets theme / wallpaper / clock / input")),
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
                tr.draw(buf, w, h, (content.x + theme::size::px(120)) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
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
            tr.draw(buf, w, h, (content.x + theme::size::px(124)) as f32, tr.vcenter(ry as f32, theme::size::px(42) as f32, font::CAPTION), crate::i18n::pick(lang, "时区（半小时制）", "Time zone (30-min steps)"), font::CAPTION, color::text_dim(), 0.9);
            hits.push((back, SettingsHit::TzShift(-30)));
            hits.push((fwd, SettingsHit::TzShift(30)));

            // ---- §3.3 对时与系统时区 ----
            // 这里要分清两件事，否则用户会以为"我已经改好了"：
            //   上面那行是**系统当前**时区，下面那行是**待写入**的候选名。
            //
            // 前进量必须覆盖**整个按钮行**（`y: ry + 6` 起、高约 `CTRL_H`）——
            // 只前进一个 GAP_TIGHT 的话，组标题会落进按钮行里
            // （判据 ⑪「文字带区重叠」抓到：`-1` 与「时间同步」压住，附录 GF）。
            ry += theme::size::px(theme::size::CTRL_H) + 6 + theme::size::px(theme::size::GAP_TIGHT);
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
                Some(crate::i18n::tr("点击切换候选（UTC / 上海 / 东京 / …）",
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
                (content.x + theme::size::px(340)) as f32,
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
                tr.draw(buf, w, h, (content.x + theme::size::px(190)) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
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
                tr.draw(buf, w, h, (content.x + theme::size::px(140)) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
                ry += theme::size::px(theme::size::LINE_GAP);
            }
            ry += 10;
            draw_wrapped(tr, buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry, crate::i18n::pick(lang, "模型与密钥由 aetherd 管理，不经过合成器进程。", "The model and its key are managed by aetherd; they never pass through the compositor process."), font::CAPTION, color::text_dim(), 0.8, (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,);
            ry += theme::size::px(28);
            // 第二个"能改系统"的动作（P4.4）：AI 卡住时把 aetherd 重启一次
            let ai_act = Rect { x: content.x + theme::size::px(theme::size::CONTROL_INSET), y: ry, w: row_w + 12, h: theme::size::px(theme::size::ROW_H) };
            widgets::row(buf, w, h, ai_act, crate::i18n::pick(lang, "重启 AI 服务", "Restart AI service"), Some(crate::i18n::pick(lang, "aetherd 重启后由 init 拉起", "init starts aetherd again after the restart")),
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
                tr.draw(buf, w, h, (content.x + theme::size::px(130)) as f32, ry as f32, v, font::CAPTION, color::text_dim(), 0.85);
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
                (crate::i18n::pick(lang, "关机", "Power off"), crate::i18n::pick(lang, "停止所有服务并断电", "Stops all services and cuts power"), SettingsHit::Power(aether_ipc::PowerAction::PowerOff)),
                (crate::i18n::pick(lang, "重启", "Reboot"), crate::i18n::pick(lang, "停止所有服务并重新引导（需确认）", "Stops all services and reboots (needs confirmation)"), SettingsHit::Power(aether_ipc::PowerAction::Reboot)),
                (crate::i18n::pick(lang, "注销", "Log out"), crate::i18n::pick(lang, "重启桌面（窗口会关闭）", "Restarts the desktop (windows close)"), SettingsHit::Logout),
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
            draw_wrapped(tr, 
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                crate::i18n::tr(
                    "只列出人类账户（uid 1000–65533）；系统账户不在这里",
                    "Only human accounts (uid 1000–65533); system accounts are hidden",
                ),
                font::CAPTION, color::text_dim(), 0.75,
            (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32);
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
                        buf, w, h, (content.x + theme::size::px(150)) as f32, ry as f32,
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
            draw_wrapped(tr, 
                buf, w, h, (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32, &mut ry,
                crate::i18n::tr(
                    "旧口令先在本机校验（证明是你本人），新口令再交给 aetherd 写入",
                    "Your current password is checked locally first, then aetherd writes the new one",
                ),
                font::CAPTION, color::text_dim(), 0.75,
            (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,);
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
                Some(crate::i18n::pick(lang, "关掉后不进历史", "Off: not in history")),
                s.record_notifications,
                SettingsHit::ToggleRecordNotifications,
                hits,
            );
            // 全文另起一行：行内放不下（150% 下超出 228px，附录 FJ 判读出来的），
            // 用页面已有的 ry 流水画在行之下（附录 DV 的手法）。
            ry = r1.y + r1.h + theme::size::px(theme::size::GAP_TIGHT);
            draw_wrapped(
                tr, buf, w, h,
                (content.x + theme::size::px(theme::size::CONTENT_PAD)) as f32,
                &mut ry,
                crate::i18n::pick(lang, "关掉后不再记录（提示条本身照常显示，只是不再进历史）", "When off, nothing is recorded (toasts still show, they just stay out of history)"),
                font::CAPTION, color::text_dim(), 0.85,
                (content.w - theme::size::px(theme::size::CONTENT_PAD) * 2) as f32,
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
                crate::i18n::pick(lang, "在控制中心里打开", "Opens from the control center"),
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
                Some(crate::i18n::tr("点击切换并试听", "Click to cycle and preview")),
                widgets::States::at(r_theme, mouse), Some(tr),
            );
            hits.push((r_theme, SettingsHit::CycleSoundTheme));

        }
        _ => {}
    }
}
