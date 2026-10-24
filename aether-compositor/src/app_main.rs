//! 桌面主循环与事件分发（从 main.rs 拆出，2026-10-24；行为不变）。

use super::*;

pub(crate) fn main() -> anyhow::Result<()> {
    // 主题：**明亮（纯白）为产品默认**，用户可在设置中心「外观」或控制中心切换。
    // 必须在任何绘制之前定好——色板在绘制期只读。
    // 优先级：显式 `--theme`（走查/调试用）> 用户设置 > 默认明亮。
    let argv: Vec<String> = std::env::args().collect();
    let theme = argv.iter().position(|a| a == "--theme").and_then(|i| argv.get(i + 1));
    let mode = match theme.map(|s| s.as_str()) {
        Some("dark") => draw::theme::color::Mode::Dark,
        Some("light") => draw::theme::color::Mode::Light,
        _ => {
            // 读用户设置（读不到就是默认值 = 明亮）
            let saved = settings::Settings::load(settings::SETTINGS_PATH, text::tz_offset_min());
            // 语言也要在**第一帧之前**生效，否则开机会先闪一屏中文（§3.5）。
            crate::i18n::set_lang(crate::i18n::Lang::from_locale(saved.locale));
            // 无障碍设置与主题一起应用（§3.4）：高对比度也要在**第一帧之前**生效，
            // 否则开机瞬间会闪一下低对比界面。
            draw::theme::color::set_high_contrast(saved.high_contrast);
            // §3.4 字号也要在**第一帧之前**生效（否则开机会先闪一屏 100%，再跳成 125%）
            draw::theme::ui::set_scale_pct(saved.ui_scale_pct);
            if saved.dark_mode {
                draw::theme::color::Mode::Dark
            } else {
                draw::theme::color::Mode::Light
            }
        }
    };
    draw::theme::color::set_mode(mode);

    // §4.1 自动 e2e：`--e2e` 从 stdin 读动作脚本，无界面执行后输出 JSONL 并退出。
    // **必须排在 fbdev 分派之前**：这条路径不该需要显示设备（否则 CI 里跑不了）。
    if argv.iter().any(|a| a == "--e2e") {
        use std::io::Read;
        let mut script = String::new();
        std::io::stdin().read_to_string(&mut script)?;
        let mut desktop = demo_desktop();
        let mut undo = UndoStack::new();
        let mut spaces: spaces::Spaces<draw::Win> = spaces::Spaces::new();
        for line in run_e2e_script(&script, &mut desktop, &mut undo, &mut spaces) {
            println!("{line}");
        }
        return Ok(());
    }

    // AetherOS 系统内：直接绘制到 Linux framebuffer（无输入，演示桌面）
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/dev/fb0").exists() && !std::env::args().any(|a| a == "--preview") {
        return fbdev_app::run_fbdev();
    }

    // 截图自检模式：`--shot [1-4]` 渲染指定布局单帧后退出
    #[cfg(not(target_os = "linux"))]
    let args: Vec<String> = std::env::args().collect();
    #[cfg(not(target_os = "linux"))]
    if let Some(pos) = args.iter().position(|a| a == "--shot") {
        // `--scale 100|125|150`（§2.4）：走查直接渲染指定缩放档。
        // **不写设置文件** —— 走查不该改用户配置；合法档位由 `ui::set_scale_pct` 吸附。
        // `--report-overflow`：渲染完打印"超过内容区宽度的文字"（只读走查，见附录 DP）。
        let report_overflow = args.iter().any(|a| a == "--report-overflow");
        let report_cjk = args.iter().any(|a| a == "--report-cjk");
        if let Some(pct) = args
            .iter()
            .position(|a| a == "--scale")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u16>().ok())
        {
            draw::theme::ui::set_scale_pct(pct);
        }
            // `--lang en|zh`：走查直接渲染指定语言。**不写设置文件**（与 --scale 同一约定）。
            // 为什么需要它：界面支持中英双语，而**英文文案宽度不同** —— 布局问题可能是英文专属的，
            // 而几何/空白/像素这些判据全是中文跑出来的（审计附录 DL）。
            if let Some(pos) = args.iter().position(|a| a == "--lang") {
                if let Some(v) = args.get(pos + 1) {
                    let en = v.eq_ignore_ascii_case("en");
                    crate::i18n::set_lang(if en { crate::i18n::Lang::En } else { crate::i18n::Lang::Zh });
                }
            }
        let lay = args
            .get(pos + 1)
            .and_then(|s| s.parse::<usize>().ok())
            .map(layout::for_key)
            .unwrap_or(Layout::TwoCol);
        let mut buf = vec![0u32; WIDTH * HEIGHT];
        let mut desktop = demo_desktop();
        desktop.layout = lay;
        // `--settings`：打开设置中心再截图（设置窗口不在默认演示桌面上）。
        // 走查图用 `AETHER_FAKE_UTC` 固定时钟，否则顶栏与设置页预览每分钟都会变。
// 注意（2026-10-04 实测，见审计附录 CD.3）：**固定时钟不等于输出可复现** ——
// 这些界面还会去探真实运行时状态（音频探测、AI 状态等），所以同一构建连跑两遍，
// 仍有若干档的 BMP 不同。**要拿 `--shot` 当自动化的像素回归判据，得先给它一个
// "确定模式"**（把所有运行时探针换成夹具值）；在那之前它只适合人眼看走查图。
        if args.iter().any(|a| a == "--settings") {
            let _ = open_app(&mut desktop, 4);
        }
        // `--desktop-only`：清空演示窗口，走查**默认桌面**（无窗口 + 桌面图标）
        if args.iter().any(|a| a == "--desktop-only") {
            desktop.wins.clear();
            desktop.active = 0;
        }
        // `--control-center`：展开控制中心再截图（状态簇面板的视觉走查）
        snap_now(&mut desktop, lay);
        let tr = text::TextRenderer::load();
        let mut renderer = draw::Renderer::new(WIDTH, HEIGHT);
        // `--control-center`：展开控制中心再截图（状态簇面板的视觉走查）。
        // **必须在 renderer 建好之后**设置（它在 Renderer 上，不在 Desktop 上）。
        if args.iter().any(|a| a == "--control-center") {
            renderer.control_open = true;
        }
        // `--settings-page N`：设置窗口打开后切到第 N 页（走查各页的视觉基线）。
        // 索引与 `settings::PAGES` 一致；夹一次上限，越界会静默拍成别的页。
        // `--control-scroll N`：走查控制中心**滚到底**的样子 —— 滚轮事件在 --shot 里
        // 发不出来，没有它就没法把滚动后的状态渲出来看。只为可观测，不写设置。
        if let Some(pos) = args.iter().position(|a| a == "--control-scroll") {
            if let Some(n) = args.get(pos + 1).and_then(|s| s.parse::<i32>().ok()) {
                renderer.control_scroll = n.max(0);
            }
        }
        if let Some(pos) = args.iter().position(|a| a == "--settings-page") {
            if let Some(n) = args.get(pos + 1).and_then(|s| s.parse::<usize>().ok()) {
                let n_pages = settings::PAGES.len();
                renderer.settings_page = if n_pages == 0 { 0 } else { n.min(n_pages - 1) };
            }
        }
        let sample_input = "把窗口排成两列";
        // `--ime`：走查输入法候选框（预置 "nihao" → 候选「你好」）
        let shot_ime = if args.iter().any(|a| a == "--ime") {
            let mut i = ime::Ime::default();
            i.toggle();
            for c in "nihao".chars() {
                i.push(c);
            }
            i
        } else {
            ime::Ime::default()
        };
        let sample_reply = "好的，已把窗口排成两列。";
        // `--mouse X Y`：把指针放到指定位置，便于截图覆盖悬停态（红绿灯符号、Dock hover…）
        let mouse = args
            .iter()
            .position(|a| a == "--mouse")
            .and_then(|i| {
                let x = args.get(i + 1)?.parse::<f32>().ok()?;
                let y = args.get(i + 2)?.parse::<f32>().ok()?;
                Some((x, y))
            })
            .unwrap_or(if args.contains(&"--menu".to_string()) { (700.0, 120.0) } else { (0.0, 0.0) });
        // `--installer`：渲染安装向导单帧（视觉走查用，非真实会话）
        let demo_disks = vec![("/dev/vda".to_string(), 20_480u64), ("/dev/vdb".to_string(), 8_192)];
        let installer = args.iter().any(|a| a == "--installer").then(|| draw::InstallerUi {
            disks: &demo_disks,
            selected: 0,
            phase: draw::InstallerPhase::Idle,
            message: None,
        });
        // `--confirm [2|3]`：渲染 L2+ 权限确认弹窗；加 `--echo` 预填回显（走查匹配态）
        let confirm_level = args
            .iter()
            .position(|a| a == "--confirm")
            .map(|i| args.get(i + 1).and_then(|s| s.parse::<u8>().ok()).unwrap_or(3));
        let demo_args: Vec<(String, String)> = vec![("disk".to_string(), "/dev/vda".to_string())];
        let echo_seed: &str = if args.iter().any(|a| a == "--echo") { "/dev/vda" } else { "" };
        let confirm = confirm_level.map(|level| draw::ConfirmUi {
            tool: "install_disk",
            level,
            arguments: &demo_args,
            consequence: "整盘覆盖写入：目标磁盘上的分区表与所有数据将被永久删除，不可恢复。",
            echo_required: if level >= 3 { Some("/dev/vda") } else { None },
            echo_input: echo_seed,
        });
        // `--ai-status local|cloud|offline`：顶栏 AI 三态走查（4.1 验收依据）
        let ai_status = match args.iter().position(|a| a == "--ai-status").and_then(|i| args.get(i + 1)) {
            Some(s) if s == "cloud" => draw::AiStatus::Cloud,
            Some(s) if s == "offline" => draw::AiStatus::Offline,
            _ => draw::AiStatus::Local,
        };
        // `--bubble user|ai|tool`：三类回复气泡走查（4.2 验收依据）
        let sample_bubble = match args.iter().position(|a| a == "--bubble").and_then(|i| args.get(i + 1)) {
            Some(s) if s == "user" => draw::BubbleKind::User,
            Some(s) if s == "tool" => draw::BubbleKind::Tool,
            _ => draw::BubbleKind::Ai,
        };
        let ui = draw::UiState {
            snap: None,
            toast: None,
            ai_input: sample_input,
            ime: &shot_ime,
            ai_cursor: sample_input.chars().count(),
            ai_focused: true,
            ai_thinking: args.iter().any(|a| a == "--thinking"),
            ai_status,
            ai_reply: Some((sample_reply, sample_bubble, 0.5)),
            mouse,
            mouse_down: false,
            // 截图/预览模式没有真实音频栈：如实标"未检测"并禁用控件（不画假滑块）
            audio_label: "未检测到音频（预览模式）",
            audio_volume: 0,
            audio_enabled: false,
            open_menu: if args.contains(&"--menu".to_string()) { Some(2) } else { None },
            show_installer: args.iter().any(|a| a == "--installer"),
            installer,
            confirm,
        };
        // 单帧输出必须先把壁纸补全（交互路径是分帧生成的）
        renderer.prepare_background(WIDTH, HEIGHT, 1.2);
        if report_cjk { if let Some(t) = tr.as_ref() { t.set_cjk_report(true); } }
        // 判据 ⑧ 的精确版：让渲染器登记文字带区（附录 GE）
        if args.iter().any(|a| a == "--dump-layout") {
            if let Some(t) = tr.as_ref() {
                t.set_band_report(true);
            }
        }
        renderer.render_frame(&mut buf, WIDTH, HEIGHT, 1.2, &desktop, &ui, tr.as_ref());
        // 只读走查报表：哪些文字超过内容区宽度（不改变绘制结果，见附录 DP）
        if report_overflow {
            if let Some(t) = tr.as_ref() {
                for (s, over) in t.take_over_hits() {
                    println!("OVERFLOW {over:.1}px {s}");
                }
            }
        }
                if report_cjk {
                    if let Some(t) = tr.as_ref() {
                        for s in t.take_cjk_hits() {
                            println!("CJK {s}");
                        }
                    }
                }
        draw::write_bmp("preview.bmp", &buf, WIDTH, HEIGHT)?;
        // `--dump-layout`（§2.4 的几何判据）：把这一帧登记过的矩形打成 JSON。
        //
        // 为什么需要它：像素比对能证明"没变"，但证明不了"内容在框里"。
        // 缩放最容易出的问题是**溢出**，而那是纯几何问题 —— 用断言查比用人眼看可靠。
        if args.iter().any(|a| a == "--dump-layout") {
            let work = layout::work_area(WIDTH, HEIGHT);
            let texts: Vec<serde_json::Value> = tr
                .as_ref()
                .map(|t| t.take_bands())
                .unwrap_or_default()
                .into_iter()
                .map(|(x0, y0, x1, y1, s, win, clip)| {
                    let c = clip.map(|(cx0, cy0, _, _)| (cx0, cy0));
                    serde_json::json!({"x": x0, "y": y0, "w": x1 - x0, "h": y1 - y0, "text": s, "win": win, "clip": c})
                })
                .collect();
            let hits: Vec<serde_json::Value> = renderer
                .settings_hits
                .iter()
                .map(|(r, h)| {
                    serde_json::json!({"x": r.x, "y": r.y, "w": r.w, "h": r.h, "kind": format!("{h:?}")})
                })
                .collect();
            let wins: Vec<serde_json::Value> = desktop
                .wins
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "title": w.title, "x": w.rect.x, "y": w.rect.y, "w": w.rect.w, "h": w.rect.h
                    })
                })
                .collect();
            // 窗口类界面的命中区也一起吐出来 —— 它们同样是"可点的东西"，
            // 同样不该越界或互相压住。
            let mut widgets: Vec<serde_json::Value> = Vec::new();
            let mut push = |group: &str, kind: String, r: &Rect| {
                widgets.push(serde_json::json!({
                    "group": group, "kind": kind, "x": r.x, "y": r.y, "w": r.w, "h": r.h
                }));
            };
            for r in &renderer.dock_icons {
                push("dock", "icon".to_string(), r);
            }
            for (r, h) in &renderer.control_hits {
                push("control", format!("{h:?}"), r);
            }
            for (r, i) in &renderer.sidebar_hits {
                push("files-sidebar", format!("item-{i}"), r);
            }
            for (r, s) in &renderer.crumb_hits {
                push("files-crumb", s.clone(), r);
            }
            for (r, i) in &renderer.music_hits {
                push("music-rows", format!("row-{i}"), r);
            }
            for (wi, close, min, max) in &renderer.window_lights {
                push("window-lights", format!("win{wi}-close"), close);
                push("window-lights", format!("win{wi}-min"), min);
                push("window-lights", format!("win{wi}-max"), max);
            }
            let snap = serde_json::json!({
                "w": WIDTH,
                "h": HEIGHT,
                "work": {"x": work.x, "y": work.y, "w": work.w, "h": work.h},
                "wins": wins,
                "hits": hits,
                "texts": texts,
                "widgets": widgets,
            });
            // 加个前缀，方便脚本从"preview.bmp written"之类输出里认出来
            println!("LAYOUT {snap}");
        }
        println!("preview.bmp written (layout: {})", lay.label());
        return Ok(());
    }

    // 帧耗时基准：`--bench [N]` 连续渲染 N 帧，输出**平均/最差/P99**，并按阈值给退出码。
    //
    // 只测 **CPU 侧绘制成本**（不上屏）—— 这是门禁能稳定的前提：
    // 上屏路径依赖真实显示设备，那种数字在 CI 里没有可比性。
    //
    // 它**刻意放在 fbdev 分支之前**（§4.2）：早先它排在后面且只编非 Linux，
    // 于是 Linux CI 永远跑不到它 —— 一个"只在开发机上存在"的门禁等于没有门禁。
    // 注意用 `argv` 而不是 `args`：后者只在非 Linux 分支里存在，而这批把本块挪到了
    // 平台无关的位置 —— **musl 检查当场抓出来**（Windows 构建看不到这个作用域差异）。
    if let Some(pos) = argv.iter().position(|a| a == "--bench") {
        let n: u32 = argv.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(120);
        let shot_ime = ime::Ime::default();
        let mut buf = vec![0u32; WIDTH * HEIGHT];
        let mut desktop = demo_desktop();
        desktop.layout = Layout::TwoCol;
        snap_now(&mut desktop, Layout::TwoCol);
        let tr = text::TextRenderer::load();
        let ui = draw::UiState {
            snap: None,
            toast: None,
            ai_input: "把窗口排成两列",
            ime: &shot_ime,
            ai_cursor: "把窗口排成两列".chars().count(),
            ai_focused: true,
            ai_thinking: false,
            ai_status: draw::AiStatus::Local,
            ai_reply: Some(("好的，已把窗口排成两列。", draw::BubbleKind::Ai, 0.5)),
            mouse: (640.0, 400.0),
            mouse_down: false,
            // 截图/预览模式没有真实音频栈：如实标"未检测"并禁用控件（不画假滑块）
            audio_label: "未检测到音频（预览模式）",
            audio_volume: 0,
            audio_enabled: false,
            open_menu: None,
            show_installer: false,
            installer: None,
            confirm: None,
        };

        // 壁纸整屏生成单独计时：交互路径已改为分帧摊开（每帧 64 行），
        // 这里量的是"整屏一次算完"的原始成本，用于判断还要不要继续优化。
        let mut fresh = draw::Renderer::new(WIDTH, HEIGHT);
        let t_bg = std::time::Instant::now();
        fresh.prepare_background(WIDTH, HEIGHT, 1.2);
        let bg_ms = t_bg.elapsed().as_secs_f64() * 1000.0;
        let t_frame = std::time::Instant::now();
        fresh.render_frame(&mut buf, WIDTH, HEIGHT, 1.2, &desktop, &ui, tr.as_ref());
        let first_ms = t_frame.elapsed().as_secs_f64() * 1000.0;

        // 交互路径的真实首帧：不分帧前这里要 0.4–0.6 秒（开机第一眼就是卡死），
        // 现在只生成 64 行壁纸。这一行就是"分帧"改动的验收数字。
        let mut inc = draw::Renderer::new(WIDTH, HEIGHT);
        let t_inc = std::time::Instant::now();
        inc.render_frame(&mut buf, WIDTH, HEIGHT, 1.2, &desktop, &ui, tr.as_ref());
        let inc_first = t_inc.elapsed().as_secs_f64() * 1000.0;

        let mut renderer = draw::Renderer::new(WIDTH, HEIGHT);
        renderer.prepare_background(WIDTH, HEIGHT, 1.2);
        for _ in 0..3 {
            renderer.render_frame(&mut buf, WIDTH, HEIGHT, 1.2, &desktop, &ui, tr.as_ref());
        }
        // **逐帧计时**（不是"总时间 ÷ 帧数"）：门禁要的是"最差那一帧"，
        // 而平均值会把偶发的长帧摊平 —— 用户感觉到的卡顿恰恰是长帧。
        let mut per_frame: Vec<f64> = Vec::with_capacity(n as usize);
        for i in 0..n {
            let t1 = std::time::Instant::now();
            renderer.render_frame(
                &mut buf, WIDTH, HEIGHT, 1.2 + i as f32 * 0.016, &desktop, &ui, tr.as_ref(),
            );
            draw::draw_cursor(&mut buf, WIDTH, HEIGHT, 640.0, 400.0);
            per_frame.push(t1.elapsed().as_secs_f64() * 1000.0);
        }
        // 平均值直接用逐帧计时求和（与分位数同一份数据，避免两套数字打架）
        let per = if per_frame.is_empty() {
            0.0
        } else {
            per_frame.iter().sum::<f64>() / per_frame.len() as f64
        };
        // 分位数：先排序再取（n 只有几百，不值得上选择算法）
        let mut sorted = per_frame.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let worst = sorted.last().copied().unwrap_or(0.0);
        let pick = |q: f64| -> f64 {
            if sorted.is_empty() {
                return 0.0;
            }
            let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[idx.min(sorted.len() - 1)]
        };
        let p50 = pick(0.50);
        let p99 = pick(0.99);
        // 分帧生成：整屏行数 ÷ 每帧行数
        let bg_frames = (HEIGHT as f64 / draw::BG_ROWS_PER_FRAME as f64).ceil();
        println!("bench @ {WIDTH}x{HEIGHT}（两列 + 鼠标）");
        println!("  壁纸整屏生成（一次性）: {bg_ms:.2} ms");
        println!("  分帧后单帧增量: ≈{:.2} ms（{bg_frames:.0} 帧铺完，不再阻塞首帧）", bg_ms / bg_frames);
        println!("  **交互路径首帧**: {inc_first:.2} ms（分帧生效；改动前 = 首帧 + 整屏生成）");
        println!("  首帧（壁纸已就绪）: {first_ms:.2} ms");
        println!("  稳态 {n} 帧: 平均 {per:.2} ms/帧 → 上限 {:.0} fps", 1000.0 / per);
        println!("  理论 30fps 预算 33.3ms/帧，当前占用 {:.0}%", per / 33.3 * 100.0);
        // §4.2 门禁看这三行：平均值会骗人，**P99 才是"会不会卡一下"**
        println!("  逐帧分位: P50 {p50:.2} ms · **P99 {p99:.2} ms** · 最差 {worst:.2} ms");
        // 阈值可由环境变量覆盖：CI 机器比开发机慢/吵，硬编码一个数会把门禁变成噪声源
        let limit: f64 = std::env::var("AETHER_BENCH_MAX_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16.0);
        if p99 > limit {
            println!("  [FAIL] P99 {p99:.2} ms 超过阈值 {limit:.2} ms（AETHER_BENCH_MAX_MS 可调）");
            std::process::exit(1);
        }
        println!("  [OK] P99 在阈值 {limit:.2} ms 之内");
        return Ok(());
    }

    // 字体标本模式：`--fonttest` 渲染 11–15px 中英文/粗细同屏单帧（文字渲染质量调参用）
    #[cfg(not(target_os = "linux"))]
    if args.iter().any(|a| a == "--fonttest") {
        let tr = text::TextRenderer::load().expect("字体标本模式需要可用字体");
        // 打印真实 metrics：垂直居中的换算依赖它，靠猜会在换字体后错位
        tr.dump_metrics(&[11.0, 12.0, 13.0, 14.0, 20.0]);
        let (fw, fh) = (920usize, 560usize);
        let mut buf = vec![0u32; fw * fh];
        for p in buf.iter_mut() {
            *p = (38 << 16) | (40 << 8) | 50;
        }
        let sentence = "中文清晰度：把窗口排成两列，打开终端与文件管理器。";
        let latin = "Aether 0.1.0 — install_disk /dev/vda [OK]";
        let mut y = 20.0f32;
        for px in [11.0f32, 12.0, 13.0, 14.0, 15.0] {
            for (bold, label) in [(false, "常规"), (true, "粗体")] {
                let tag = format!("{px:.0}px {label}");
                tr.draw(&mut buf, fw, fh, 16.0, y, &tag, 11.0, draw::theme::color::text_dim(), 0.9);
                let x = 96.0;
                let x = tr.draw(&mut buf, fw, fh, x, y, sentence, px, draw::theme::color::text(), 0.98);
                if bold {
                    tr.draw_bold(&mut buf, fw, fh, x + 12.0, y, latin, px, draw::theme::color::text(), 0.98);
                } else {
                    tr.draw(&mut buf, fw, fh, x + 12.0, y, latin, px, draw::theme::color::text_dim(), 0.9);
                }
                y += px * 1.9 + 6.0;
            }
        }
        draw::write_bmp("fonttest.bmp", &buf, fw, fh)?;
        println!("fonttest.bmp written");
        return Ok(());
    }

    // 桌面预览模式（minifb，仅非 Linux 主机开发迭代用；musl/系统内走 fbdev）
    #[cfg(not(target_os = "linux"))]
    {
        preview_main()?;
    }
    #[cfg(target_os = "linux")]
    {
        // Linux 有 X11/Wayland 桌面时可走 --preview，否则 fbdev 已在上面接管
        eprintln!("aether-compositor: 无 /dev/fb0，且系统内尚无 Wayland 后端（M1 后期接入 smithay）");
        std::process::exit(1);
    }
    #[allow(unreachable_code)] // Linux 分支已在上面 exit
    Ok(())
}

/// minifb 预览主循环（Windows 主机开发迭代形态，M1–M4 联调）。
#[cfg(not(target_os = "linux"))]
pub(crate) fn preview_main() -> anyhow::Result<()> {
    let tr = text::TextRenderer::load();
    let mut renderer = draw::Renderer::new(WIDTH, HEIGHT);
    // 开发机预览也显示已装应用（Windows 上是 `%TEMP%\aether-apps`）：
    // 这条路径是唯一能在不用起 QEMU 的情况下看到 Dock 长什么样的地方。
    renderer.installed_apps = load_installed_apps(&apps_root());
    let mut last_apps_scan = Instant::now();
    let mut desktop = demo_desktop();
    let mut buffer = vec![0u32; WIDTH * HEIGHT];
    let mut window = minifb::Window::new(
        "AetherOS — Compositor Preview（打字输入指令回车发送 · 1/2/3/4 布局 · Esc 退出）",
        WIDTH,
        HEIGHT,
        minifb::WindowOptions::default(),
    )?;
    #[allow(deprecated)]
    window.limit_update_rate(Some(std::time::Duration::from_micros(16_600)));

    let start = Instant::now();
    let mut drag: Option<(usize, f32, f32)> = None;
    // 缩放中的窗口：(索引, 边, 按下时的原始矩形)
    let mut resize: Option<(usize, Edge, draw::Rect)> = None;
    let mut snap_zone = layout::Snap::None;
    let mut toast: Option<(ToastMsg, Instant)> = None;
    let mut ai_input = String::new();
    // 2.4：输入光标 —— 支持在中间插入/删除，不再只能追加（打错一个字要全删重来）
    let mut ai_cursor = textview::TextCursor::default();
    // 2.3：中文输入法（默认关，Ctrl+Space 切换 —— 见 ime 模块头注释）
    let mut ime = ime::Ime::default();
    // P3 设置项「默认启用中文输入法」：启动时按用户偏好打开（否则这个开关就是"点了没用"）
    if renderer.settings.ime_default && !ime.enabled() {
        ime.toggle();
    }
    let mut ai_reply: Option<(String, draw::BubbleKind, Instant)> = None;
    let (tx, rx) = mpsc::channel::<AiEvent>();
    let mut open_menu: Option<usize> = None;
    let mut confirm: Option<(ConfirmRequest, String)> = None;
    let mut ai_status = draw::AiStatus::Local;
    let mut thinking = false;

    // §5.1 撤销栈：**预览循环也要有**（两个循环都会应用 AI 动作，
    // 只给其中一个接线正是会产生静默不一致的那类改动）
    let mut undo = UndoStack::new();
    while window.is_open() && !window.is_key_down(minifb::Key::Escape) {
        let t = start.elapsed().as_secs_f32();
        // 通知记录开关（§3.2）：**预览循环也必须同步** —— 自查时发现只有 fbdev 循环同步，
        // 于是主机预览里"关闭记录"会被忽略（两个循环各写一份状态，正是会产生
        // 静默不一致的那类改动；这里补上，并留注释免得下次又只改一边）。
        renderer.notifs.set_enabled(renderer.settings.record_notifications);
        let (mx, my) = window
            .get_mouse_pos(minifb::MouseMode::Discard)
            .unwrap_or((0.0, 0.0));
        let down = window.get_mouse_down(minifb::MouseButton::Left);

        // 终端会话维护（读 PTY / 同步 winsize）—— 每帧一次
        maintain_terminals(&mut desktop, tr.as_ref());

        // ---- 键盘：AI 指令输入 + 布局切换 + 窗口/文件导航 ----
        // minifb 无字符级输入 API，预览期用按键映射（字母/数字/常用符号）。
        // 布局切换 = **Alt+1..4**：裸数字必须留给输入框（2026-09-29 修复，见 input.rs 同一处）。
        let ctrl = window.is_key_down(minifb::Key::LeftCtrl)
            || window.is_key_down(minifb::Key::RightCtrl);
        let alt = window.is_key_down(minifb::Key::LeftAlt)
            || window.is_key_down(minifb::Key::RightAlt);
        // 终端拿到焦点时按键归 shell（与真机路径同一条规则）
        let term_owns_keys = confirm.is_none() && focused_terminal(&desktop);
        if term_owns_keys {
            for k in window.get_keys_pressed(minifb::KeyRepeat::Yes) {
                let tk = if ctrl {
                    minifb_nav(&k)
                        .map(term::TermKey::Nav)
                        .or_else(|| key_to_char(&k).map(term::TermKey::Ctrl))
                } else if let Some(nav) = minifb_nav(&k) {
                    Some(term::TermKey::Nav(nav))
                } else {
                    match k {
                        minifb::Key::Enter => Some(term::TermKey::Enter),
                        minifb::Key::Backspace => Some(term::TermKey::Backspace),
                        _ => key_to_char(&k).map(term::TermKey::Char),
                    }
                };
                if let Some(tk) = tk {
                    feed_terminal(&mut desktop, tk, &mut ime);
                }
            }
            // 终端占据键盘时，下面的 AI 指令条输入/导航/布局键一律跳过
        } else {
        let held = window.get_keys_pressed(minifb::KeyRepeat::Yes);
        for k in &held {
            // 按住 Ctrl 时不产生字符 —— 否则 Ctrl+W 会先把 'w' 打进指令条再关窗
            if ctrl {
                continue;
            }
            if let Some(ch) = key_to_char(k) {
                // 与真机同款 IME 分流
                let mut committed: Option<String> = None;
                if ime.composing() {
                    if let Some(d) = ch.to_digit(10).filter(|d| *d >= 1) {
                        committed = ime.select_index(d as usize - 1);
                    } else if ch == ' ' {
                        committed = ime.commit();
                    }
                }
                match committed {
                    Some(text) => push_str(&mut ai_input, &mut ai_cursor, &text),
                    None => {
                        if let Some(text) = ime.push(ch) {
                            push_str(&mut ai_input, &mut ai_cursor, &text);
                        }
                    }
                }
            }
        }
        if !ctrl && held.contains(&minifb::Key::Backspace) {
            // IME 正在拼字时，退格删拼音串而不是正文
            if !ime.backspace() {
                ai_cursor.backspace(&mut ai_input);
            }
        }
        let keys = window.get_keys_pressed(minifb::KeyRepeat::No);
        for k in &keys {
            // Ctrl+Space 切中英输入（与真机路径的 NUL 特判对应）
            if ctrl && *k == minifb::Key::Space {
                ime.toggle();
                push_toast(&mut toast, &mut renderer.notifs, if ime.enabled() { toast_msg("中文输入：开", "Chinese IME: on") } else { toast_msg("中文输入：关", "Chinese IME: off") }, notifications::Kind::Err);
                continue;
            }
            // Ctrl 组合键：Ctrl+W 关窗、Ctrl+Shift+C/V 复制粘贴
            if ctrl {
                if let Some(ch) = key_to_char(k) {
                    let shift = window.is_key_down(minifb::Key::LeftShift)
                        || window.is_key_down(minifb::Key::RightShift);
                    match apply_ctrl(&mut desktop, ch.to_ascii_lowercase(), shift) {
                        CtrlOutcome::Closed { at, message, win } => {
                            // 与 fbdev 循环**同一套**处理：预览路径也不能例外，
                            // 否则"红灯能撤销、Ctrl+W 不能"这种漂移迟早出现。
                            drag = None;
                            snap_zone = layout::Snap::None;
                            let title = win.title.clone();
                            let _ = undo.keep_closed(*win);
                            undo.push(UndoStep::ClosedWindow { at, title });
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                message,
                                notifications::Kind::Ok,
                            );
                            continue;
                        }
                        CtrlOutcome::Message(msg) => {
                            drag = None;
                            snap_zone = layout::Snap::None;
                            push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Ok);
                            continue;
                        }
                        CtrlOutcome::Nothing => {}
                    }
                }
            }
            // 导航键：三级分流（指令条编辑 → 预览滚动 → 文件列表），与真机同一条实现
            if let Some(nav) = minifb_nav(k) {
                if let Some(msg) =
                    dispatch_nav(&mut desktop, nav, &mut ai_input, &mut ai_cursor, &mut ime, &tx)
                {
                    push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                }
                continue;
            }
            match k {
                minifb::Key::Enter => {
                    if !ai_input.trim().is_empty() {
                        let text = ai_input.trim().to_string();
                        ai_input.clear();
                        ai_cursor.home(); // 发送后光标回到开头
                        // 先把自己的话显示成"用户"气泡，再进入思考态
                        ai_reply = Some((text.clone(), draw::BubbleKind::User, Instant::now()));
                        thinking = true;
                        let tx = tx.clone();
                        std::thread::spawn(move || query_aether(text, 130, tx));
                    }
                }
                minifb::Key::Key1 if alt => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::Float) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
                minifb::Key::Key2 if alt => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::TwoCol) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
                minifb::Key::Key3 if alt => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::ThreeCol) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
                minifb::Key::Key4 if alt => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::Monocle) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
                _ => {}
            }
        }
        }

        // ---- AI 事件轮询（非阻塞）----
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AiEvent::UsersLoaded { list } => {
                renderer.users = list;
                renderer.users_loaded = true;
            }
            AiEvent::PasswordAck { ok, message } => {
                renderer.pass_form.pending = false;
                renderer.pass_form.feedback = Some(message.clone());
                if ok {
                    // 成功才清空三个框：失败要留着让用户改
                    renderer.pass_form.clear();
                }
                push_toast(
                    &mut toast,
                    &mut renderer.notifs,
                    ToastMsg::from_external(message),
                    if ok { notifications::Kind::Ok } else { notifications::Kind::Err },
                );
            }
            AiEvent::PeripheralsLoaded { list } => {
                renderer.peripherals = list;
                renderer.peripherals_loaded = true;
            }
            AiEvent::RemovablesLoaded { devices } => {
                    renderer.removables = devices;
                    renderer.removables_loaded = true;
                    renderer.removable_pending = None;
                }
                AiEvent::RemovableAck { ok, message } => {
                    renderer.removable_pending = None;
                    renderer.removable_message = Some(message.clone());
                    let kind = if ok {
                        notifications::Kind::Ok
                    } else {
                        notifications::Kind::Err
                    };
                    push_toast(&mut toast, &mut renderer.notifs, ToastMsg::from_external(message), kind);
                    // 这一处（另一个循环）没有 IPC 发送端：只落地状态，不触发重新扫描
                }
                AiEvent::DisplaysLoaded { driven_index, outputs } => {
                    renderer.displays = outputs;
                    renderer.driven_index = driven_index;
                    renderer.displays_loaded = true;
                }
                AiEvent::UpdatesLoaded { packages, current, staged } => {
                    renderer.update.on_list(packages, current, staged);
                }
                AiEvent::UpdateResult { ok, message, version } => {
                    renderer.update.on_result(ok, message.clone(), version);
                    let kind = if ok {
                        notifications::Kind::Ok
                    } else {
                        notifications::Kind::Err
                    };
                    push_toast(&mut toast, &mut renderer.notifs, ToastMsg::from_external(message), kind);
                }
                AiEvent::SnapshotsLoaded(items) => {
                    renderer.snapshots.on_list(items);
                }
                AiEvent::SnapshotDone { ok, message, notes } => {
                    if ok {
                        push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        ToastMsg::from_external(message),
                        notifications::Kind::Ok,
                    );
                        // 回滚的逐文件说明：只说"已回滚"信息太少，用户想知道到底换了哪些文件
                        if !notes.is_empty() {
                            let brief: String = notes
                                .iter()
                                .take(2)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("；");
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                ToastMsg::new(
                                    format!("{brief}（共 {} 项）", notes.len()),
                                    format!("{brief} ({} items)", notes.len()),
                                ),
                                notifications::Kind::Info,
                            );
                        }
                        // 成功后**重新取一次清单**：本地不猜（列表要如实反映磁盘上的快照）
                        // 预览循环（--preview）没有 IPC 发送端：这里只落地状态，不做「再取一次」
                    } else {
                        renderer.snapshots.on_failure(message.clone());
                        push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        ToastMsg::from_external(message),
                        notifications::Kind::Err,
                    );
                    }
                }
                AiEvent::TimeStatus { timezone, last_offset_ms, last_sync_unix } => {
                    renderer.time.on_status(timezone, last_offset_ms, last_sync_unix);
                }
                AiEvent::TimeSynced { ok, message } => {
                    if ok {
                        push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        ToastMsg::from_external(message),
                        notifications::Kind::Ok,
                    );
                        // 对完时再取一次状态：「上次偏移」那一行才会显示刚刚这次的结果
                        // 预览循环（--preview）没有 IPC 发送端：这里只落地状态，
                        // 不做「再取一次」（那要连 aetherd，而预览模式刻意不依赖 IPC/显示环境）
                    } else {
                        renderer.time.on_failure(message.clone());
                        push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        ToastMsg::from_external(message),
                        notifications::Kind::Err,
                    );
                    }
                }
                AiEvent::TimezoneSet { ok, message } => {
                    if ok {
                        push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        ToastMsg::from_external(message),
                        notifications::Kind::Ok,
                    );
                        // 写成功后重取状态：界面上的「系统时区」要跟着变成新名字
                        // 预览循环（--preview）没有 IPC 发送端：这里只落地状态，
                        // 不做「再取一次」（那要连 aetherd，而预览模式刻意不依赖 IPC/显示环境）
                    } else {
                        renderer.time.on_failure(message.clone());
                        push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        ToastMsg::from_external(message),
                        notifications::Kind::Err,
                    );
                    }
                }
                AiEvent::PolicyLoaded(entries) => {
                    renderer.policy.set_entries(entries);
                }
                AiEvent::PolicySaved { ok, message, tool, policy } => {
                    if ok {
                        // 服务端确认后才更新那一行 —— 本地先改就等于"假装已生效"
                        renderer.policy.apply_confirmed(&tool, policy);
                        push_toast(
                            &mut toast,
                            &mut renderer.notifs,
                            ToastMsg::from_external(message),
                            notifications::Kind::Ok,
                        );
                    } else {
                        renderer.policy.set_error(message.clone());
                        push_toast(
                            &mut toast,
                            &mut renderer.notifs,
                            ToastMsg::from_external(message),
                            notifications::Kind::Err,
                        );
                    }
                }
                AiEvent::PolicyFailed(why) => {
                    renderer.policy.set_error(why.clone());
                    push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        // 策略失败原因来自 aetherd（已按其连接语言渲染）
                        ToastMsg::from_external(why),
                        notifications::Kind::Err,
                    );
                }
                AiEvent::DnsSaved { ok, message } => {
                    // 回执落到输入框的反馈行：**成功才退出编辑态**，失败留在编辑态
                    // 让用户接着改（把他踢出去等于让他重来一遍）。
                    // `DnsField::finish` 封装了这条规则，两处调用点的行为因此一致。
                    let msg = message;
                    renderer.dns.finish(if ok { Ok(msg.clone()) } else { Err(msg.clone()) });
                    // 也弹一条提示：编辑框可能已滚出视野，而"改系统配置"的结果必须让人看到
                    push_toast(
                        &mut toast,
                        &mut renderer.notifs,
                        // DNS 回执来自 aetherd（已按其连接语言渲染）
                        ToastMsg::from_external(format!("DNS：{msg}")),
                        if ok { notifications::Kind::Ok } else { notifications::Kind::Err },
                    );
                }
                AiEvent::Reply(text, channel) => {
                    ai_status = match channel.as_deref() {
                        Some("cloud") => draw::AiStatus::Cloud,
                        _ => draw::AiStatus::Local,
                    };
                    thinking = false;
                    ai_reply = Some((text, draw::BubbleKind::Ai, Instant::now()));
                }
                AiEvent::Error(text) => {
                    ai_status = draw::AiStatus::Offline;
                    thinking = false;
                    ai_reply = Some((format!("⚠ {text}"), draw::BubbleKind::Ai, Instant::now()));
                }
                AiEvent::Action(name, args) => {
                    if let Some(msg) = apply_action(&mut desktop, &name, &args, &mut undo) {
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("Aether 执行 · {}", "Aether ran · {}", msg.text()), notifications::Kind::Info);
                    }
                }
                AiEvent::Confirm(req) => {
                    thinking = false;
                    confirm = Some((*req, String::new()));
                }
                AiEvent::ToolDone { ok, output, .. } => {
                    thinking = false;
                    let last = output.lines().last().unwrap_or("").to_string();
                    let mark = if ok { "✓" } else { "✗" };
                    ai_reply = Some((format!("{mark} {last}"), draw::BubbleKind::Tool, Instant::now()));
                }
            }
        }

        // ---- 鼠标：确认弹窗模态优先，其次拖拽优先，再次 UI 命中测试 ----
        if down && confirm.is_some() {
            // 权限确认弹窗独占命中：允许一次（L3 需回显匹配）/ 拒绝
            let hit = renderer
                .confirm_buttons
                .iter()
                .find(|(r, _)| r.contains(mx, my))
                .map(|(_, b)| *b);
            let echo_ok = confirm.as_ref().map(|(r, e)| r.echo_ok_input(e)).unwrap_or(false);
            match hit {
                Some(draw::ConfirmButton::Allow) if echo_ok => {
                    if let Some((req, _)) = confirm.take() {
                        dispatch_approved(req, &tx);
                    }
                }
                Some(draw::ConfirmButton::Allow) => {
                    push_toast(&mut toast, &mut renderer.notifs, toast_msg("回显不匹配：请原样输入目标名", "Echo mismatch: type the target name exactly"), notifications::Kind::Info);
                }
                Some(draw::ConfirmButton::Deny) => {
                    if let Some((req, _)) = confirm.take() {
                        // 拒绝要回传服务端（撤销令牌 + 留痕），不能只关本地弹窗
                        cancel_confirm(&req.token);
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("已拒绝「{}」", "Denied 「{}」", req.tool), notifications::Kind::Info);
                    }
                }
                None => {}
            }
        } else if down {
            if let Some((idx, edge, orig)) = resize {
                if down {
                    if let Some(w) = desktop.wins.get_mut(idx) {
                        apply_resize(w, edge, orig, mx, my);
                    } else {
                        resize = None;
                    }
                } else {
                    resize = None;
                }
            } else if focused_terminal(&desktop) && down {
                terminal_drag_select(&mut desktop, tr.as_ref(), mx, my, false);
            } else if let Some((i, lx, ly)) = drag {
                // 拖拽进行中：移动窗口 + 更新吸附区
                //
                // **先校验索引**：窗口可能已被**其它路径**关掉（AI 的 close_active、
                // 菜单「关闭窗口」），而它们只 `wins.pop()` 不清 `drag`。若拖的正好是
                // 最后一个窗口，pop 后 `wins[drag.0]` 就出界 —— 而 compositor 是
                // essential + restart:false，**一次 panic 等于桌面永久死掉**（0.8 审计）。
                if i >= desktop.wins.len() {
                    drag = None;
                    snap_zone = layout::Snap::None;
                } else {
                    let w = &mut desktop.wins[i].rect;
                    w.x += (mx - lx) as i32;
                    w.y = (w.y + (my - ly) as i32).max(layout::TOP_BAR);
                    drag = Some((i, mx, my));
                    snap_zone = if desktop.wins[i].floating {
                        layout::detect(mx, my, WIDTH, HEIGHT)
                    } else {
                        layout::Snap::None
                    };
                }
            } else if let Some((idx, min, zoom, close)) = renderer
                .window_lights
                .iter()
                .rev() // 后画的窗口在上层，命中优先
                .find(|(_, mn, z, c)| mn.contains(mx, my) || z.contains(mx, my) || c.contains(mx, my))
                .copied()
            {
                // Windows 三键（预览路径与 fbdev 路径同一套语义）
                let msg = if min.contains(mx, my) {
                    minimize_window(&mut desktop, idx)
                } else if close.contains(mx, my) {
                    // §5.1：红灯关窗也要**可撤销** —— 与 Ctrl+W 走同一条记录方式，
                    // 否则「用哪个方式关的」会决定能不能撤销。
                    match close_window(&mut desktop, idx) {
                        Some((at, message, win)) => {
                            let title = win.title.clone();
                            let _ = undo.keep_closed(win);
                            undo.push(UndoStep::ClosedWindow { at, title });
                            Some(message)
                        }
                        None => None,
                    }
                } else if zoom.contains(mx, my) {
                    toggle_zoom(&mut desktop, idx, layout::work_area(WIDTH, HEIGHT))
                } else {
                    None
                };
                drag = None;
                snap_zone = layout::Snap::None;
                if let Some(m) = msg {
                    push_toast(&mut toast, &mut renderer.notifs, m, notifications::Kind::Info);
                }
            } else if focused_terminal(&desktop)
                && active_term_contains(&desktop, tr.as_ref(), mx, my)
            {
                open_menu = None;
                drag = None;
                terminal_drag_select(&mut desktop, tr.as_ref(), mx, my, true);
            } else if let Some((idx, edge)) = topmost_edge(&desktop, mx, my) {
                open_menu = None;
                resize = begin_resize(&mut desktop, idx, edge);
                drag = None;
                snap_zone = layout::Snap::None;
            } else if my < layout::TOP_BAR as f32 {
                // 1. 菜单栏区域
                let hit_menu = renderer
                    .menubar_menus
                    .iter()
                    .position(|r| r.contains(mx, my));
                if let Some(mi) = hit_menu {
                    open_menu = if open_menu == Some(mi) { None } else { Some(mi) };
                } else if open_menu.is_some() {
                    open_menu = None;
                }
            }
            // 2. 展开中的下拉菜单
            else if let Some((items, _labels)) = renderer.dropdown.clone() {
                let hit = items.iter().position(|r| r.contains(mx, my));
                if let Some(i) = hit {
                    let (t, r) = run_menu_item(&mut desktop, open_menu.unwrap_or(0), i);
                    if let Some(m) = t {
                        push_toast(&mut toast, &mut renderer.notifs, m, notifications::Kind::Info);
                    }
                    if let Some(r) = r {
                        ai_reply = Some((r, draw::BubbleKind::Ai, Instant::now()));
                    }
                }
                open_menu = None;
            }
            // 3. Dock 图标 → 启动应用窗口
            else if let Some(icon) = renderer.dock_icons.iter().position(|r| r.contains(mx, my)) {
                // 这条路径是开发机预览（minifb），不是 Live ISO，所以没有安装向导
                match dock_action(icon, &renderer.installed_apps, false) {
                    DockAction::Builtin(i) => {
                        // 先看有没有**已最小化**的同类窗口：有就恢复，而不是再开一个
                        // （否则用户点了没反应，会以为最小化把窗口弄丢了）
                        let title = APP_TITLES.get(i).copied().unwrap_or("");
                        let mini = desktop
                            .wins
                            .iter()
                            .position(|w| w.title == title && is_minimized(w));
                        match mini {
                            Some(idx) => {
                                restore_if_minimized(&mut desktop, idx);
                                desktop.active = idx;
                            }
                            None => {
                                if let Some(msg) = open_app(&mut desktop, i) {
                                    push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                                }
                            }
                        }
                    }
                    DockAction::Installed(id, name) => {
                        if let Some(msg) = open_installed_app(&mut desktop, id, name) {
                            push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                        }
                    }
                    DockAction::Installer | DockAction::Nothing => {}
                }
            }
            // 3.4 侧栏"上级目录"
            else if let Some((_, i)) = renderer
                .sidebar_hits
                .iter()
                .find(|(r, _)| r.contains(mx, my))
                .copied()
            {
                // 侧栏"位置"：接真实目录
                if let Some((label, path)) = desktop.sidebar.get(i).cloned() {
                    if let Some(msg) = navigate_to(&mut desktop, &path, &label, &label) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
            }
            else if let Some((_, path)) = renderer
                .crumb_hits
                .iter()
                .find(|(r, _)| r.contains(mx, my))
                .cloned()
            {
                // 面包屑：点哪一段就跳到哪一级
                let label = draw::path_leaf(&path);
                if let Some(msg) = navigate_to(&mut desktop, &path, &label, &label) {
                    push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                }
            }
            else if renderer.file_up.contains(mx, my) {
                open_menu = None;
                if let Some(msg) = go_up(&mut desktop) {
                    push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                }
            }
            // 3.5 文件管理器图标 → 点一次选中，再点一次打开
            else if let Some((_, idx)) = renderer.file_cells.iter().find(|(r, _)| r.contains(mx, my)).copied() {
                open_menu = None;
                if desktop.selected == Some(idx) {
                    if let Some(msg) = open_entry(&mut desktop, idx) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                } else {
                    desktop.selected = Some(idx);
                }
            }
            // 4. 窗口标题栏 → 置顶 + 开始拖拽
            else {
                open_menu = None;
                for i in (0..desktop.wins.len()).rev() {
                    let r = desktop.wins[i].rect;
                    let title_hit = Rect { x: r.x, y: r.y, w: r.w, h: metric::TITLE_HIT_H };
                    if title_hit.contains(mx, my) {
                        let clicked = desktop.wins.remove(i);
                        desktop.wins.push(clicked);
                        desktop.active = desktop.wins.len() - 1;
                        let wi = desktop.wins.len() - 1;
                        desktop.wins[wi].floating = true;
                        desktop.wins[wi].target = None;
                        drag = Some((wi, mx, my));
                        break;
                    }
                }
            }
        } else {
            if let Some((i, _, _)) = drag.take() {
                if snap_zone != layout::Snap::None {
                    if let Some(win) = desktop.wins.get_mut(i) {
                        win.target = Some(layout::rect(snap_zone, layout::work_area(WIDTH, HEIGHT)));
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg_lang(|l| snap_zone.label_in(l).to_string()), notifications::Kind::Info);
                    }
                }
                snap_zone = layout::Snap::None;
            }
        }

        // ---- 布局目标 → 缓动动画 ----
        let lay = desktop.layout;
        let work = layout::work_area(WIDTH, HEIGHT);
        let tiled = layout::tiled_targets(desktop.wins.len(), lay, work);
        for (i, win) in desktop.wins.iter_mut().enumerate() {
            if lay != Layout::Float && !win.floating {
                win.target = tiled[i];
            }
            if let Some(tg) = win.target {
                const EASE: f32 = 0.22;
                win.rect.x = draw::lerp(win.rect.x as f32, tg.x as f32, EASE).round() as i32;
                win.rect.y = draw::lerp(win.rect.y as f32, tg.y as f32, EASE).round() as i32;
                win.rect.w = draw::lerp(win.rect.w as f32, tg.w as f32, EASE).round() as i32;
                win.rect.h = draw::lerp(win.rect.h as f32, tg.h as f32, EASE).round() as i32;
                let settled = (win.rect.x - tg.x).abs() < 2
                    && (win.rect.y - tg.y).abs() < 2
                    && (win.rect.w - tg.w).abs() < 2
                    && (win.rect.h - tg.h).abs() < 2;
                if settled {
                    win.rect = tg;
                    win.target = None;
                }
            }
        }

        let snap_preview = drag
            .filter(|_| snap_zone != layout::Snap::None)
            .map(|_| layout::rect(snap_zone, work));
        let toast_now = match toast.take() {
            Some((msg, t0)) if t0.elapsed().as_secs_f32() < 1.6 => {
                Some((msg, t0.elapsed().as_secs_f32()))
            }
            _ => None,
        };
        let reply_now = match ai_reply.take() {
            Some((msg, kind, t0)) if t0.elapsed().as_secs_f32() < 6.0 => {
                Some((msg, kind, t0.elapsed().as_secs_f32()))
            }
            _ => None,
        };

        let confirm_ui = confirm.as_ref().map(|(req, echo)| draw::ConfirmUi {
            tool: &req.tool,
            level: req.level,
            arguments: &req.arguments,
            consequence: &req.consequence,
            echo_required: req.echo_required.as_deref(),
            echo_input: echo,
        });
        let ui = UiState {
            snap: snap_preview,
            toast: toast_now.as_ref().map(|(m, a)| (m.text(), *a)),
            ai_input: &ai_input,
            ai_cursor: ai_cursor.pos(),
            ime: &ime,
            ai_focused: confirm.is_none(),
            ai_thinking: thinking,
            ai_status,
            ai_reply: reply_now.as_ref().map(|(m, k, a)| (m.as_str(), *k, *a)),
            mouse: (mx, my),
            mouse_down: down,
            // 预览模式没有真实音频栈：如实标"未检测"并禁用控件（不画假滑块）
            audio_label: "未检测到音频（预览模式）",
            audio_volume: 0,
            audio_enabled: false,
            open_menu,
            show_installer: false,
            installer: None,
            confirm: confirm_ui,
        };

        // 空闲降帧：没有任何动画/交互时不必跑满 60fps
        let busy = drag.is_some()
            || desktop.wins.iter().any(|w| w.target.is_some())
            || toast_now.is_some()
            || reply_now.is_some()
            || ui.snap.is_some();
        if !busy {
            std::thread::sleep(Duration::from_millis(10));
        }

        refresh_apps_throttled(&mut renderer, &mut last_apps_scan);
        renderer.render_frame(&mut buffer, WIDTH, HEIGHT, t, &desktop, &ui, tr.as_ref());
        window.update_with_buffer(&buffer, WIDTH, HEIGHT)?;
    }
    Ok(())
}


/// 按键 → 字符（预览期英文输入；数字键 1-4 保留给布局）。
#[cfg(not(target_os = "linux"))]
pub(crate) fn key_to_char(k: &minifb::Key) -> Option<char> {
    use minifb::Key::*;
    let c = match k {
        A => 'a',
        B => 'b',
        C => 'c',
        D => 'd',
        E => 'e',
        F => 'f',
        G => 'g',
        H => 'h',
        I => 'i',
        J => 'j',
        K => 'k',
        L => 'l',
        M => 'm',
        N => 'n',
        O => 'o',
        P => 'p',
        Q => 'q',
        R => 'r',
        S => 's',
        T => 't',
        U => 'u',
        V => 'v',
        W => 'w',
        X => 'x',
        Y => 'y',
        Z => 'z',
        Space => ' ',
        Comma => ',',
        Period => '.',
        Minus => '-',
        // 数字与常用符号：曾经整段缺失，导致预览里**连数字都打不出来**（2026-09-29 修复）
        Key0 => '0',
        Key1 => '1',
        Key2 => '2',
        Key3 => '3',
        Key4 => '4',
        Key5 => '5',
        Key6 => '6',
        Key7 => '7',
        Key8 => '8',
        Key9 => '9',
        Semicolon => ';',
        Apostrophe => '\'',
        Slash => '/',
        Backslash => '\\',
        Equal => '=',
        _ => return None,
    };
    Some(c)
}

/// minifb 按键 → 导航键（与 evdev 路径的 `input::nav_key` 语义一一对应）。
#[cfg(not(target_os = "linux"))]
pub(crate) fn minifb_nav(k: &minifb::Key) -> Option<input::NavKey> {
    use minifb::Key::*;
    Some(match k {
        Left => input::NavKey::Left,
        Right => input::NavKey::Right,
        Up => input::NavKey::Up,
        Down => input::NavKey::Down,
        Home => input::NavKey::Home,
        End => input::NavKey::End,
        PageUp => input::NavKey::PageUp,
        PageDown => input::NavKey::PageDown,
        Tab => input::NavKey::Tab,
        Delete => input::NavKey::Delete,
        _ => return None,
    })
}

/// 可拖拽的窗口边缘。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Edge {
    Left,
    Right,
    Bottom,
    BottomLeft,
    BottomRight,
}

/// 鼠标落在哪条边上（纯函数，便于单测）。
///
/// 只有左/右/下/两角：上边是标题栏，它的语义是"拖动窗口"，
/// 抢过来当缩放区会让用户没法拖窗口。
pub(crate) fn edge_at(win: &Win, mx: f32, my: f32) -> Option<Edge> {
    const T: f32 = 6.0; // 抓取厚度：太薄点不中，太厚会挡住内容
    let (l, r, b) = (
        win.rect.x as f32,
        win.rect.x as f32 + win.rect.w as f32,
        win.rect.y as f32 + win.rect.h as f32,
    );
    let inside_y = my >= win.rect.y as f32 - T && my <= b + T;
    let inside_x = mx >= l - T && mx <= r + T;
    if !inside_x || !inside_y {
        return None;
    }
    let near_left = (mx - l).abs() <= T;
    let near_right = (mx - r).abs() <= T;
    let near_bottom = (my - b).abs() <= T;
    match (near_left, near_right, near_bottom) {
        (true, _, true) => Some(Edge::BottomLeft),
        (_, true, true) => Some(Edge::BottomRight),
        (true, _, _) => Some(Edge::Left),
        (_, true, _) => Some(Edge::Right),
        (_, _, true) => Some(Edge::Bottom),
        _ => None,
    }
}

/// 最上层那条边（后画的窗口在上层，命中优先）。
pub(crate) fn topmost_edge(desktop: &Desktop, mx: f32, my: f32) -> Option<(usize, Edge)> {
    for (i, win) in desktop.wins.iter().enumerate().rev() {
        if win.rect.w <= 0 {
            continue;
        }
        if let Some(e) = edge_at(win, mx, my) {
            return Some((i, e));
        }
    }
    None
}

/// 按鼠标位置改窗口矩形。以**按下时的原始矩形**为基准计算，
/// 否则每次移动都在上一次结果上叠加，会产生累积漂移。
///
/// 语义是"被拖的那条边跟着鼠标走"，而不是"在旧尺寸上加减"：
/// 直接由鼠标坐标算出新尺寸/位置，重复应用同一坐标结果恒等。
pub(crate) fn apply_resize(win: &mut Win, edge: Edge, orig: draw::Rect, mx: f32, my: f32) {
    const MIN_W: i32 = 320;
    const MIN_H: i32 = 200;
    let mx = mx.round() as i32;
    let my = my.round() as i32;
    let right = orig.x + orig.w;
    match edge {
        Edge::Right => win.rect.w = (mx - orig.x).max(MIN_W),
        Edge::Bottom => win.rect.h = (my - orig.y).max(MIN_H),
        Edge::Left => {
            // 右边固定：新宽度与原右边一起决定新的 x
            let w = (right - mx).max(MIN_W);
            win.rect.x = right - w;
            win.rect.w = w;
        }
        Edge::BottomRight => {
            win.rect.w = (mx - orig.x).max(MIN_W);
            win.rect.h = (my - orig.y).max(MIN_H);
        }
        Edge::BottomLeft => {
            let w = (right - mx).max(MIN_W);
            win.rect.x = right - w;
            win.rect.w = w;
            win.rect.h = (my - orig.y).max(MIN_H);
        }
    }
    win.rect.w = win.rect.w.max(MIN_W);
    win.rect.h = win.rect.h.max(MIN_H);
    win.rect.y = win.rect.y.max(crate::layout::TOP_BAR);
}

/// 开始缩放：脱离平铺并清掉动画目标（否则下一帧被布局改回去）。
pub(crate) fn begin_resize(desktop: &mut Desktop, idx: usize, edge: Edge) -> Option<(usize, Edge, draw::Rect)> {
    let win = desktop.wins.get_mut(idx)?;
    win.floating = true;
    win.target = None;
    desktop.active = idx;
    Some((idx, edge, win.rect))
}

/// 点击是否落在**活动终端**的内容区里（决定按下是"选择文本"还是别的交互）。
pub(crate) fn active_term_contains(desktop: &Desktop, _tr: Option<&text::TextRenderer>, mx: f32, my: f32) -> bool {
    let Some(win) = desktop.wins.get(desktop.active) else {
        return false;
    };
    let Some(_) = win.term.as_ref() else {
        return false;
    };
    draw::term_content_rect(win.rect).contains(mx, my)
}

/// 终端拖选的每帧推进。`start_new = true` 表示按下那一刻（重设起点）。
///
/// 终端聚焦时，内容区里的按下/拖动是**选择文本**而不是拖动窗口 ——
/// 这与所有终端的行为一致。
pub(crate) fn terminal_drag_select(
    desktop: &mut Desktop,
    tr: Option<&text::TextRenderer>,
    mx: f32,
    my: f32,
    start_new: bool,
) -> bool {
    let cell = match tr {
        Some(t) => t.mono_cell(),
        None => (8.0, 18.0),
    };
    let Some(win) = desktop.wins.get_mut(desktop.active) else {
        return false;
    };
    let Some(t) = win.term.as_mut() else {
        return false;
    };
    let content = draw::term_content_rect(win.rect);
    let (cols, rows) = draw::term_grid_size(win.rect, cell.0, cell.1);
    if !content.contains(mx, my) {
        return false;
    }
    let (c, r) = term::Terminal::cell_at(content, cell.0, cell.1, mx, my, cols, rows);
    t.select_at(c, r, start_new);
    true
}

/// 活动窗口是否是终端 —— 决定键盘归谁。
///
/// 终端是文本界面：它拿到焦点时，按键（尤其是 Ctrl+C/Ctrl+W 这类）
/// 属于 shell 而不属于窗口管理器。否则 `Ctrl+C` 会被当成"关窗"，终端里永远中断不了命令。
pub(crate) fn focused_terminal(desktop: &Desktop) -> bool {
    desktop.wins.get(desktop.active).map(|w| w.term.is_some()).unwrap_or(false)
}

/// 每帧维护终端会话：读 PTY、并把窗口尺寸同步成 winsize。
///
/// 两条渲染路径共用一份（否则会出现"真机能刷新、预览不刷新"这类只在一条路径成立的差异）。
pub(crate) fn maintain_terminals(desktop: &mut Desktop, tr: Option<&text::TextRenderer>) {
    let (cw, ch) = match tr {
        Some(t) => t.mono_cell(),
        None => (8.0, 18.0),
    };
    for win in desktop.wins.iter_mut() {
        if let Some(t) = win.term.as_mut() {
            t.pump();
            let (cols, rows) = draw::term_grid_size(win.rect, cw, ch);
            t.resize(cols, rows);
        }
    }
}

/// evdev 事件 → 终端按键（返回 `None` 表示这个事件与终端无关，交给上层照常处理）。
/// 仅真机（Linux）路径使用 —— 预览路径走 minifb 自己的键映射。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn term_key_from_ui(ev: &input::UiEvent) -> Option<term::TermKey> {
    Some(match ev {
        input::UiEvent::Char(c) => term::TermKey::Char(*c),
        // Ctrl+Shift+X 是剪贴板动作（复制/粘贴），不归 shell —— 返回 None 交给上层
        input::UiEvent::Ctrl(c, false) => term::TermKey::Ctrl(*c),
        input::UiEvent::Ctrl(_, true) => return None,
        input::UiEvent::Enter => term::TermKey::Enter,
        input::UiEvent::Backspace => term::TermKey::Backspace,
        input::UiEvent::Escape => term::TermKey::Escape,
        input::UiEvent::Nav(k) => term::TermKey::Nav(*k),
        _ => return None,
    })
}

/// 把终端按键写进活动窗口的会话。返回是否已消费该按键。
/// 把按键送给活动终端。返回是否真的送进去了（活动窗口不是终端则为 false）。
///
/// **中文输入法在这里介入**（2.3）：被 IME 吃掉的字符不进 PTY —— 否则拼音字母会
/// 直接打到 shell 里，用户看到的是 `nihao` 而不是「你好」。
pub(crate) fn feed_terminal(desktop: &mut Desktop, key: term::TermKey, ime: &mut ime::Ime) -> bool {
    let bytes: Option<Vec<u8>> = match key {
        term::TermKey::Char(c) => {
            let mut committed: Option<String> = None;
            if ime.composing() {
                if let Some(d) = c.to_digit(10).filter(|d| *d >= 1) {
                    committed = ime.select_index(d as usize - 1);
                } else if c == ' ' {
                    committed = ime.commit();
                }
            }
            match committed {
                Some(text) => Some(text.into_bytes()),
                // 未被 IME 吃掉的字符才送 PTY
                None => ime.push(c).map(String::into_bytes),
            }
        }
        // 拼字中退格 = 删拼音，不送 PTY
        term::TermKey::Backspace if ime.backspace() => None,
        other => Some(term::bytes_for(other)),
    };

    let Some(t) = desktop
        .wins
        .get_mut(desktop.active)
        .and_then(|w| w.term.as_mut())
    else {
        return false;
    };
    if let Some(b) = bytes {
        t.write(&b);
    }
    // 键入即清选区（与所有终端一致：开始打字就意味着放弃选择）
    t.clear_selection();
    true
}

/// 切换布局（键盘路径）；返回 toast 文案。
pub(crate) fn set_layout(desktop: &mut Desktop, lay: Layout) -> Option<ToastMsg> {
    if lay == desktop.layout {
        return None;
    }
    desktop.layout = lay;
    for w in &mut desktop.wins {
        w.floating = false;
        w.target = None;
    }
    Some(ToastMsg::new(format!("布局：{}", lay.label_in(crate::i18n::Lang::Zh)), format!("Layout: {}", lay.label_in(crate::i18n::Lang::En))))
}

// ---------------------------------------------------------------------------
// 窗口管理与键盘导航：**两条渲染路径（预览 / fbdev）共用同一份实现**。
// 写在各自的事件循环里迟早会漂移成"预览能关窗、真机不能"这类只在一条路径成立的差异。
// ---------------------------------------------------------------------------

/// 关闭窗口。返回 toast 文案。
///
/// **必须同时修正 `active`**：窗口数组被移除后旧索引会越界。调用方还负责清 `drag`——
/// 拖拽中关掉被拖的窗口正是 P2-3 那类 panic 的来源。
pub(crate) fn close_window(desktop: &mut Desktop, idx: usize) -> Option<(usize, ToastMsg, draw::Win)> {
    if idx >= desktop.wins.len() {
        return None;
    }
    // §5.1：**交出整个窗口**（而不是只要标题）—— 调用方把它放进"最近关闭"托盘，
    // 这样"撤销关闭"能把**同一个窗口连同活会话**放回来。
    let win = desktop.wins.remove(idx);
    let title = win.title.clone();
    if desktop.wins.is_empty() {
        desktop.active = 0;
    } else {
        // 关掉的不在活动项之前时，焦点前移一位；总体再夹一次，防越界
        if desktop.active > idx {
            desktop.active -= 1;
        }
        desktop.active = desktop.active.min(desktop.wins.len() - 1);
    }
    Some((idx, ToastMsg::new(format!("已关闭「{title}」"), format!("Closed \"{title}\"")), win))
}

/// 最大化 / 恢复。用 `restore` 记住原矩形，再点一次回到原位。
pub(crate) fn toggle_zoom(desktop: &mut Desktop, idx: usize, work: draw::Rect) -> Option<ToastMsg> {
    let win = desktop.wins.get_mut(idx)?;
    match win.restore.take() {
        Some(prev) => {
            win.target = Some(prev);
            Some(ToastMsg::new(format!("已恢复「{}」", win.title), format!("Restored \"{}\"", win.title)))
        }
        None => {
            win.restore = Some(win.rect);
            win.floating = true; // 脱离平铺，否则下一帧会被布局覆盖回去
            win.target = Some(work);
            Some(ToastMsg::new(format!("已最大化「{}」", win.title), format!("Maximized \"{}\"", win.title)))
        }
    }
}

/// 活动窗口若是文件管理器，返回它的网格布局（键盘导航要与画出来的一致）。
pub(crate) fn active_files_grid(desktop: &Desktop) -> Option<draw::GridLayout> {
    let win = desktop.wins.get(desktop.active)?;
    if win.kind != draw::WinKind::Files {
        return None;
    }
    Some(draw::grid_layout(draw::files_grid_rect(win.rect)))
}

/// 把 Wayland 会话里已 map 的 surface 同步成桌面窗口（3.1 spike W4）。
///
/// **以 surface 的对象 id 为身份**（`WaylandSurface.surface_id`）：
/// - 新 map 的 surface → 新窗口（置为活动）
/// - 已 destroy / unmap 的 surface → 窗口移除
/// - 已有窗口 → 只更新标题与像素（客户端每帧 commit，不能每帧重建窗口 ——
///   那会把用户摆好的窗口位置全部冲掉）
///
/// 每个 commit 都调一次（渲染主循环里），所以这里必须是**幂等**的：
/// 同一个 session 状态同步两遍，结果必须一样。
#[allow(dead_code)] // Wayland spike 尚未接生产路径；目前只有身份同步测试调用它。
pub fn sync_wayland(desktop: &mut Desktop, session: &wayland::session::Session) {
    // 1. 移除不再 map 的 Wayland 窗口
    desktop.wins.retain(|w| match &w.wayland {
        Some(ws) => session.mapped.contains(&ws.surface_id),
        None => true,
    });
    // 2. 新 map 的 → 建窗口
    for sid in &session.mapped {
        let already = desktop
            .wins
            .iter()
            .any(|w| w.wayland.as_ref().map(|x| x.surface_id) == Some(*sid));
        if already {
            continue;
        }
        let Some(st) = session.surfaces.get(sid) else { continue };
        let Some(role) = &st.role else { continue };
        let Some(pixels) = &st.pixels else { continue };
        // 尺寸来自 buffer（客户端自己决定的大小）
        let (bw, bh) = st
            .attached_buffer
            .and_then(|bid| session.buffers.get(&bid))
            .map(|i| (i.width as u32, i.height as u32))
            .unwrap_or((1, 1));
        let win = Win {
            rect: Rect {
                x: 120,
                y: 120,
                w: bw as i32 + 2,
                h: bh as i32 + metric::TITLE_H + 2,
            },
            target: None,
            title: if role.title.is_empty() {
                role.app_id.clone()
            } else {
                role.title.clone()
            },
            kind: draw::WinKind::Wayland,
            floating: true,
            restore: None,
            preview: None,
            term: None,
            wayland: Some(draw::WaylandSurface {
                surface_id: *sid,
                app_id: role.app_id.clone(),
                width: bw,
                height: bh,
                pixels: pixels.clone(),
            }),
        };
        desktop.wins.push(win);
        desktop.active = desktop.wins.len() - 1;
    }
    // 3. 更新已有窗口（标题 / 像素 / 尺寸），不动位置
    for w in desktop.wins.iter_mut() {
        let Some(ws) = &mut w.wayland else { continue };
        let Some(st) = session.surfaces.get(&ws.surface_id) else { continue };
        if let Some(role) = &st.role {
            if !role.title.is_empty() {
                w.title = role.title.clone();
            }
        }
        if let (Some(bid), Some(pixels)) = (st.attached_buffer, &st.pixels) {
            if let Some(info) = session.buffers.get(&bid) {
                ws.width = info.width as u32;
                ws.height = info.height as u32;
                ws.pixels = pixels.clone();
            }
        }
    }
}

/// 把一段文本插到光标处（逐字符走 `TextCursor`，中文安全）。
pub(crate) fn push_str(input: &mut String, cursor: &mut textview::TextCursor, s: &str) {
    for ch in s.chars() {
        cursor.insert(input, ch);
    }
}

/// 活动窗口若是文本预览，返回 (滚动视图, 总行数, 一屏行数)。
///
/// 三个值一起返回是因为它们必须来自**同一个**窗口状态 —— 分两次借用拿不到。
pub(crate) fn focused_preview(desktop: &mut Desktop) -> Option<(&mut textview::ScrollView, usize, usize)> {
    let idx = desktop.active;
    let w = desktop.wins.get_mut(idx)?;
    if w.kind != draw::WinKind::Preview {
        return None;
    }
    let visible = draw::preview_visible_lines(w.rect);
    let p = w.preview.as_mut()?;
    Some((&mut p.scroll, p.lines.len(), visible))
}

/// 导航键的**三级分流**（2.4）：指令条编辑 → 预览滚动 → 文件列表。
///
/// 优先级是刻意的：
/// 1. 用户正在打字时，左右 / Home / End / Delete 归文本光标（不该被预览或列表抢走）；
/// 2. 活动窗口是文本预览时，翻页键归它的滚动；
/// 3. 其余归文件列表导航。
///
/// 集中在一处而不是散在两个事件循环里 —— 真机（evdev）与预览（minifb）两条路径
/// 各写一套必然漂移。
pub(crate) fn dispatch_nav(
    desktop: &mut Desktop,
    nav: input::NavKey,
    ai_input: &mut String,
    ai_cursor: &mut textview::TextCursor,
    ime: &mut ime::Ime,
    tx: &mpsc::Sender<AiEvent>,
) -> Option<ToastMsg> {
    // IME 正在拼字时，上下键选候选 —— 优先于列表导航与预览滚动
    if ime.composing() {
        match nav {
            input::NavKey::Up => {
                ime.move_selection(-1);
                return None;
            }
            input::NavKey::Down => {
                ime.move_selection(1);
                return None;
            }
            _ => {}
        }
    }
    if !ai_input.is_empty() {
        let len = ai_input.chars().count();
        // 走 `textview::nav_action` 而不是自己 match NavKey ——
        // "哪些键属于文本导航"只应有一处定义，否则迟早和预览/列表的分流漂移
        let handled = match textview::nav_action(nav) {
            Some(textview::TextNav::Left) => {
                ai_cursor.left();
                true
            }
            Some(textview::TextNav::Right) => {
                ai_cursor.right(len);
                true
            }
            Some(textview::TextNav::Home) => {
                ai_cursor.home();
                true
            }
            Some(textview::TextNav::End) => {
                ai_cursor.end(len);
                true
            }
            Some(textview::TextNav::Delete) => {
                ai_cursor.delete(ai_input);
                true
            }
            // PageUp/PageDown 在编辑态留给列表/预览滚动，不归光标
            _ => false,
        };
        if handled {
            return None;
        }
    }
    if let Some((scroll, total, visible)) = focused_preview(desktop) {
        // 窗口尺寸可能变过：先夹取再滚动，否则从越界位置起步会跳一下
        scroll.clamp(total, visible);
        let handled = match nav {
            input::NavKey::PageUp => {
                scroll.page(false, total, visible);
                true
            }
            input::NavKey::PageDown => {
                scroll.page(true, total, visible);
                true
            }
            input::NavKey::Home => {
                scroll.home();
                true
            }
            input::NavKey::End => {
                scroll.end(total, visible);
                true
            }
            _ => false,
        };
        if handled {
            return None;
        }
    }
    // 非编辑态的 Delete：删除文件管理器选中项（走 L2 确认链路，只发起不执行）
    if nav == input::NavKey::Delete {
        return request_delete(desktop, tx);
    }
    apply_nav(desktop, nav)
}

/// 导航键动作：Tab 轮换焦点；方向/翻页/Home/End 在文件网格里移动选择并保持可见。
///
/// `Delete` 不在这里处理 —— 它要么归文本光标（指令条非空），要么走
/// `request_delete`（L2 确认链路）。见 `dispatch_nav`。
pub(crate) fn apply_nav(desktop: &mut Desktop, key: input::NavKey) -> Option<ToastMsg> {
    use input::NavKey::*;
    if key == Tab {
        if desktop.wins.len() < 2 {
            return None;
        }
        desktop.active = (desktop.active + 1) % desktop.wins.len();
        // 切到**已最小化**的窗口时必须把它恢复 —— 这是 Windows 的 Alt+Tab 语义。
        //
        // 两种错法都要避免：
        // ① 不处理 → 焦点落在一个不可见的窗口上，用户看到的是"按了 Alt+Tab 什么都没发生"；
        // ② 直接跳过最小化的窗口 → 最小化的窗口就**键盘不可达**了（Dock 之外没有入口）。
        let idx = desktop.active;
        if desktop.wins.get(idx).map(is_minimized).unwrap_or(false) {
            restore_if_minimized(desktop, idx);
        }
        // 用 get 而不是直接索引：默认桌面现在**没有窗口**，索引会越界 panic
        return Some(ToastMsg::new(
            format!("焦点：{}", desktop.wins.get(desktop.active)?.title),
            format!("Focus: {}", desktop.wins.get(desktop.active)?.title),
        ));
    }
    if key == Delete {
        return None;
    }
    let g = active_files_grid(desktop)?;
    let per_page = g.page_size().max(1);
    let total = desktop.entries.len();
    if total == 0 {
        return None;
    }
    let cur = desktop.selected.unwrap_or(0).min(total - 1);
    let cols = g.cols.max(1);
    let next = match key {
        Down => (cur + cols).min(total - 1),
        Up => cur.saturating_sub(cols),
        Right => (cur + 1).min(total - 1),
        Left => cur.saturating_sub(1),
        PageDown => (cur + per_page).min(total - 1),
        PageUp => cur.saturating_sub(per_page),
        Home => 0,
        End => total - 1,
        _ => cur,
    };
    desktop.selected = Some(next);
    desktop.scroll = draw::scroll_to_show(next, desktop.scroll, per_page);
    None
}

/// 请求 aetherd 控制一个服务（当前只用于「重启网络」）。
///
/// **为什么不在合成器里直接改系统**：改系统状态是 aetherd / init 的职责，合成器只画界面。
/// 所以走 `Request::ServiceControl` —— 协议里本来就有这个变体，**不需要新增 IPC**。
///
/// 与剪贴板同步同一套路：独立线程 + 超时 + 结果回到主循环（绝不阻塞渲染）。
/// 改系统状态必须来自**已注册的 UI 通道**（服务端会 403），所以先 `register_ui`。
///
/// 调用点在 Linux 的 fbdev 循环里（设置命中处理），非 Linux 目标允许未使用。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn control_service(tx: mpsc::Sender<AiEvent>, unit: &'static str, action: aether_ipc::ServiceAction) {
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<Response> {
            let mut stream = IpcStream::connect(Duration::from_secs(2))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(
                aether_ipc::encode(&Request::ServiceControl {
                    unit: unit.to_string(),
                    action,
                    en: crate::i18n::is_en(),
                })
                .as_bytes(),
            )?;
            let line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
            serde_json::from_str::<Response>(line.trim()).map_err(|e| anyhow::anyhow!("解析响应失败: {e}"))
        };
        let ev = match run() {
            Ok(Response::ServiceAck { unit, ok, message }) => AiEvent::ToolDone {
                ok,
                output: format!("{unit}：{message}"),
                origin: ConfirmOrigin::Ai,
            },
            Ok(_) => AiEvent::ToolDone {
                ok: false,
                output: "重启网络：收到意外响应".to_string(),
                origin: ConfirmOrigin::Ai,
            },
            Err(e) => AiEvent::ToolDone {
                ok: false,
                output: format!("重启网络失败：{e}"),
                origin: ConfirmOrigin::Ai,
            },
        };
        let _ = tx.send(ev);
    });
}

/// 把 `input::UiEvent` 映射成面板认识的按键。
///
/// 面板**不直接依赖输入层**（那样它的按键行为就没法脱离 evdev 事件测），
/// 所以这里做一次薄薄的转换 —— 而未识别的键映射成 `None`，由调用方吞掉。
#[cfg(target_os = "linux")]
pub(crate) fn palette_key_of(ev: &input::UiEvent) -> crate::palette::PaletteKey {
    use crate::palette::PaletteKey as K;
    match ev {
        input::UiEvent::Char(c) => K::Char(*c),
        input::UiEvent::Backspace => K::Backspace,
        input::UiEvent::Enter => K::Enter,
        input::UiEvent::Escape => K::Escape,
        // 上下键在输入层走 Nav
        input::UiEvent::Nav(input::NavKey::Up) => K::Up,
        input::UiEvent::Nav(input::NavKey::Down) => K::Down,
        _ => K::Escape, // 兜底不会用到：调用方只在识别出的键上动作（见下）
    }
}

/// 面板的条目表：**三个来源装配**（应用 / 设置页 / 当前目录条目）。
///
/// 文件只取**当前目录**（`desktop.entries`，来自已有的 read_dir）——
/// 全盘递归搜索要索引与权限模型，是另一个量级；这里不做，也不假装做了。
#[cfg(target_os = "linux")]
pub(crate) fn palette_items(desktop: &Desktop) -> Vec<crate::palette::Item> {
    let apps: Vec<&str> = APP_TITLES.iter().copied().collect();
    let pages: Vec<(&str, &str)> = settings::PAGES
        .iter()
        .map(|p| (p.title, p.title_en))
        .collect();
    let entries: Vec<(String, bool)> = desktop
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.is_dir))
        .collect();
    crate::palette::build_items(&apps, &pages, &entries)
}

/// 执行面板选中项。
///
/// 三类的落点都是**已有能力**，面板不新造动作：
/// * 应用 → `open_app`（与点图标同一条路）；
/// * 设置页 → 打开/激活设置窗口并切到那一页（与点左栏同一条路）；
/// * 文件 → `open_entry`（与在文件列表里回车同一条路）。
#[cfg(target_os = "linux")]
pub(crate) fn run_palette_target(
    desktop: &mut Desktop,
    renderer: &mut draw::Renderer,
    target: crate::palette::Target,
    toast: &mut Option<(ToastMsg, std::time::Instant)>,
) {
    use crate::palette::Target;
    match target {
        Target::OpenApp(i) => {
            if let Some(msg) = open_app(desktop, i) {
                push_toast(toast, &mut renderer.notifs, msg, notifications::Kind::Info);
            }
        }
        Target::GotoPage(i) => {
            renderer.settings_page = i.min(settings::PAGES.len().saturating_sub(1));
            // 没有设置窗口就先开一个（索引 4 = 设置，见 `open_app` 里的映射）
            let has = desktop
                .wins
                .iter()
                .any(|w| matches!(&w.kind, draw::WinKind::Settings));
            if !has {
                let _ = open_app(desktop, 4);
            }
            if let Some(idx) = desktop
                .wins
                .iter()
                .position(|w| matches!(&w.kind, draw::WinKind::Settings))
            {
                desktop.active = idx;
            }
        }
        Target::OpenFile(i) => {
            if i < desktop.entries.len() {
                desktop.selected = Some(i);
                if let Some(msg) = open_entry(desktop, i) {
                    push_toast(toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                }
            }
        }
    }
}

/// 锁屏期间的按键处理：**只认解锁需要的键**，其余一律丢弃。
///
/// 放在 Linux 门后：唯一调用点在 fbdev 事件循环里（与锁屏本身同一条路径）。
#[cfg(target_os = "linux")]
pub(crate) fn handle_lock_key(lock: &mut crate::lock::LockScreen, ev: &input::UiEvent) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match ev {
        input::UiEvent::Char(c) => lock.insert(*c),
        input::UiEvent::Backspace => lock.backspace(),
        input::UiEvent::Enter => {
            lock.submit(crate::lock::LOCK_USER, now);
        }
        // Esc 只清空输入、**不解锁**（有人把 Esc 当"取消锁定"，那等于没锁）
        input::UiEvent::Escape => lock.clear(),
        _ => {}
    }
}

/// DNS 输入框的键盘处理。返回 `true` = **已消费**该事件（调用方应跳过其余处理）。
///
/// **只在 Linux 下编译**：唯一调用点在 fbdev 的事件循环里（那是真机路径）。
/// 预览模式（minifb）用的是**另一套输入路径**（直接轮询按键，不走 `UiEvent`），
/// 本轮没有接 —— 也就是说**预览里能看见输入框、但打不进字**。
/// 这一点是缺的，不是"已经能用"；接手的人不要被画出来的框骗到。
///
/// 设计要点：
/// * 回车时**先本地校验**（用共享的纯函数校验器）：不合法的输入**根本不发 IPC** ——
///   没必要为一次注定被拒的请求去连服务端，用户也能立刻看到原因；
/// * 校验通过才发 IPC，并先显示"正在保存…"（写入是异步的，不能假装已经成功）；
/// * 只消费键盘事件，其它一律返回 `false` 交回主循环。
#[cfg(target_os = "linux")]
pub(crate) fn handle_dns_key(
    f: &mut crate::settings::DnsField,
    ev: &input::UiEvent,
    tx: &mpsc::Sender<AiEvent>,
) -> bool {
    match ev {
        input::UiEvent::Char(c) => {
            f.insert(*c);
            true
        }
        input::UiEvent::Backspace => {
            f.backspace();
            true
        }
        input::UiEvent::Enter => {
            match f.validate() {
                Ok(list) => {
                    let v = list
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    set_dns(tx.clone(), v);
                    // 不写"已保存"：这时只是发出去了，结果由 DnsSaved 事件回来
                    f.feedback = Some("正在保存…".to_string());
                }
                Err(why) => {
                    // 校验不过：本地报错，不发 IPC（也**不退出编辑态**，用户接着改）
                    f.feedback = Some(why);
                }
            }
            true
        }
        input::UiEvent::Escape => {
            f.cancel();
            true
        }
        _ => false,
    }
}

/// 扫描可移动设备（§3.7）。
#[cfg(target_os = "linux")]
pub(crate) fn fetch_removables(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::ListRemovable) {
            Ok(Response::RemovableList { devices }) => AiEvent::RemovablesLoaded { devices },
            Ok(Response::Error { code, message }) => AiEvent::RemovableAck {
                ok: false,
                message: format!("扫描可移动设备被拒绝（{code}）：{message}"),
            },
            Ok(_) => AiEvent::RemovableAck {
                ok: false,
                message: "扫描可移动设备：收到意外响应".into(),
            },
            Err(e) => AiEvent::RemovableAck {
                ok: false,
                message: format!("连接 aetherd 失败：{e}"),
            },
        };
        let _ = tx.send(ev);
    });
}

/// 挂载或卸载一个可移动设备（§3.7）。**只传设备名**，能挂不能挂由 aetherd 判。
#[cfg(target_os = "linux")]
pub(crate) fn toggle_removable(tx: mpsc::Sender<AiEvent>, dev: String, mount: bool) {
    std::thread::spawn(move || {
        let req = if mount {
            Request::MountRemovable { dev }
        } else {
            Request::UnmountRemovable { dev }
        };
        let ev = match admin_request(&req) {
            Ok(Response::RemovableAck { ok, message }) => AiEvent::RemovableAck { ok, message },
            Ok(Response::Error { code, message }) => AiEvent::RemovableAck {
                ok: false,
                message: format!("被拒绝（{code}）：{message}"),
            },
            Ok(_) => AiEvent::RemovableAck {
                ok: false,
                message: "挂载/卸载：收到意外响应".into(),
            },
            Err(e) => AiEvent::RemovableAck {
                ok: false,
                message: format!("连接 aetherd 失败：{e}"),
            },
        };
        let _ = tx.send(ev);
    });
}

/// 取一次显示输出（§3.8）。**只读**：本系统不驱动第二个输出，这里只是如实报告。
#[cfg(target_os = "linux")]
pub(crate) fn fetch_displays(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::GetDisplays) {
            Ok(Response::DisplayList { driven_index, outputs }) => {
                AiEvent::DisplaysLoaded { driven_index, outputs }
            }
            // 失败就把列表清空 + 标记已读：界面会显示「没有读到」
            // （比一直显示「正在读取…」诚实 —— 读取确实结束了，只是没成功）
            Ok(Response::Error { code, message }) => {
                eprintln!("aether-compositor: 读取显示输出被拒绝（{code}）：{message}");
                AiEvent::DisplaysLoaded { driven_index: 0, outputs: Vec::new() }
            }
            Ok(_) => AiEvent::DisplaysLoaded { driven_index: 0, outputs: Vec::new() },
            Err(e) => {
                eprintln!("aether-compositor: 读取显示输出失败：{e}");
                AiEvent::DisplaysLoaded { driven_index: 0, outputs: Vec::new() }
            }
        };
        let _ = tx.send(ev);
    });
}

/// 取一次更新列表（§3.1）。
#[cfg(target_os = "linux")]
pub(crate) fn fetch_updates(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::ListUpdates) {
            Ok(Response::UpdateList { packages, current_version, staged_version }) => {
                AiEvent::UpdatesLoaded {
                    packages,
                    current: current_version,
                    staged: staged_version,
                }
            }
            Ok(Response::Error { code, message }) => AiEvent::UpdateResult {
                ok: false,
                message: format!("读取更新列表被拒绝（{code}）：{message}"),
                version: None,
            },
            Ok(_) => AiEvent::UpdateResult {
                ok: false,
                message: "读取更新列表：收到意外响应".into(),
                version: None,
            },
            Err(e) => AiEvent::UpdateResult {
                ok: false,
                message: format!("连接 aetherd 失败：{e}"),
                version: None,
            },
        };
        let _ = tx.send(ev);
    });
}

/// 暂存一个更新包（§3.1）。**版本与哈希由 aetherd 校验** —— 这里只传包名。
#[cfg(target_os = "linux")]
pub(crate) fn stage_update(tx: mpsc::Sender<AiEvent>, name: String) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::StageUpdate { name }) {
            Ok(Response::UpdateStaged { ok, message, version }) => {
                AiEvent::UpdateResult { ok, message, version }
            }
            Ok(Response::Error { code, message }) => AiEvent::UpdateResult {
                ok: false,
                message: format!("被拒绝（{code}）：{message}"),
                version: None,
            },
            Ok(_) => AiEvent::UpdateResult {
                ok: false,
                message: "暂存更新：收到意外响应".into(),
                version: None,
            },
            Err(e) => AiEvent::UpdateResult {
                ok: false,
                message: format!("连接 aetherd 失败：{e}"),
                version: None,
            },
        };
        let _ = tx.send(ev);
    });
}

/// 取一次快照清单（§5.6）。
#[cfg(target_os = "linux")]
pub(crate) fn fetch_snapshots(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::GetSnapshots) {
            Ok(Response::SnapshotList { items }) => AiEvent::SnapshotsLoaded(items),
            Ok(Response::Error { code, message }) => {
                AiEvent::SnapshotDone { ok: false, message: format!("读取快照被拒绝（{code}）：{message}"), notes: Vec::new() }
            }
            Ok(_) => AiEvent::SnapshotDone { ok: false, message: "读取快照：收到意外响应".into(), notes: Vec::new() },
            Err(e) => AiEvent::SnapshotDone { ok: false, message: format!("连接 aetherd 失败：{e}"), notes: Vec::new() },
        };
        let _ = tx.send(ev);
    });
}

/// 创建一个配置快照（§5.6）。
#[cfg(target_os = "linux")]
pub(crate) fn create_snapshot(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::CreateSnapshot) {
            Ok(Response::SnapshotAck { ok, message, notes }) => AiEvent::SnapshotDone { ok, message, notes },
            Ok(Response::Error { code, message }) => {
                AiEvent::SnapshotDone { ok: false, message: format!("被拒绝（{code}）：{message}"), notes: Vec::new() }
            }
            Ok(_) => AiEvent::SnapshotDone { ok: false, message: "创建快照：收到意外响应".into(), notes: Vec::new() },
            Err(e) => AiEvent::SnapshotDone { ok: false, message: format!("连接 aetherd 失败：{e}"), notes: Vec::new() },
        };
        let _ = tx.send(ev);
    });
}

/// 回滚到某个快照（§5.6）。**只应由两步确认的第二步调用。**
#[cfg(target_os = "linux")]
pub(crate) fn restore_snapshot(tx: mpsc::Sender<AiEvent>, id: String) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::RestoreSnapshot { id }) {
            Ok(Response::SnapshotAck { ok, message, notes }) => AiEvent::SnapshotDone { ok, message, notes },
            Ok(Response::Error { code, message }) => {
                AiEvent::SnapshotDone { ok: false, message: format!("被拒绝（{code}）：{message}"), notes: Vec::new() }
            }
            Ok(_) => AiEvent::SnapshotDone { ok: false, message: "回滚：收到意外响应".into(), notes: Vec::new() },
            Err(e) => AiEvent::SnapshotDone { ok: false, message: format!("连接 aetherd 失败：{e}"), notes: Vec::new() },
        };
        let _ = tx.send(ev);
    });
}

/// 取一次时间同步状态（§3.3）。与 `fetch_policy` 同一套路。
#[cfg(target_os = "linux")]
pub(crate) fn fetch_time_status(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::GetTimeStatus) {
            Ok(Response::TimeStatus { timezone, last_offset_ms, last_sync_unix }) => {
                AiEvent::TimeStatus { timezone, last_offset_ms, last_sync_unix }
            }
            Ok(Response::Error { code, message }) => {
                AiEvent::PolicyFailed(format!("读取时间状态被拒绝（{code}）：{message}"))
            }
            Ok(_) => AiEvent::PolicyFailed("读取时间状态：收到意外响应".into()),
            Err(e) => AiEvent::PolicyFailed(format!("连接 aetherd 失败：{e}")),
        };
        let _ = tx.send(ev);
    });
}

/// 立刻对一次时（§3.3）。服务端可能阻塞几秒（等 NTP 响应），所以放独立线程。
#[cfg(target_os = "linux")]
pub(crate) fn sync_time_now(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::SyncTime) {
            // 只取 ok/message：偏移数值服务端已经写进 message（见 TimeSynced 的说明）
            Ok(Response::TimeSync { ok, message, .. }) => AiEvent::TimeSynced { ok, message },
            Ok(Response::Error { code, message }) => AiEvent::TimeSynced {
                ok: false,
                message: format!("被拒绝（{code}）：{message}"),
            },
            Ok(_) => AiEvent::TimeSynced { ok: false, message: "对时：收到意外响应".into() },
            Err(e) => AiEvent::TimeSynced {
                ok: false,
                message: format!("连接 aetherd 失败：{e}"),
            },
        };
        let _ = tx.send(ev);
    });
}

/// 把时区写进系统（§3.3）。
#[cfg(target_os = "linux")]
pub(crate) fn write_timezone(tx: mpsc::Sender<AiEvent>, name: String) {
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::SetTimezone { name }) {
            Ok(Response::SettingAck { ok, message, .. }) => AiEvent::TimezoneSet { ok, message },
            Ok(Response::Error { code, message }) => {
                AiEvent::TimezoneSet { ok: false, message: format!("被拒绝（{code}）：{message}") }
            }
            Ok(_) => AiEvent::TimezoneSet { ok: false, message: "写时区：收到意外响应".into() },
            Err(e) => AiEvent::TimezoneSet { ok: false, message: format!("连接 aetherd 失败：{e}") },
        };
        let _ = tx.send(ev);
    });
}

/// 发一个"管理类"请求并读回执（时间、快照这些共用同一段接线）。
///
/// 原本叫 `time_request` —— 快照也用它之后就改名了：名字要跟着职责走，
/// 否则下一个人会以为它只能问时间。
#[cfg(target_os = "linux")]
pub(crate) fn admin_request(req: &Request) -> anyhow::Result<Response> {
    let mut stream = IpcStream::connect(Duration::from_secs(2))?;
    // 对时要等网络（服务端最长几秒），给足读超时
    stream.set_read_timeout(Some(Duration::from_secs(20))).ok();
    let mut reader = BufReader::new(stream.try_clone()?);
    register_ui(&mut stream, &mut reader)?;
    stream.write_all(aether_ipc::encode(req).as_bytes())?;
    let line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
    aether_ipc::decode::<Response>(&line)
}

/// 取一次权限策略（§5.3）。与 `set_dns` 同一套路：独立线程 + 超时 + 事件回执。
///
/// 需要已注册的 UI 通道（服务端会 403）—— 策略决定"什么能自动执行"，
/// 能读它就知道系统的安全边界长什么样。
#[cfg(target_os = "linux")]
pub(crate) fn fetch_policy(tx: mpsc::Sender<AiEvent>) {
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<Response> {
            let mut stream = IpcStream::connect(Duration::from_secs(2))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(aether_ipc::encode(&Request::GetPolicy).as_bytes())?;
            let line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
            serde_json::from_str::<Response>(line.trim()).map_err(|e| anyhow::anyhow!("解析响应失败: {e}"))
        };
        let ev = match run() {
            Ok(Response::PolicyList { entries }) => AiEvent::PolicyLoaded(entries),
            Ok(Response::Error { code, message }) => {
                AiEvent::PolicyFailed(format!("被拒绝（{code}）：{message}"))
            }
            Ok(_) => AiEvent::PolicyFailed("收到意外响应".into()),
            Err(e) => AiEvent::PolicyFailed(format!("连接 aetherd 失败：{e}")),
        };
        let _ = tx.send(ev);
    });
}

/// 写一个工具的策略（§5.3）。回执里带上"写成了什么"，视图据此更新那一行 ——
/// **本地不先改**：显示成已生效而服务端没写成功，是最难查的一类问题。
#[cfg(target_os = "linux")]
pub(crate) fn save_tool_policy(
    tx: mpsc::Sender<AiEvent>,
    tool: String,
    policy: aether_ipc::ToolPolicyWire,
) {
    std::thread::spawn(move || {
        let value = tool.clone();
        let run = || -> anyhow::Result<Response> {
            let mut stream = IpcStream::connect(Duration::from_secs(2))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(
                aether_ipc::encode(&Request::SetToolPolicy { tool: value, policy }).as_bytes(),
            )?;
            let line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
            serde_json::from_str::<Response>(line.trim()).map_err(|e| anyhow::anyhow!("解析响应失败: {e}"))
        };
        let ev = match run() {
            Ok(Response::PolicyAck { ok, message, tool: t2 }) => AiEvent::PolicySaved {
                ok,
                message,
                tool: t2,
                policy,
            },
            Ok(Response::Error { code, message }) => AiEvent::PolicyFailed(format!(
                "被拒绝（{code}）：{message}（策略未改变）"
            )),
            Ok(_) => AiEvent::PolicyFailed("收到意外响应".into()),
            Err(e) => AiEvent::PolicyFailed(format!("连接 aetherd 失败：{e}")),
        };
        let _ = tx.send(ev);
    });
}

/// 把 DNS 设置写进系统（§2.4 网络写入的界面侧出口）。
///
/// **只在 Linux 下编译**：唯一调用者是 `handle_dns_key`（同样只在 fbdev 路径）。
///
/// **为什么不在合成器里直接写 `/etc/resolv.conf`**：改系统配置是 aetherd 的职责
/// （它做校验、原子写入、审计），合成器只画界面。与「重启网络服务」同一条路子。
///
/// 与剪贴板/服务控制同一套路：**独立线程 + 超时 + 结果回主循环**，绝不阻塞渲染。
/// 服务端会要求"已注册的 UI 通道"，所以先 `register_ui`。
///
/// 结果用 `AiEvent::DnsSaved` 回到主循环 —— **不复用 `ToolDone`**：那会让
/// 一次手动保存显示成 AI 回复。
#[cfg(target_os = "linux")]
pub(crate) fn set_dns(tx: mpsc::Sender<AiEvent>, value: String) {
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<Response> {
            let mut stream = IpcStream::connect(Duration::from_secs(2))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(
                aether_ipc::encode(&Request::SetSetting {
                    key: aether_ipc::SettingKey::Dns,
                    value,
                })
                .as_bytes(),
            )?;
            let line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
            serde_json::from_str::<Response>(line.trim()).map_err(|e| anyhow::anyhow!("解析响应失败: {e}"))
        };
        let ev = match run() {
            Ok(Response::SettingAck { ok, message, .. }) => AiEvent::DnsSaved { ok, message },
            Ok(Response::Error { code, message }) => AiEvent::DnsSaved {
                ok: false,
                message: format!("被拒绝（{code}）：{message}"),
            },
            Ok(_) => AiEvent::DnsSaved { ok: false, message: "收到意外响应".into() },
            Err(e) => AiEvent::DnsSaved { ok: false, message: format!("连接 aetherd 失败：{e}") },
        };
        let _ = tx.send(ev);
    });
}

/// 把本地剪贴板同步给 aetherd（2.2 跨进程剪贴板的落地点）。
///
/// 尽力而为：连不上/超时都**静默放弃** —— 剪贴板同步失败不该打扰用户，
/// 更不该在交互线程里等网络。AI 的 `clipboard_read` 工具读的就是这份状态。
pub(crate) fn sync_clipboard_to_daemon(text: String) {
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<()> {
            let mut stream = IpcStream::connect(Duration::from_secs(2))?;
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            // 剪贴板 IPC 要求已注册 UI 通道（第四轮审查 P1-1）：不注册会被 403。
            //
            // 顺带消掉了"端口劫持"（P2-3）：注册需要 ui.key，冒充 aetherd 的本机进程
            // 拿不到密钥，所以即便抢到 7311 也无法让这条推送成功 —— 服务端那侧会
            // 因密钥不匹配而拒绝。此前这里是"连上就发"，对端身份完全不校验。
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(aether_ipc::encode(&Request::ClipboardSet { text }).as_bytes())?;
            // 读掉一条响应（内容用不上，但必须消费掉才符合"一问一答"的协议）
            read_line_capped(&mut reader, MAX_IPC_LINE)?;
            Ok(())
        };
        let _ = run();
    });
}

/// 复制到剪贴板。目标是终端就复制可见内容，是文件管理器就复制选中项路径。
/// 返回 toast 文案。
pub(crate) fn clipboard_copy(desktop: &mut Desktop) -> Option<ToastMsg> {
    // 终端优先：它的"内容"就是屏幕
    if let Some(t) = desktop.wins.get(desktop.active).and_then(|w| w.term.as_ref()) {
        // 有拖选复制选区，没有则退化为整屏（两条路都不让 Ctrl+Shift+C 落空）
        let text = t.selection_text();
        let trimmed = text.trim_end().to_string();
        let lines = trimmed.lines().count();
        let explicit = t.sel.is_some();
        // 两种语言的"复制了什么"要各写一份 —— 不能把中文的「选区」拼进英文句子
        let (what_zh, what_en) = if explicit { ("选区", "selection") } else { ("终端内容", "terminal contents") };
        desktop.clipboard = trimmed;
        // 只有**显式拖选**的内容才同步给 aetherd。
        //
        // 为什么：无选区时复制的是**整屏**，而终端里出现 `cat /etc/shadow` 的输出、
        // 密码提示、环境变量打印都不罕见。用户随手按一下 Ctrl+Shift+C 不该等于
        // "授权 AI 读取这一屏"（第四轮审查 P3-2）。
        // 本地粘贴不受影响 —— 用户仍能从 desktop.clipboard 粘贴整屏内容。
        if explicit {
            sync_clipboard_to_daemon(desktop.clipboard.clone());
        }
        return Some(ToastMsg::new(
            format!("已复制{what_zh}（{lines} 行）"),
            format!("Copied {what_en} ({lines} lines)"),
        ));
    }
    // 文件管理器：复制选中项的完整路径（比复制文件名有用得多）
    if let Some(i) = desktop.selected {
        if let Some(e) = desktop.entries.get(i) {
            desktop.clipboard = draw::join_path(&desktop.cwd, &e.name);
            sync_clipboard_to_daemon(desktop.clipboard.clone());
            return Some(ToastMsg::new(
                format!("已复制路径「{}」", e.name),
                format!("Copied path \"{}\"", e.name),
            ));
        }
    }
    None
}

/// 粘贴。目标是终端就直接送进 PTY（返回 None 表示已消费）；
/// 否则把文本交回调用方插入输入框。
///
/// 返回的是**剪贴板文本本身**（不是提示文案）—— 类型上必须与
/// `clipboard_copy` 的 `Option<ToastMsg>` 区分开，否则这里会被误当成"已双语"。
pub(crate) fn clipboard_paste(desktop: &mut Desktop) -> Option<String> {
    if desktop.clipboard.is_empty() {
        return None;
    }
    let text = desktop.clipboard.clone();
    if let Some(t) = desktop.wins.get_mut(desktop.active).and_then(|w| w.term.as_mut()) {
        // 应用声明了括号粘贴（DECSET 2004）就用它：多行文本进编辑缓冲，
        // 不会被逐行执行。没声明则退回老行为（见 term::paste_bytes 的说明）。
        // 2026-10-02 审计 M-13：两种模式都会先剥掉控制字符。
        let bracketed = t.bracketed_paste();
        t.write(&term::paste_bytes(&text, bracketed));
        return None;
    }
    Some(text)
}

/// Ctrl 组合键动作。
///
/// 规则：`Ctrl+W` 关窗；`Ctrl+Shift+C/V` 复制粘贴。**带 Shift 的才做剪贴板** ——
/// 终端里 `Ctrl+C` 必须留给 shell（中断命令），不区分 shift 就会两件事抢一个键。
/// `Ctrl+键` 的分类。**纯数据**，由 [`ctrl_intent`] 产出。
///
/// 存在的理由（批次 AR-1）："哪个键会关窗"这件事原本在**两处**各写了一遍 ——
/// `apply_ctrl` 里一处、事件循环判断"要不要清拖拽状态"时又手抄了一处。
/// 两处现在一致，但哪天有人只改一处，就会出现
/// **窗口数组变了而拖拽状态还指着旧下标**（批次 P2-3 那次越界的成因）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlIntent {
    /// `Ctrl+Shift+C`：复制（终端里的复制是另一条路，见 `feed_terminal`）
    Copy,
    /// `Ctrl+Shift+V`：粘贴（换行会被压成空格）
    Paste,
    /// `Ctrl+W`：关闭当前窗口 —— **唯一会改动窗口数组的那个**，
    /// 所以调用方必须据此清掉拖拽与吸附预览
    CloseWindow,
    /// 其余：不做任何事
    Unhandled,
}

/// 判断一个 `Ctrl+键` 想做什么。**判断与执行分开**（同 [`super_intent`] 的理由）。
///
/// 注意这里是**唯一**定义"哪个键做什么"的地方：`apply_ctrl` 按它匹配，
/// 事件循环也用它判断"要不要清拖拽状态"。
pub fn ctrl_intent(c: char, shift: bool) -> CtrlIntent {
    match (c, shift) {
        ('c', true) => CtrlIntent::Copy,
        ('v', true) => CtrlIntent::Paste,
        ('w', false) => CtrlIntent::CloseWindow,
        _ => CtrlIntent::Unhandled,
    }
}

/// 应用向导的一个动作（**键盘与鼠标共用这一条路**）。
///
/// 返回是否要把"向导已完成"落盘（走完与跳过都算完成，见调用方）。
///
/// 为什么抽出来：向导的动作原本写在事件循环的 `Setup` 分支里，
/// 而 §5.8 要给它加鼠标 —— 如果鼠标那条路自己再写一遍，
/// "按 1 选语言"与"点语言按钮"就会是两份实现，迟早分叉。
// 只在 Linux 的输入循环里被调用（键盘与鼠标两条路都在那儿），
// 而它内部用到的 `set_dns` 本身也是 Linux-only —— 按平台门控最诚实。
#[cfg(target_os = "linux")]
pub(crate) fn apply_setup_action(
    act: setup::Action,
    renderer: &mut draw::Renderer,
    ai_tx: &mpsc::Sender<AiEvent>,
    toast: &mut Option<(ToastMsg, Instant)>,
) -> bool {
    let mut mark_done = false;
    match act {
        setup::Action::None => {}
        setup::Action::Next => {
            if renderer.setup.advance() {
                mark_done = true;
            }
        }
        setup::Action::Back => renderer.setup.back(),
        setup::Action::SkipAll => {
            renderer.setup.finish();
            mark_done = true;
        }
        // 下面是"让用户做一次已有的设置操作"——**复用设置中心那条命中路径**
        // （`apply_hit`），本处不复制任何设置逻辑，否则两份必然分叉。
        setup::Action::SetLang(i) => {
            settings::apply_hit(
                &mut renderer.settings,
                &mut renderer.settings_page,
                settings::SettingsHit::Locale(i),
            );
            crate::i18n::set_lang(crate::i18n::Lang::from_locale(renderer.settings.locale));
        }
        setup::Action::TzShift(d) => {
            settings::apply_hit(
                &mut renderer.settings,
                &mut renderer.settings_page,
                settings::SettingsHit::TzShift(d),
            );
        }
        setup::Action::SetDark => {
            settings::apply_hit(
                &mut renderer.settings,
                &mut renderer.settings_page,
                settings::SettingsHit::ToggleDark,
            );
            // 先取值再调用：同一表达式里既 `&mut renderer` 又读它的字段会借不过
            let (dark, hc) = (renderer.settings.dark_mode, renderer.settings.high_contrast);
            apply_theme(renderer, dark, hc);
        }
        setup::Action::SetContrast => {
            settings::apply_hit(
                &mut renderer.settings,
                &mut renderer.settings_page,
                settings::SettingsHit::ToggleContrast,
            );
            let (dark, hc) = (renderer.settings.dark_mode, renderer.settings.high_contrast);
            apply_theme(renderer, dark, hc);
        }
        setup::Action::UseDefaultDns => {
            // 走与设置页同一条出口（`set_dns` → aetherd → resolv.conf）
            set_dns(ai_tx.clone(), setup::DEFAULT_DNS.to_string());
            push_toast(
                toast,
                &mut renderer.notifs,
                toast_msg("正在写入默认 DNS…", "Writing the default DNS…"),
                notifications::Kind::Info,
            );
        }
    }
    mark_done
}

/// 把"向导已完成"落盘。**失败要大声说出来** ——
/// 否则下次开机又弹一遍，而用户不知道为什么。
#[cfg(target_os = "linux")]
pub(crate) fn save_setup_done(renderer: &mut draw::Renderer, toast: &mut Option<(ToastMsg, Instant)>) {
    renderer.settings.setup_done = true;
    if let Err(e) = renderer.settings.save(settings::SETTINGS_PATH) {
        eprintln!("[aether-compositor] 保存向导完成状态失败：{e}");
        push_toast(
            toast,
            &mut renderer.notifs,
            ToastMsg::new(
                format!("向导状态保存失败：{e}"),
                format!("Failed to save wizard state: {e}"),
            ),
            notifications::Kind::Err,
        );
    }
}

/// `apply_ctrl` 的结果。
///
/// 为什么不是一个 `Option<String>`：关窗要额外交出**被关掉的窗口**
///（撤销时要放回来），所以"关窗"必须与"只说了一句话"区分开。
pub(crate) enum CtrlOutcome {
    /// 什么也没做
    Nothing,
    /// 做了一件事，只是一句提示
    Message(ToastMsg),
    /// **关掉了一个窗口** —— 调用方要把 `win` 收进"最近关闭"托盘
    Closed {
        at: usize,
        message: ToastMsg,
        win: Box<draw::Win>,
    },
}

pub(crate) fn apply_ctrl(desktop: &mut Desktop, c: char, shift: bool) -> CtrlOutcome {
    match ctrl_intent(c, shift) {
        // 按 `ctrl_intent` 匹配：**"哪个键做什么"只在这里定义一次**
        CtrlIntent::Copy => clipboard_copy(desktop)
            .map(CtrlOutcome::Message)
            .unwrap_or(CtrlOutcome::Nothing),
        CtrlIntent::Paste => clipboard_paste(desktop)
            .map(|t| {
            // 粘贴到单行输入框：换行折成空格，避免出现看不见的换行
            let one_line: String = t.replace(['\r', '\n'], " ");
            let n = one_line.chars().count();
            desktop.clipboard = one_line;
                CtrlOutcome::Message(toast_msg_1("已粘贴 {} 个字符", "Pasted {} characters", n))
            })
            .unwrap_or(CtrlOutcome::Nothing),
        // **唯一**调用 `close_window` 的地方（有源码探针守着）——
        // 调用方据此清拖拽状态，见 `CtrlIntent::CloseWindow` 的说明
        CtrlIntent::CloseWindow => match close_window(desktop, desktop.active) {
            Some((at, message, win)) => CtrlOutcome::Closed {
                at,
                message,
                win: Box::new(win),
            },
            None => CtrlOutcome::Nothing,
        },
        CtrlIntent::Unhandled => CtrlOutcome::Nothing,
    }
}

/// 截图模式下立即就位（无缓动）。
#[cfg_attr(target_os = "linux", allow(dead_code))] // 仅预览/走查路径使用
pub(crate) fn snap_now(desktop: &mut Desktop, lay: Layout) {
    let work = layout::work_area(WIDTH, HEIGHT);
    let tg = layout::tiled_targets(desktop.wins.len(), lay, work);
    for (i, w) in desktop.wins.iter_mut().enumerate() {
        if let Some(r) = tg[i] {
            w.rect = r;
        }
    }
}
