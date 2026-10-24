//! framebuffer 主循环（从 main.rs 拆出，2026-10-24；行为不变）。

use super::*;

/// 系统内 framebuffer 模式：以屏幕真实分辨率渲染 Essence 桌面。
/// evdev 键盘/鼠标接入（M4）：AI 指令条可输入、Dock/菜单可点击、
/// 窗口可拖拽；Enter 发送指令到 aetherd（离线意图优先），Action 落地为桌面行为。
#[cfg(target_os = "linux")]
fn run_fbdev() -> anyhow::Result<()> {
    tty_log("启动，正在打开 /dev/fb0");
    let mut fb = match fbdev::Fbdev::open() {
        Ok(fb) => fb,
        Err(e) => {
            tty_log(&format!("打开 /dev/fb0 失败: {e}"));
            return Err(e);
        }
    };
    let info = format!("fbdev {}x{} @{}bpp", fb.width, fb.height, fb.bpp);
    println!("aether-compositor: {info}");
    tty_log(&format!("{info} 打开成功，开始渲染"));
    let (w, h) = (fb.width, fb.height);
    let mut buf = vec![0u32; w * h];
    let mut desktop = demo_desktop_sized(w, h);
    let tr = text::TextRenderer::load();
    let mut renderer = draw::Renderer::new(w, h);
    // 用户设置（P3）：从唯一持久分区读入；文件缺失/损坏会回落默认值（见 settings.rs）
    // 用户设置（P3）：从唯一持久分区读入；文件缺失/损坏会回落默认值（见 settings.rs）。
    // 没有设置文件时，默认时区取 `TZ`（镜像可按发行配置定），而不是写死 +8。
    renderer.settings = settings::Settings::load(settings::SETTINGS_PATH, text::tz_offset_min());
    // §5.8 首次启动向导：**只在全新安装时弹**（设置里没记过"已完成"）。
    // 判据与"旧文件缺键视为已完成"的兼容规则合起来，保证升级用户不会被弹窗骚扰。
    renderer.setup = setup::Setup::initial(renderer.settings.setup_done);
    if renderer.setup.open {
        tty_log("首次启动：显示设置向导");
    }
    // Dock 里要显示"已装应用"（见 load_installed_apps）。开机扫一次，
    // 之后由 refresh_apps_throttled 每 ~3 秒重扫 —— 这样"让 AI 装一个应用"之后，
    // Dock 上会自己冒出来，不必重启合成器。
    renderer.installed_apps = load_installed_apps(&apps_root());
    let mut last_apps_scan = Instant::now();
    let (input_tx, input_rx) = mpsc::channel();
    input::spawn(input_tx);
    let (ai_tx, ai_rx) = mpsc::channel::<AiEvent>();
    // Live ISO（有光驱）才显示"安装"Dock 图标
    let live_installer = std::path::Path::new("/dev/sr0").exists();
    let mut installer = Installer::new();

    let start = Instant::now();
    let mut frames: u64 = 0;

    // 输入/UI 状态
    let mut mouse = (w as f32 / 2.0, h as f32 / 2.0);
    let mut mouse_down = false;
    // 本帧待处理的点击（按下-抬起可能同帧到达）
    let mut click_pending = false;
    let mut drag: Option<(usize, f32, f32)> = None;
    // 缩放中的窗口：(索引, 边, 按下时的原始矩形)
    let mut resize: Option<(usize, Edge, draw::Rect)> = None;
    let mut snap_zone = layout::Snap::None;

    // §5.4 虚拟桌面。**被停放的窗口不在 `desktop.wins` 里** —— 这是本设计的要点：
    // 输入、绘制、命中测试都只遍历那个向量，所以别的空间的窗口既画不出来也点不到，
    // 不存在「忘了过滤」这种漏洞（详见 `spaces` 模块的注释）。
    let mut spaces: spaces::Spaces<draw::Win> = spaces::Spaces::new();
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
    let mut open_menu: Option<usize> = None;
    // L2+ 权限确认：待确认请求 + 用户已输入的回显文本
    let mut confirm: Option<(ConfirmRequest, String)> = None;
    let mut ai_status = draw::AiStatus::Local;
    // 是否在等 AI 回复（指令条显示呼吸动效）
    let mut thinking = false;
    // 音频（提示音 + 音量，批次 K）：**探测一次**，不在循环里反复探测
    // （`refresh()` 会起子进程，每帧调用会把 CPU 吃光）
    let mut audio = audio::Audio::probe();
    // 空闲计时（§3.6）：任何输入都重置它。用 `Instant` 而不是累计秒数 ——
    // 累计秒数在丢帧/挂起后会漂，而"上一次输入是什么时候"永远准确。
    let mut last_input = Instant::now();
    // 当前是否已息屏（避免每帧重复写 sysfs）
    let mut idle_blanked = false;
    // 上一帧耗时（§5.10 HUD）：本帧开始时间减去它就是上一帧的真实耗时
    let mut last_frame_ms = 0.0f32;
    tty_log(&format!(
        "音频：{}（音量 {}%{}）",
        audio.state().label(crate::i18n::is_en()),
        audio.volume_pct(),
        if audio.muted() { "，已静音" } else { "" }
    ));

    // §5.1 撤销栈（栈在主循环里，不进 Desktop：Desktop 的构造点有多处，
    // 而撤销栈只被这一个循环使用）
    let mut undo = UndoStack::new();
    loop {
        let frame_start = Instant::now();
        let t = start.elapsed().as_secs_f32();

        // 终端会话维护（读 PTY / 同步 winsize）—— 每帧一次
        maintain_terminals(&mut desktop, tr.as_ref());

        // ---- 1. evdev 输入（drain 本帧积累的全部事件）----
        while let Ok(ev) = input_rx.try_recv() {
            // 任何输入都算"用户还在"（§3.6 息屏）：**先重置计时再处理事件** ——
            // 放后面的话，`continue` 掉的分支（终端按键）就不会重置，
            // 于是"在终端里一直打字"反而会被息屏。
            last_input = Instant::now();
            if idle_blanked {
                // 有输入就立刻亮屏（不等这一帧画完），并**吞掉这一次输入**？
                // 不吞：息屏时用户按的第一个键就是他想按的那个键（吞掉会让人以为键盘失灵）。
                set_fb_blank(false, &mut idle_blanked);
            }
            // §4.1：**一个事件交给谁，只在这一处决定**（顺序即安全决策，见 `key_route`）。
            //
            // 这里曾经把"终端"守卫放在最上面，于是锁屏时按键先落进背后的 shell：
            // 既是锁屏旁路，也让用户按不出解锁密码（安全审计附录 K，高危）。
            let route = key_route(
                renderer.lock.is_locked(),
                renderer.setup.open,
                renderer.palette.open,
                renderer.dns.editing,
                confirm.is_some() || installer.open,
                focused_terminal(&desktop),
            );
            // §2.3：**锁屏时吞掉所有输入** —— 这是「锁屏」的定义。
            // 只处理解锁需要的键，其余（鼠标、快捷键、导航、设置页输入）一律丢弃：
            // 否则锁屏就只是「盖了一层图」，后面的窗口照样能被点到。
            // 放在 match 之前、且排在 DNS 守卫之前：锁着的时候一切输入都归锁屏。
            if matches!(route, KeyRoute::Lock) {
                apply_pointer(&ev, &mut mouse, renderer.settings.mouse_speed_pct as f32 / 100.0, w, h);
                handle_lock_key(&mut renderer.lock, &ev);
                continue;
            }
            // §5.8 首次启动向导：**优先级高于命令面板与 DNS 编辑** ——
            // 开机第一次进来时它是当前唯一该做的事，别的浮层不该抢键。
            if matches!(route, KeyRoute::Setup) {
                // 键盘与鼠标**共用同一个动作应用函数**（`apply_setup_action`）——
                // 否则"按 1 选语言"与"点语言按钮"会各写一份，两份迟早分叉。
                // 向导打开时也必须更新光标位置（否则只有一个鼠标的用户永远出不去：向导只吃 Char/Enter/Escape）
                apply_pointer(&ev, &mut mouse, renderer.settings.mouse_speed_pct as f32 / 100.0, w, h);
                let act = match &ev {
                    input::UiEvent::Char(c) => setup::handle_key(renderer.setup.step, *c),
                    input::UiEvent::Enter => setup::Action::Next,
                    input::UiEvent::Escape => setup::Action::SkipAll,
                    // §5.8：**向导的按钮也能点** —— 命中区由 draw_setup 登记进 setup_hits，
                    // 动作经 setup::action_at 从 options 那张表取出（与按键同源，不分叉）。
                    // 用 MouseUp 而不是 MouseDown：与下文"按下-抬起 = 一次点击"的约定一致（拖出去不算点击）。
// 缺了这条：光标能移动（apply_pointer 已修），但点"中文/English"没反应 ——
// 因为 KeyRoute::Setup 分支在下面那段点击处理**之前**就 continue 了。
                    input::UiEvent::MouseUp => renderer
                        .setup_hit(mouse.0, mouse.1)
                        .map_or(setup::Action::None, |idx| setup::action_at(renderer.setup.step, idx)),
                    _ => setup::Action::None,
                };
                let mark_done = apply_setup_action(act, &mut renderer, &ai_tx, &mut toast);
                if mark_done {
                    save_setup_done(&mut renderer, &mut toast);
                }
                continue;
            }
            // §5.5：命令面板是**模态**的 —— 打开期间所有按键都归它，
            // 未识别的键也**吞掉**（否则 Ctrl+K 打开面板后按 w 会把背后的窗口关掉）。
            if matches!(route, KeyRoute::Palette) {
                match crate::palette::handle_key(&mut renderer.palette, &palette_key_of(&ev)) {
                    crate::palette::PaletteAction::Run(target) => {
                        // 先关面板再执行：执行可能开窗口/切页，留着面板会挡住结果
                        renderer.palette.close();
                        run_palette_target(&mut desktop, &mut renderer, target, &mut toast);
                    }
                    crate::palette::PaletteAction::Close => renderer.palette.close(),
                    // Handled / Ignored 都是"吞掉"
                    _ => {}
                }
                continue;
            }
            // §2.4：设置页正在编辑 DNS 时，**键盘事件先给输入框**。
            // 放在 match 之前而不是改 4 个分支（Char/Backspace/Escape/Enter）：
            // 改 4 处就有 4 处出错机会，守卫只有一处。
            // **只拦键盘** —— 鼠标等事件继续走下面的匹配，否则编辑态下界面会僵住。
            if matches!(route, KeyRoute::Dns) && handle_dns_key(&mut renderer.dns, &ev, &ai_tx) {
                continue;
            }

            // 终端拿到焦点时按键归 shell（否则 Ctrl+C 会被当成关窗、方向键会去翻文件列表）。
            // 放在这里（而不是最前面）：模态浮层与编辑态都该优先于它 ——
            // 原来在最前面时，锁屏 / 向导 / 命令面板 / DNS 输入框都被终端抢了键。
            if matches!(route, KeyRoute::Terminal) {
                if let Some(k) = term_key_from_ui(&ev) {
                    if feed_terminal(&mut desktop, k, &mut ime) {
                        continue;
                    }
                }
            }

            match ev {
                input::UiEvent::MouseMove { dx, dy } => {
                    // 位移在事件流里累积，不受渲染帧率影响。
                    //
                    // 倍率原来是**硬编码 ×2**（早期在 QEMU 里嫌 PS/2 相对计数偏小加的），
                    // 结果在 VMware 与真机上灵敏度过高（用户实测反馈）。现改为 **1:1 默认**，
                    // 倍数交给用户设置（设置中心 → 输入 → 鼠标速度）。
                    let spd = renderer.settings.mouse_speed_pct as f32 / 100.0;
                    mouse.0 = (mouse.0 + dx as f32 * spd).clamp(0.0, (w - 1) as f32);
                    mouse.1 = (mouse.1 + dy as f32 * spd).clamp(0.0, (h - 1) as f32);
                {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static N: AtomicU64 = AtomicU64::new(0);
                    let n = N.fetch_add(1, Ordering::Relaxed) + 1;
                    if n <= 5 || n % 100 == 0 {
                        eprintln!("[cursor] REL 第 {n} 次：({:.0},{:.0}) +({dx},{dy}) 屏 {w}x{h}", mouse.0, mouse.1);
                    }
                }
                }
                // 绝对指针（USB 手写板）：按设备量程线性映射到屏幕像素。
                // 量程退化（max<=min）直接忽略 —— 不做除零。
                input::UiEvent::MouseAbs { x, y, min_x, max_x, min_y, max_y } => {
                    let span_x = (max_x - min_x) as f32;
                    let span_y = (max_y - min_y) as f32;
                    if span_x > 0.0 && span_y > 0.0 {
                        mouse.0 = ((x - min_x) as f32 / span_x * (w - 1) as f32)
                            .clamp(0.0, (w - 1) as f32);
                        mouse.1 = ((y - min_y) as f32 / span_y * (h - 1) as f32)
                            .clamp(0.0, (h - 1) as f32);
                        {
                            use std::sync::atomic::{AtomicU64, Ordering};
                            static N: AtomicU64 = AtomicU64::new(0);
                            let n = N.fetch_add(1, Ordering::Relaxed) + 1;
                            if n <= 5 || n % 100 == 0 {
                                eprintln!("[cursor] ABS 第 {n} 次：({:.0},{:.0}) 屏 {w}x{h}", mouse.0, mouse.1);
                            }
                        }
                    }
                }
                input::UiEvent::MouseDown => mouse_down = true,
                input::UiEvent::Wheel { dy } => {
                    // P1：滚轮此前**完全没有映射**（长列表只能一页页翻）。
                    // 归"当前活动窗口"的内容：文件网格滚动。
                    //
                    // `visible = 1`：滚轮可以一路滚到列表末尾，真实可见区间由状态条显示
                    // （"1–8 / 76"）。这里刻意**不复制一份网格页大小的算法**——那会与
                    // draw.rs 的布局常量各算一套、迟早漂移。
                    // 控制中心开着时**先滚它**（面板是覆盖层）；否则走原来的文件网格。
                    if renderer.control_open {
                        let max = (renderer.control_content_h - renderer.control_panel.h).max(0);
                        renderer.control_scroll = (renderer.control_scroll
                            - dy * widgets::WHEEL_LINES as i32 * draw::theme::size::px(20))
                            .clamp(0, max);
                    } else {
                    let active_is_files = desktop
                        .wins
                        .get(desktop.active)
                        .map(|x| matches!(&x.kind, crate::draw::WinKind::Files))
                        .unwrap_or(false);
                    if active_is_files {
                        desktop.scroll = widgets::scroll_apply(
                            desktop.scroll,
                            dy,
                            desktop.entries.len(),
                            1,
                            widgets::WHEEL_LINES,
                        );
                    }
                    }
                }
                input::UiEvent::MouseUp => {
                    mouse_down = false;
                let was_drag = drag.is_some();
                if let Some((i, _, _)) = drag.take() {
                    if snap_zone != layout::Snap::None {
                        if let Some(win) = desktop.wins.get_mut(i) {
                            win.target = Some(layout::rect(snap_zone, layout::work_area(w, h)));
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg_lang(|l| snap_zone.label_in(l).to_string()), notifications::Kind::Info);
                        }
                    }
                }
                    snap_zone = layout::Snap::None;
                    // 非拖拽的按下-抬起 = 一次点击。QMP/真机都可能把 down+up
                    // 打进同一批事件，在帧间锁存，否则快速点击会被吞掉。
                    if !was_drag {
                        // §5.8 鼠标点选：向导（若开着）是模态的，点击先归它 ——
                        // 与键盘的 `KeyRoute::Setup` 同序。走的是与键盘**同一个**
                        // `apply_setup_action`，所以"按 1 选语言"与"点语言按钮"不会分叉。
                        let wizard_action = if renderer.setup.open {
                            renderer
                                .setup_hit(mouse.0, mouse.1)
                                .and_then(|i| crate::setup::options(renderer.setup.step).get(i).map(|o| o.2))
                        } else {
                            None
                        };
                        if let Some(act) = wizard_action {
                            let mark_done =
                                apply_setup_action(act, &mut renderer, &ai_tx, &mut toast);
                            if mark_done {
                                save_setup_done(&mut renderer, &mut toast);
                            }
                        } else if renderer.setup.open {
                            // 向导开着但点的是别处：**吞掉**这次点击（模态语义），
                            // 不让它穿透到背后的窗口
                        } else if let Some(item) = renderer.palette_hit(mouse.0, mouse.1) {
                            // 与键盘的守卫顺序同一道理（`KeyRoute::Palette`）：
                            // 面板吃到就不置 `click_pending`，否则点击会穿透到背后的窗口。
                            if let Some(target) = renderer.palette.activate_item(item) {
                                // 与键盘回车**完全同一条路**：先关面板再执行
                                renderer.palette.close();
                                run_palette_target(&mut desktop, &mut renderer, target, &mut toast);
                            }
                        } else {
                            click_pending = true;
                        }
                    }
                }
                input::UiEvent::Super(c, shift) => {
                    // §4.1：**判断交给纯函数**（`input::super_intent`），这里只执行结果。
                    //
                    // 为什么这样分：这个分支历史上出过三次问题（审计附录 K / O / P），
                    // 三次的共同点都是"判断写在执行里" —— 无条件调用、写死常量、顺序放错。
                    // 抽成纯函数之后：那些状态组合都被单元测试钉住；
                    // 执行侧也**不再有机会塞进一句无条件调用**（没有条件可塞了）。
                    match input::super_intent(c, shift, spaces.count()) {
                        // 锁屏：最高优先级（顺序由 `super_intent` 的测试守着）
                        input::SuperIntent::Lock => {
                            renderer.lock.lock();
                            tty_log("屏幕已锁定（Super+L）");
                        }
                        // 把当前窗口搬到第 N 个桌面
                        input::SuperIntent::MoveToSpace(i) => {
                            // 窗口数组变了：拖拽与吸附预览必须一起清掉
                            //（那些状态属于上一个空间的窗口 —— 与关窗同一类问题）
                            if spaces.move_active(i, &mut desktop.wins, &mut desktop.active) {
                                drag = None;
                                snap_zone = layout::Snap::None;
                                push_toast(
                                    &mut toast,
                                    &mut renderer.notifs,
                                        toast_msg_1("已移到桌面 {}", "Moved to desktop {}", i + 1),
                                    notifications::Kind::Info,
                                );
                            } else {
                                push_toast(
                                    &mut toast,
                                    &mut renderer.notifs,
                                        toast_msg("没有可移动的窗口", "No window to move"),
                                    notifications::Kind::Info,
                                );
                            }
                        }
                        // 切到第 N 个桌面
                        input::SuperIntent::SwitchSpace(i) => {
                            if spaces.switch(i, &mut desktop.wins, &mut desktop.active) {
                                drag = None;
                                snap_zone = layout::Snap::None;
                                // 自检：切完之后"当前空间那一格必须是空的"（不变量）。
                                // 只在调试构建里查，代价为零；真出问题时能立刻指出是哪一步破坏的。
                                debug_assert!(spaces.invariant_holds(), "虚拟桌面不变量被破坏");
                                // 提示带上"第几个/共几个 + 这个桌面上有几个窗口"——
                                // 只说"切过去了"信息太少，用户会想知道自己到了哪儿、那儿有什么
                                push_toast(
                                    &mut toast,
                                    &mut renderer.notifs,
                                    // 两种语言各写一次（`ToastMsg::new` 不做翻译，只把两份都留着）
                                    ToastMsg::new(
                                        format!(
                                            "桌面 {}/{} · {} 个窗口",
                                            spaces.current() + 1,
                                            spaces.count(),
                                            spaces.len_of(spaces.current(), desktop.wins.len()),
                                        ),
                                        format!(
                                            "Desktop {}/{} · {} windows",
                                            spaces.current() + 1,
                                            spaces.count(),
                                            spaces.len_of(spaces.current(), desktop.wins.len()),
                                        ),
                                    ),
                                    notifications::Kind::Info,
                                );
                            }
                        }
                        // 其余 Super 组合键：**明确忽略** —— 兜底的作用就在这里，
                        // 但输入层已保证它们不会漏成普通字符（不会退回成 `Char`）
                        input::SuperIntent::Ignore => {}
                    }
                }
                input::UiEvent::Char(c) => {
                    // Ctrl+Space 在 evdev 侧通常落成 NUL —— 用它当中英切换键。
                    // 换成显式的 UiEvent 变体更好，但那要改 input.rs 的采集层；
                    // 这里特判的代价小得多，且行为可预期。
                    if c == '\0' {
                        ime.toggle();
                        push_toast(&mut toast, &mut renderer.notifs, if ime.enabled() { toast_msg("中文输入：开", "Chinese IME: on") } else { toast_msg("中文输入：关", "Chinese IME: off") }, notifications::Kind::Info);
                    } else if let Some((req, echo)) = confirm.as_mut() {
                        // 确认弹窗独占键盘：L3 的回显输入写进弹窗而不是指令条
                        if req.echo_required.is_some() {
                            echo.push(c);
                        }
                    } else if renderer.pass_form.old.editing
                        || renderer.pass_form.new.editing
                        || renderer.pass_form.confirm.editing
                    {
                        // §2.3：口令框正在编辑 —— 字符进**当前聚焦的框**，不进 AI 指令条。
                        // 已知限制：不做 IME 拼字（口令是 ASCII 场景），与审计搜索框同一条取舍。
                        if renderer.pass_form.focused().insert(c) {
                            renderer.pass_form.feedback = None;
                        }
                    } else if renderer.audit_editing {
                        // §5.2：搜索框正在编辑 —— 字符进搜索词，**不进 AI 指令条**
                        //（以前的后果会是：想搜一条审计记录，结果把词发给了 AI）。
                        //
                        // 已知限制：这里不做 IME 拼字（搜索目标是工具名/结论，都是 ASCII），
                        // 中文输入法在搜索框里不生效 —— 如实记在这里，不假装支持。
                        renderer.audit_query.push(c);
                    } else if !installer.open {
                        // 向导打开时键盘让位给向导，字符不进指令条。
                        // IME 拼字中：数字 1-6 选候选、空格提交首个；其余走正常流程。
                        let mut committed: Option<String> = None;
                        if ime.composing() {
                            if let Some(d) = c.to_digit(10).filter(|d| *d >= 1) {
                                committed = ime.select_index(d as usize - 1);
                            } else if c == ' ' {
                                committed = ime.commit();
                            }
                        }
                        match committed {
                            Some(text) => push_str(&mut ai_input, &mut ai_cursor, &text),
                            // 未被 IME 吃掉的字符直接进正文
                            None => {
                                if let Some(text) = ime.push(c) {
                                    push_str(&mut ai_input, &mut ai_cursor, &text);
                                }
                            }
                        }
                    }
                }
                input::UiEvent::Backspace => {
                    // §2.3：口令框优先吃掉退格（`continue` 回到事件循环，不再落到下面的分支）
                    if renderer.pass_form.old.editing
                        || renderer.pass_form.new.editing
                        || renderer.pass_form.confirm.editing
                    {
                        renderer.pass_form.focused().backspace();
                        renderer.pass_form.feedback = None;
                        continue;
                    }
                    match confirm.as_mut() {
                        Some((_, echo)) => {
                            echo.pop();
                        }
                        None => {
                            if renderer.audit_editing {
                                // §5.2：搜索框编辑时，退格删搜索词（不是 AI 指令条）
                                renderer.audit_query.pop();
                            } else if !ime.backspace() {
                                // IME 正在拼字时，退格删拼音串而不是正文
                                ai_cursor.backspace(&mut ai_input);
                            }
                        }
                    }
                }
                input::UiEvent::Escape => {
                    // §5.2：搜索框编辑时，Esc **先清搜索词**；已经是空的才退出编辑态
                    //（与主流搜索框一致：一次 Esc 清词、再一次退出）
                    if renderer.audit_editing {
                        if renderer.audit_query.is_empty() {
                            renderer.audit_editing = false;
                        } else {
                            renderer.audit_query.clear();
                        }
                        continue;
                    }
                    // 控制中心（P4）：Esc 一律先关它（浮层的常规语义）
                    renderer.control_open = false;
                    // IME 拼字中：Esc 先取消拼字（与主流输入法一致），
                    // 而不是顺手把确认弹窗也拒了
                    if ime.composing() {
                        ime.cancel();
                    } else {
                    open_menu = None;
                    if let Some((req, _)) = confirm.take() {
                        // 拒绝路径：回传服务端撤销令牌并落审计（P1-9）。
                        // 只改本地状态是不够的——令牌会在服务端继续存活到过期，
                        // 且"用户拒绝过"不会留下任何痕迹。
                        tty_log(&format!("权限确认被拒绝: {}", req.tool));
                        cancel_confirm(&req.token);
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("已拒绝「{}」", "Denied 「{}」", req.tool), notifications::Kind::Err);
                    } else if installer.open && !installer.running {
                        installer.open = false;
                    }
                    }
                }
                input::UiEvent::LayoutKey(n) => {
                    // `layout::for_key` 是**纯函数**（有单测）：越界/0 都不 panic，
                    // 且与 `Layout::ALL` 同源 —— 见它的注释（批次 AP-1）
                    if let Some(msg) = set_layout(&mut desktop, layout::for_key(n)) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
                input::UiEvent::Ctrl(c, shift) => {
                    // §5.1 撤销：`Ctrl+Z` 在这里直接处理（不进 `apply_ctrl` ——
                    // 撤销要访问撤销栈，而 `apply_ctrl` 的签名里没有它）
                    if c == 'z' && !shift {
                        if let Some(msg) = undo_last(&mut desktop, &mut undo) {
                            push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                        }
                        continue;
                    }
                    // §5.5 命令面板：`Ctrl+K`。
                    // 打开时**快照**三个来源（应用 / 设置页 / 当前目录条目）：
                    // 面板开着的时候文件列表变了，也不该让候选在脚下变动。
                    if c == 'k' && !shift {
                        let items = palette_items(&desktop);
                        renderer.palette.open_with(items);
                        continue;
                    }
                    // 关窗会改动窗口数组：拖拽状态必须一起清掉（P2-3 同类越界）。
                    // **"哪个键会关窗"由 `ctrl_intent` 定义**（与 `apply_ctrl` 同源）——
                    // 原来是手抄的 `!shift && c == 'w'`，两处会各自漂移。
                    let intent = ctrl_intent(c, shift);
                    let closed = matches!(intent, CtrlIntent::CloseWindow);
                    match apply_ctrl(&mut desktop, c, shift) {
                        CtrlOutcome::Closed { at, message, win } => {
                            // 关窗改动了窗口数组：拖拽与吸附预览必须清掉
                            drag = None;
                            snap_zone = layout::Snap::None;
                            // §5.1：窗口进"最近关闭"托盘（**不释放**），并记一步可撤销。
                            // 托盘满时最旧的那个被丢 —— 它那一步撤销会如实报"无法撤销"。
                            let title = win.title.clone();
                            let _ = undo.keep_closed(*win);
                            undo.push(UndoStep::ClosedWindow { at, title });
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                message,
                                notifications::Kind::Info,
                            );
                        }
                        CtrlOutcome::Message(msg) => {
                            if closed {
                                drag = None;
                                snap_zone = layout::Snap::None;
                            }
                            push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                        }
                        CtrlOutcome::Nothing => {}
                    }
                }
                input::UiEvent::Nav(n) => {
                    if let Some(msg) =
                        dispatch_nav(&mut desktop, n, &mut ai_input, &mut ai_cursor, &mut ime, &ai_tx)
                    {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
                input::UiEvent::Enter => {
                    // §2.3：口令框编辑中 —— 回车是"下一个框"，到最后一个框才提交。
                    // （`UiEvent` 里本来没有 Tab 变体，用回车走完三个框最省事。）
                    if renderer.pass_form.old.editing
                        || renderer.pass_form.new.editing
                        || renderer.pass_form.confirm.editing
                    {
                        if renderer.pass_form.focus < 2 {
                            renderer.pass_form.focus_next();
                            renderer.pass_form.focused().begin();
                        } else {
                            submit_password(&mut renderer, &mut toast, ai_tx.clone());
                        }
                        continue;
                    }
                    // §4.1：**优先级交给纯函数**（`input::enter_intent`），这里只执行结果。
                    // 卡片 > 向导 > 指令条 的顺序由它的单元测试守着 ——
                    // 这类"顺序"逻辑写错时界面完全看不出来（附录 K / O 的教训）。
                    let has_confirm = confirm.is_some();
                    let echo_ok = confirm
                        .as_ref()
                        .map(|(req, echo)| req.echo_ok_input(echo))
                        .unwrap_or(false);
                    let intent = input::enter_intent(
                        has_confirm,
                        echo_ok,
                        installer.open,
                        ai_input.trim().is_empty(),
                    );
                    match intent {
                        // 放行：走到这里说明 `echo_ok` 为真，卡片一定还在。
                        // 用 `if let` 而不是 `unwrap()` —— 事件循环里不留 panic 点。
                        input::EnterIntent::Approve => {
                            if let Some((req, _)) = confirm.take() {
                                tty_log(&format!("权限确认通过: {}", req.tool));
                                dispatch_approved(req, &ai_tx);
                            }
                        }
                        input::EnterIntent::EchoMismatch => {
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg("回显不匹配：请原样输入目标名", "Echo mismatch: type the target name exactly"),
                                notifications::Kind::Err,
                            );
                        }
                        input::EnterIntent::StartInstall => {
                            if begin_install(&mut installer, &ai_tx) {
                                tty_log("安装向导 → 开始安装（键盘确认）");
                            }
                        }
                        input::EnterIntent::SendAi => {
                            let text = ai_input.trim().to_string();
                            ai_input.clear();
                            ai_cursor.home(); // 发送后光标回到开头
                            // 先把自己的话显示成"用户"气泡，再进入思考态
                            ai_reply =
                                Some((text.clone(), draw::BubbleKind::User, Instant::now()));
                            thinking = true;
                            let tx = ai_tx.clone();
                            std::thread::spawn(move || query_aether(text, 15, tx));
                        }
                        // 空回车：什么都不做（**明确的兜底**，不是"忘了处理"）
                        input::EnterIntent::Nothing => {}
                    }
                }
            }
        }

        // ---- 1.35 通知记录开关（§3.2）----
        // 每帧同步一次：设置页改了开关，下一次进 toast 就生效，不必重启。
        // 放在这里而不是 push_toast 内部：那需要给 47 个调用点都加一个参数。
        renderer.notifs.set_enabled(renderer.settings.record_notifications);

        // ---- 1.4 性能 HUD（§5.10）----
        // 每帧算一次：帧时间取**上一帧**的耗时（本帧还没画完，拿不到自己的耗时），
        // 内存读 /proc/self/statm（拿不到就如实写"不可用"，绝不编数字）。
        let frame_ms = last_frame_ms;
        last_frame_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        renderer.perf_text = if renderer.settings.perf_hud {
            Some(perf_hud_text(frame_ms, &rss_label()))
        } else {
            None
        };

        // ---- 1.5 空闲息屏（§3.6）----
        // 放在输入之后、渲染之前：这样"这一帧刚有输入"与"该不该息屏"
        // 用的是同一份计时，不会出现"刚动鼠标却又立刻息屏"。
        if renderer.settings.should_blank(last_input.elapsed()) {
            set_fb_blank(true, &mut idle_blanked);
        }

        // ---- 2. AI 事件轮询（非阻塞）----
        while let Ok(ev) = ai_rx.try_recv() {
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
                    // 挂载状态变了 → **重新扫一次**（列表如实反映磁盘，而不是本地猜）
                    if ok {
                        fetch_removables(ai_tx.clone());
                    }
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
                        fetch_snapshots(ai_tx.clone());
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
                        fetch_time_status(ai_tx.clone());
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
                        fetch_time_status(ai_tx.clone());
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
                        // 本地通道、未知通道均按本地展示（离线由 Error 事件接管）
                        _ => draw::AiStatus::Local,
                    };
                    thinking = false;
                    ai_reply = Some((text, draw::BubbleKind::Ai, Instant::now()));
                    // 提示音（批次 K）：AI 回复完成 = 正反馈。可关（settings.notify_sound），
                    // 无音频设备时 play() 自己会静默返回（界面上已经如实显示过了）
                    if renderer.settings.notify_sound {
                        audio.play_with(audio::SoundTheme::of(renderer.settings.sound_theme), audio::Cue::Done);
                    }
                }
                AiEvent::Error(text) => {
                    // aetherd 不可达/通道故障 → 顶栏转"AI 离线"（灰）
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
                    tty_log(&format!("权限确认请求: {} L{}", req.tool, req.level));
                    thinking = false;
                    confirm = Some((*req, String::new()));
                    // 提示音（批次 K）：确认卡片需要用户注意 —— 这是三种提示音里
                    // 唯一"必须被听见"的一种（用户可能正在看别处）
                    if renderer.settings.notify_sound {
                        audio.play_with(audio::SoundTheme::of(renderer.settings.sound_theme), audio::Cue::Confirm);
                    }
                }
                AiEvent::ToolDone { ok, output, origin } => {
                    thinking = false;
                    let last = output.lines().last().unwrap_or("").to_string();
                    // 失败要**听得出来**（批次 K）：与成功音不同音高/时长，
                    // 这样用户不必盯着屏幕也知道"刚才那下没成"
                    if !ok && renderer.settings.notify_sound {
                        audio.play_with(audio::SoundTheme::of(renderer.settings.sound_theme), audio::Cue::Error);
                    }
                    match origin {
                        ConfirmOrigin::Installer => {
                            installer.running = false;
                            installer.message = Some((if last.is_empty() {
                                // 空结果 = 装完了；这句要跟着语言
                                crate::i18n::pick(crate::i18n::lang(), "安装完成", "Install finished")
                                    .to_string()
                            } else {
                                last.clone()
                            }, !ok));
                        }
                        ConfirmOrigin::Ai => {
                            // 工具调用结果单独一类气泡：让"AI 干了什么"可见
                            let mark = if ok { "✓" } else { "✗" };
                            ai_reply = Some((format!("{mark} {last}"), draw::BubbleKind::Tool, Instant::now()));
                        }
                        ConfirmOrigin::Power => {
                            // 电源没有对话气泡可挂：只进 toast。
                            // 文案要说清"受理 ≠ 已关机"——真正断电时进程已经没了。
                            let mark = if ok { "✓" } else { "✗" };
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                // 回执文本来自 aetherd（已按其连接语言渲染）
                                ToastMsg::from_external(format!("{mark} {last}")),
                                notifications::Kind::Info,
                            );
                        }
                        ConfirmOrigin::Store => {
                            let mark = if ok { "✓" } else { "✗" };
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                // 回执文本来自 aetherd（已按其连接语言渲染）
                                ToastMsg::from_external(format!("{mark} {last}")),
                                if ok { notifications::Kind::Ok } else { notifications::Kind::Err },
                            );
                            if ok {
                                // 装成功了：**重新读一遍已安装列表**，那一行才会变成「已安装」；
                                // 同时刷新 Dock 的图标（新应用要出现在那里）。
                                // 不"本地假装已装"：万一实际没装成，界面会一直骗人。
                                // 商店那份（id, 版本）与 Dock 那份（id, 名字）**各读各的**：
                                // 它们的语义不同，混用正是 P-1 那个 bug 的成因。
                                renderer.store.refresh_installed(&apps_root());
                                renderer.installed_apps = load_installed_apps(&apps_root());
                            }
                        }
                    }
                    // 电源的结果已经在上面用 toast 说清楚了；再加一句"已执行 · …"
                    // 会让人误以为"已经关机完成"（而实际只是受理）
                    if ok && origin != ConfirmOrigin::Power {
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("已执行 · {}", "Executed · {}", last.chars().take(24).collect::<String>()), notifications::Kind::Info);
                    }
                }
            }
        }

        // ---- 3. 鼠标交互 ----
        let press = mouse_down || click_pending;
        // 3.0 权限确认弹窗是模态，独占命中（允许一次 / 拒绝）
        if confirm.is_some() {
            if press {
                let hit = renderer
                    .confirm_buttons
                    .iter()
                    .find(|(r, _)| r.contains(mouse.0, mouse.1))
                    .map(|(_, b)| *b);
                let echo_ok = confirm.as_ref().map(|(r, e)| r.echo_ok_input(e)).unwrap_or(false);
                match hit {
                    Some(draw::ConfirmButton::Allow) if echo_ok => {
                        if let Some((req, _)) = confirm.take() {
                            tty_log(&format!("权限确认通过: {}", req.tool));
                            dispatch_approved(req, &ai_tx);
                        }
                    }
                    Some(draw::ConfirmButton::Allow) => {
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg("回显不匹配：请原样输入目标名", "Echo mismatch: type the target name exactly"), notifications::Kind::Info);
                    }
                    Some(draw::ConfirmButton::Deny) => {
                        if let Some((req, _)) = confirm.take() {
                            tty_log(&format!("权限确认被拒绝: {}", req.tool));
                            cancel_confirm(&req.token);
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("已拒绝「{}」", "Denied 「{}」", req.tool), notifications::Kind::Info);
                        }
                    }
                    None => {}
                }
            }
        } else if installer.open {
            if press && !installer.running {
                if renderer.installer_close.contains(mouse.0, mouse.1) {
                    // 关闭向导：Esc 之外的**可见**入口（用户反馈"点开就关不掉"）
                    installer.open = false;
                    tty_log("安装向导 → 关闭（右上角按钮）");
                } else if let Some(i) = renderer
                    .installer_rows
                    .iter()
                    .position(|(r, _, _)| r.contains(mouse.0, mouse.1))
                {
                    installer.selected = i;
                } else if renderer.installer_button.contains(mouse.0, mouse.1)
                    && begin_install(&mut installer, &ai_tx)
                {
                    tty_log("安装向导 → 开始安装（按钮确认）");
                }
            }
        } else if {
            // 控制中心是浮层：展开时它优先消费点击（含"点别处关闭"）。
            // 同样用一次性信号 —— 用电平信号会每帧翻转一次开关，面板疯狂闪。
            // 判定与执行分开：`control_center_click` 只判定（可单测），
            // 电源/注销这类"有副作用"的意图在这里执行。
            let mut intent = None;
            let consumed = control_center_click(
                &mut renderer,
                &mut desktop,
                &mut ime,
                mouse.0,
                mouse.1,
                click_pending,
                &mut intent,
            );
            if let Some(i) = intent {
                // §5.4 的增删桌面**在这里执行**（而不是塞进 `apply_ui_intent`）：
                // 它们要动的是循环里的 `desktop`/`spaces`/`drag`/`snap_zone`，
                // 而那个函数里一个都没有 —— 硬塞进去就得改它的签名、把这些
                // 循环局部一路传下去，得不偿失。
                match i {
                    UiIntent::AddSpace => match spaces.add_space() {
                        Some(idx) => {
                            renderer.space_count = spaces.count();
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                ToastMsg::new(
                                    format!(
                                        "已新增桌面（共 {} 个）· Super+{} 可切过去",
                                        spaces.count(),
                                        idx + 1
                                    ),
                                    format!(
                                        "Added a desktop ({} total) · Super+{} switches to it",
                                        spaces.count(),
                                        idx + 1
                                    ),
                                ),
                                notifications::Kind::Info,
                            );
                        }
                        None => push_toast(
                            &mut toast,
                            &mut renderer.notifs,
                                toast_msg_1("桌面数量已达上限（{}）", "Desktop limit reached ({})", spaces::Spaces::<draw::Win>::MAX),
                            notifications::Kind::Info,
                        ),
                    },
                    UiIntent::RemoveSpace => {
                        let cur = spaces.current();
                        if spaces.remove_space(cur, &mut desktop.wins, &mut desktop.active) {
                            renderer.space_count = spaces.count();
                            // 窗口数组与当前空间都变了：拖拽与吸附预览必须一起清掉
                            drag = None;
                            snap_zone = layout::Snap::None;
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg_1("已移除桌面（还剩 {} 个）", "Removed a desktop ({} left)", spaces.count()),
                                notifications::Kind::Info,
                            );
                        } else {
                            // **删不掉必须说明原因**（只说"失败"等于让用户猜）：
                            // 最常见的是"当前桌面里还有窗口"——那会连带丢掉终端会话
                            let why = if spaces.count() <= 1 {
                                toast_msg("至少要保留一个桌面", "At least one desktop must remain")
                            } else {
                                toast_msg(
                                    "当前桌面里还有窗口：先用 Super+Shift+数字 把它们移走，或关掉它们",
                                    "This desktop still has windows: move them with Super+Shift+number first, or close them",
                                )
                            };
                            push_toast(&mut toast, &mut renderer.notifs, why, notifications::Kind::Info);
                        }
                    }
                    other => apply_ui_intent(other, &ai_tx, &mut toast, &mut renderer, &mut audio),
                }
            }
            consumed
        } {
        } else if desktop_icon_click(&renderer, &mut desktop, mouse.0, mouse.1, click_pending) {
            // 桌面图标：打开对应应用。
            // **必须用一次性信号 `click_pending`，不能用 `press`（= mouse_down || click_pending）**：
            // `mouse_down` 是电平信号，按住期间每帧为真 —— 曾经导致"点一下文件图标弹出一大堆窗口"。
        } else if mouse_down || click_pending {
            if let Some((idx, edge, orig)) = resize {
                if mouse_down {
                    if let Some(w) = desktop.wins.get_mut(idx) {
                        apply_resize(w, edge, orig, mouse.0, mouse.1);
                    } else {
                        resize = None; // 窗口被关掉了
                    }
                } else {
                    resize = None; // 松手结束
                }
            } else if let Some(hit) = renderer
                .settings_hits
                .iter()
                .find(|(rr, _)| rr.contains(mouse.0, mouse.1))
                .map(|(_, id)| *id)
            {
                // 设置中心（P3）：绘制期登记命中，这里改状态并**立即落盘**。
                // 只对"当前活动窗口是设置"生效 —— 否则背后那个设置窗口会被隔空点中。
                let is_settings = desktop
                    .wins
                    .get(desktop.active)
                    .map(|x| matches!(&x.kind, draw::WinKind::Settings))
                    .unwrap_or(false);
                if is_settings {
                    settings::apply_hit(&mut renderer.settings, &mut renderer.settings_page, hit);
                    // §5.2：搜索框的编辑态在 Renderer 上（`apply_hit` 拿不到它），
                    // 所以在调用点处理 —— 与下面「动作类命中」同一做法。
                    if let settings::SettingsHit::AuditQuery = hit {
                        renderer.audit_editing = true;
                    }
                    // 动作类命中：打开内建应用（「应用」页的行）
                    if let settings::SettingsHit::OpenApp(i) = hit {
                        if let Some(msg) = open_app(&mut desktop, i) {
                            push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                        }
                    }
                    // 「网络」页：重启网络服务 —— 唯一一个真正**改系统**的动作（P4.4）。
                    // 走 aetherd（ServiceControl）转 init，合成器不直接碰系统。
                    if let settings::SettingsHit::RestartService(unit) = hit {
                        control_service(ai_tx.clone(), unit, aether_ipc::ServiceAction::Restart);
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("正在重启服务 {}…", "Restarting service {}…", unit), notifications::Kind::Info);
                    }
                    // 电源入口（批次 J-2）：与 AI 触发的工具调用走**同一条**路径
                    // （system_power 工具 → 确认卡片 → aetherd → PID 1），不新造通道。
                    // 也不在这里直接调 reboot —— 合成器只画界面，永远不碰系统。
                    match hit {
                        settings::SettingsHit::Power(action) => {
                            let a = action.as_str();
                            spawn_power_tool(action, &ai_tx);
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("请在确认卡片里输入 {} 以确认", "Type {} in the confirmation card to confirm", a), notifications::Kind::Info);
                        }
                        settings::SettingsHit::Logout => {
                            apply_ui_intent(UiIntent::Logout, &ai_tx, &mut toast, &mut renderer, &mut audio);
                        }
                        _ => {}
                    }
                    // 导出审计日志（§5.2）：**动作类**，成功给出确切路径，
                    // 失败给出原因（不静默、不谎报"已导出"）
                    // §5.6「快照与回滚」页：进入时取一次清单
                    if let settings::SettingsHit::Page(i) = hit {
                        let is_snap = settings::PAGES
                            .get(i)
                            .map(|p| p.title == "快照与回滚")
                            .unwrap_or(false);
                        if is_snap && !renderer.snapshots.is_loaded() {
                            // 预览循环（--preview）没有 IPC 发送端：这里只落地状态，不做「再取一次」
                        }
                    }
                    // §5.6：创建快照
                    if let settings::SettingsHit::CreateSnapshot = hit {
                        if renderer.snapshots.is_pending() {
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg("上一步还没完成，请稍候", "The previous step is still running, please wait"), notifications::Kind::Info);
                        } else {
                            renderer.snapshots.mark_pending();
                            create_snapshot(ai_tx.clone());
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg("正在创建配置快照…", "Creating a config snapshot…"), notifications::Kind::Info);
                        }
                    }
                    // §3.7「可移动设备」页：进入时扫一次
                    if let settings::SettingsHit::Page(i) = hit {
                        let is_rem = settings::PAGES
                            .get(i)
                            .map(|p| p.title == "可移动设备")
                            .unwrap_or(false);
                        if is_rem && !renderer.removables_loaded {
                            // §3.7：外设与可移动设备在同一页，一起读
                            fetch_peripherals(ai_tx.clone());
                            fetch_removables(ai_tx.clone());
                        }
                    }
                    // §3.7：重新扫描
                    if let settings::SettingsHit::RefreshRemovables = hit {
                        fetch_peripherals(ai_tx.clone());
                        fetch_removables(ai_tx.clone());
                    }
                    // §3.7：点一下设备 —— **挂载还是卸载由这一刻的真实状态决定**，
                    // 而不是由绘制那一帧的状态决定（否则中间状态一变就做错事）。
                    if let settings::SettingsHit::ToggleRemovable(i) = hit {
                        if renderer.removable_pending.is_some() {
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg("上一次操作还没结束，请稍候", "The previous action is still running, please wait"),
                                notifications::Kind::Info,
                            );
                        } else if let Some(d) = renderer.removables.get(i).cloned() {
                            let mount = d.mounted_at.is_none();
                            renderer.removable_pending = Some(d.dev.clone());
                            renderer.removable_message = None;
                            toggle_removable(ai_tx.clone(), d.dev.clone(), mount);
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                if mount {
                                    toast_msg_1("正在只读挂载 {}…", "Mounting read-only {}…", &d.dev)
                                } else {
                                    toast_msg_1("正在卸载 {}…", "Unmounting {}…", &d.dev)
                                },
                                notifications::Kind::Info,
                            );
                        }
                    }
                    // §3.8「显示输出」页：进入时读一次（只读报告）
                    if let settings::SettingsHit::Page(i) = hit {
                        let is_disp = settings::PAGES
                            .get(i)
                            .map(|p| p.title == "显示输出")
                            .unwrap_or(false);
                        if is_disp && !renderer.displays_loaded {
                            fetch_displays(ai_tx.clone());
                        }
                    }
                    // §3.8：重新读取显示输出
                    if let settings::SettingsHit::RefreshDisplays = hit {
                        fetch_displays(ai_tx.clone());
                    }
                    // §3.1「系统更新」页：进入时取一次列表
                    if let settings::SettingsHit::Page(i) = hit {
                        let is_upd = settings::PAGES
                            .get(i)
                            .map(|p| p.title == "系统更新")
                            .unwrap_or(false);
                        if is_upd && !renderer.update.is_loaded() {
                            fetch_updates(ai_tx.clone());
                        }
                    }
                    // §3.1：刷新更新列表（不需要先置 pending：清单回来时 on_list 会清掉临时错误）
                    if let settings::SettingsHit::RefreshUpdates = hit {
                        fetch_updates(ai_tx.clone());
                    }
                    // §3.1：暂存某个更新包。**界面不判断能不能装** ——
                    // 版本与哈希的结论由 aetherd 给，界面只负责把原因显示出来。
                    if let settings::SettingsHit::StageUpdate(i) = hit {
                        if !renderer.update.can_click() {
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg("上一次暂存还没结束，请稍候", "The previous staging is still running, please wait"),
                                notifications::Kind::Info,
                            );
                        } else if let Some(name) = renderer.update.packages().get(i).cloned() {
                            renderer.update.mark_pending(&name);
                            stage_update(ai_tx.clone(), name.clone());
                            push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg_1("正在校验并暂存 {}…", "Verifying and staging {}…", name),
                                notifications::Kind::Info,
                            );
                        }
                    }
                    // §5.6：**两步确认的回滚**。
                    // 第一次点击只选中（提示"再点一次"），第二次才真发请求 ——
                    // 回滚改的是系统配置，误点一次就换掉了用户当前能用的设置。
                    if let settings::SettingsHit::RestoreSnapshot(i) = hit {
                        if let Some(id) = renderer.snapshots.items().get(i).map(|s| s.id.clone()) {
                            match renderer.snapshots.take_confirmed(&id) {
                                Some(confirmed) => {
                                    renderer.snapshots.mark_pending();
                                    restore_snapshot(ai_tx.clone(), confirmed.clone());
                                    push_toast(
                                        &mut toast,
                                        &mut renderer.notifs,
                                            toast_msg_1("正在回滚到 {}…", "Rolling back to {}…", confirmed),
                                        notifications::Kind::Info,
                                    );
                                }
                                None => {
                                    // 还没选中（或选的是别的）：这一步只改状态，不发请求
                                    let _ = renderer.snapshots.begin_confirm(&id);
                                }
                            }
                        }
                    }
                    // §5.6：取消待确认的回滚（两步确认必须能退出来）
                    if let settings::SettingsHit::CancelRestore = hit {
                        renderer.snapshots.cancel_confirm();
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg("已取消回滚", "Rollback cancelled"), notifications::Kind::Info);
                    }
                    // §3.3「时钟与时区」页：进入该页时取一次状态（否则显示不出系统时区与上次偏移）。
                    if let settings::SettingsHit::Page(i) = hit {
                        let is_clock = settings::PAGES
                            .get(i)
                            .map(|p| p.title == "时钟与时区")
                            .unwrap_or(false);
                        if is_clock && !renderer.time.loaded {
                            // 预览循环（--preview）没有 IPC 发送端：这里只落地状态，
                        // 不做「再取一次」（那要连 aetherd，而预览模式刻意不依赖 IPC/显示环境）
                        }
                    }
                    // §3.3：立即对时
                    if let settings::SettingsHit::SyncTimeNow = hit {
                        if renderer.time.pending {
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg("上一步还没完成，请稍候", "The previous step is still running, please wait"), notifications::Kind::Info);
                        } else {
                            renderer.time.mark_pending();
                            sync_time_now(ai_tx.clone());
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg("正在对时…", "Syncing time…"), notifications::Kind::Info);
                        }
                    }
                    // §3.3：切换待写入的时区名（纯本地状态，不发 IPC）
                    if let settings::SettingsHit::CycleTimezoneName = hit {
                        renderer.time.cycle();
                    }
                    // §3.3：把时区写进系统
                    if let settings::SettingsHit::SetTimezoneNow = hit {
                        if renderer.time.pending {
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg("上一步还没完成，请稍候", "The previous step is still running, please wait"), notifications::Kind::Info);
                        } else {
                            let name = renderer.time.picked_name().to_string();
                            renderer.time.mark_pending();
                            write_timezone(ai_tx.clone(), name.clone());
                            push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("正在写入系统时区：{}", "Writing system time zone: {}", name), notifications::Kind::Info);
                        }
                    }
                    // §5.12：提示音主题切换 —— **切换即试听**（否则"可换"是看不见的：
                    // 用户点一下听不到差别，只会以为功能坏了）
                    if let settings::SettingsHit::CycleSoundTheme = hit {
                        settings::apply_hit(
                            &mut renderer.settings,
                            &mut renderer.settings_page,
                            settings::SettingsHit::CycleSoundTheme,
                        );
                        if renderer.settings.notify_sound {
                            audio.play_with(
                                audio::SoundTheme::of(renderer.settings.sound_theme),
                                audio::Cue::Done,
                            );
                        }
                        if let Err(e) = renderer.settings.save(settings::SETTINGS_PATH) {
                            eprintln!("[aether-compositor] 保存提示音主题失败：{e}");
                        }
                    }
                    // §5.3「工具权限」页：进入该页时**自动取一次**（否则会是空白，
                    // 用户不知道是没工具还是没加载）。
                    if let settings::SettingsHit::Page(i) = hit {
                        let is_tools = settings::PAGES
                            .get(i)
                            .map(|p| p.title == "工具权限")
                            .unwrap_or(false);
                        if is_tools && !renderer.policy.is_loaded() {
                            fetch_policy(ai_tx.clone());
                        }
                    }
                    // §5.11「应用商店」：进页面时扫一次目录（**不每帧扫**），
                    // 否则界面是空白，用户不知道是"没有应用"还是"还没扫"。
                    if let settings::SettingsHit::Page(i) = hit {
                        let is_store = settings::PAGES
                            .get(i)
                            .map(|p| p.title == "应用商店")
                            .unwrap_or(false);
                        if is_store && !renderer.store.is_scanned() {
                            // 两份都由商店自己读（版本语义只在一处确定 —— 见 P-1 那个 bug）
                            renderer.store.refresh(
                                std::path::Path::new(store::CATALOG_DIR),
                                &apps_root(),
                            );
                        }
                    }
                    // §5.11：重新扫描
                    if let settings::SettingsHit::RefreshStore = hit {
                        renderer.store.refresh(
                            std::path::Path::new(store::CATALOG_DIR),
                            &apps_root(),
                        );
                        push_toast(
                            &mut toast,
                            &mut renderer.notifs,
                                toast_msg("已重新扫描商店目录", "Rescanned the store folder"),
                            notifications::Kind::Info,
                        );
                    }
                    // §5.11：卸载。同样走 `send_tool_call`（L2 → 确认卡片）——
                    // 服务端是**移入回收站**而不是直接删，所以文案要如实这么说。
                    if let settings::SettingsHit::RemoveApp(i) = hit {
                        match renderer.store.installed_id(i) {
                            Some(id) => {
                                let tx = ai_tx.clone();
                                let arg = id.clone();
                                push_toast(
                                    &mut toast,
                                    &mut renderer.notifs,
                                        toast_msg_1("正在请求卸载：{}", "Requesting uninstall: {}", id),
                                    notifications::Kind::Info,
                                );
                                std::thread::spawn(move || {
                                    if let Err(e) = send_tool_call(
                                        "app_remove".to_string(),
                                        serde_json::json!({ "id": arg }),
                                        None,
                                        ConfirmOrigin::Store,
                                        &tx,
                                    ) {
                                        let _ = tx.send(AiEvent::ToolDone {
                                            ok: false,
                                            output: format!("请求卸载失败：{e}"),
                                            origin: ConfirmOrigin::Store,
                                        });
                                    }
                                });
                            }
                            None => push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg("这一项已经不在已装列表里（可能刚被卸载）", "This item is no longer installed (maybe just removed)"),
                                notifications::Kind::Info,
                            ),
                        }
                    }
                    // §5.11：安装。走**与 AI 工具调用、整盘安装完全同一条通路**
                    // （`send_tool_call` → 闸门判 L2 → 确认卡片 → 用户允许后执行）——
                    // 商店不是"绕过权限的快捷方式"，它只是同一个动作的另一个入口。
                    if let settings::SettingsHit::InstallApp(i) = hit {
                        match renderer.store.path_of(i) {
                            Some(path) if renderer.store.action_of(i).map(|a| a.actionable()).unwrap_or(false) => {
                                let tx = ai_tx.clone();
                                let arg = path.display().to_string();
                                push_toast(
                                    &mut toast,
                                    &mut renderer.notifs,
                                    // 注意：助手按值收参数，所以这里要 `clone()` —— 直接把 `items()[i].name`
                        // 传进去会**移动**它（只能在 Linux 上编译时才暴露，musl 检查抓到过）
                        toast_msg_1(
                            "正在请求安装：{}",
                            "Requesting install: {}",
                            renderer.store.items()[i].name.clone(),
                        ),
                                    notifications::Kind::Info,
                                );
                                // `send_tool_call` 会阻塞等回执（最长 300 秒），必须在独立线程里
                                std::thread::spawn(move || {
                                    if let Err(e) = send_tool_call(
                                        "app_install".to_string(),
                                        serde_json::json!({ "path": arg }),
                                        None,
                                        ConfirmOrigin::Store,
                                        &tx,
                                    ) {
                                        let _ = tx.send(AiEvent::ToolDone {
                                            ok: false,
                                            output: format!("请求安装失败：{e}"),
                                            origin: ConfirmOrigin::Store,
                                        });
                                    }
                                });
                            }
                            _ => push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg("这一项已安装（或不可安装）", "This item is already installed (or cannot be installed)"),
                                notifications::Kind::Info,
                            ),
                        }
                    }
                    // §5.3：点击一行 → 循环到下一个策略。**先标记 pending 再发请求**：
                    // 等回执期间不再接受点击（连点会发多个请求，回执顺序无法保证，
                    // 最后落盘的可能不是用户最后想要的那个）。
                    if let settings::SettingsHit::CycleToolPolicy(i) = hit {
                        match renderer.policy.cycle(i) {
                            Some((tool, next)) => {
                                renderer.policy.mark_pending(&tool);
                                save_tool_policy(ai_tx.clone(), tool.clone(), next);
                                push_toast(
                                    &mut toast,
                                    &mut renderer.notifs,
                                        toast_msg_1("正在保存 {}…", "Saving {}…", tool),
                                    notifications::Kind::Info,
                                );
                            }
                            None => push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg("正在保存上一项，请稍候", "Saving the previous item, please wait"),
                                notifications::Kind::Info,
                            ),
                        }
                    }
                    // §5.3：重新读取
                    if let settings::SettingsHit::RefreshPolicy = hit {
                        fetch_policy(ai_tx.clone());
                        push_toast(
                            &mut toast,
                            &mut renderer.notifs,
                                toast_msg("正在读取权限策略…", "Reading the permission policy…"),
                            notifications::Kind::Info,
                        );
                    }
                    // 「修改 DNS」（§2.4）：点击进入编辑态，输入框里带上**当前值**
                    // （用户从现状开始改，而不是从空白重打）。
                    if let settings::SettingsHit::EditDns = hit {
                        let current = crate::net::read().dns.join(", ");
                        renderer.dns.begin(&current);
                    }
                    // §2.3：点口令框 → 焦点移过去并进入编辑态。
                    // 口令框**从空白开始**（旧口令的"当前值"在 shadow 里是哈希，拿不到明文）。
                    if let settings::SettingsHit::FocusPassField(i) = hit {
                        renderer.pass_form.focus = i.min(2);
                        renderer.pass_form.focused().begin();
                        renderer.pass_form.feedback = None;
                    }
                    // §2.3：重新读取账户列表
                    if let settings::SettingsHit::RefreshUsers = hit {
                        renderer.users_loaded = false;
                        fetch_users(ai_tx.clone());
                    }
                    // §2.3：提交改口令（旧口令先在本机校验，见 submit_password）
                    if let settings::SettingsHit::SubmitPassword = hit {
                        submit_password(&mut renderer, &mut toast, ai_tx.clone());
                    }
                    if let settings::SettingsHit::ExportAudit = hit {
                        match export_audit() {
                            Ok(path) => push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg_1("审计日志已导出：{}", "Audit log exported: {}", path),
                                notifications::Kind::Ok,
                            ),
                            Err(e) => push_toast(
                                &mut toast,
                                &mut renderer.notifs,
                                    toast_msg_1("导出失败：{}", "Export failed: {}", e),
                                notifications::Kind::Err,
                            ),
                        }
                    }

                    // 清空通知历史（§3.2）：**动作类**，绘制期不改状态，这里执行。
                    // 顺手也把未读清零（历史的未读与"有没有内容"不该出现不一致）。
                    if let settings::SettingsHit::ClearNotifications = hit {
                        let had = renderer.notifs.len();
                        renderer.notifs.clear();
                        // **刻意不走 push_toast**：那条消息会被记回刚清空的历史，
                        // 于是用户点完"清空"看到的仍是 1 条 —— 看起来像没生效。
                        // 统一走 push_toast 是为了"不漏记"，而这里正是**唯一**一个
                        // 不该被记的动作；例外写在这里，并有测试钉住历史确实是空的。
                        toast = Some((toast_msg_1("已清空 {} 条通知", "Cleared {} notifications", had), Instant::now()));
                    }

                    // 主题类改动要立刻作用到全局 MODE（并重建背景缓存），不能等重启
                    // §3.5：语言切换同样**立即生效**（左栏与各项标题下一帧就变）。
                    // §3.4：字号切换同样立即生效（下一帧所有文字就变了）
                    if matches!(hit, settings::SettingsHit::CycleUiScale) {
                        draw::theme::ui::set_scale_pct(renderer.settings.ui_scale_pct);
                    }
                    if matches!(hit, settings::SettingsHit::Locale(_)) {
                        crate::i18n::set_lang(crate::i18n::Lang::from_locale(
                            renderer.settings.locale,
                        ));
                    }
                    if matches!(
                        hit,
                        settings::SettingsHit::ToggleDark | settings::SettingsHit::ToggleContrast
                    ) {
                        // 先取值，避免同时可变借用 renderer
                        let dark = renderer.settings.dark_mode;
                        let hc = renderer.settings.high_contrast;
                        apply_theme(&mut renderer, dark, hc);
                    }
                    // 壁纸（批次 L-1）：换壁纸要重建背景缓存 —— 与换主题同一条机制。
                    // 不重建的话画面会保持旧壁纸直到某次偶然的缓存失效，看起来像"没生效"。
                    if matches!(hit, settings::SettingsHit::Wallpaper(_)) {
                        renderer.bg_dirty = true;
                    }
                    // 恢复默认：可能同时改了主题与壁纸，两件事都要做
                    if matches!(hit, settings::SettingsHit::ResetDefaults) {
                        let dark = renderer.settings.dark_mode;
                        let hc = renderer.settings.high_contrast;
                        apply_theme(&mut renderer, dark, hc);
                        renderer.bg_dirty = true;
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg("外观已恢复默认", "Appearance restored to defaults"), notifications::Kind::Ok);
                    }
                    if let Err(e) = renderer.settings.save(settings::SETTINGS_PATH) {
                        // 写不进去要让用户知道（只读盘/没挂 /var），不能静默失败
                        push_toast(&mut toast, &mut renderer.notifs, toast_msg_1("设置未能写入磁盘：{}", "Settings could not be written to disk: {}", e), notifications::Kind::Err);
                    }
                }
            } else if focused_terminal(&desktop) && mouse_down {
                // 按住并移动 = 继续拖选
                terminal_drag_select(&mut desktop, tr.as_ref(), mouse.0, mouse.1, false);
            } else if let Some((i, lx, ly)) = drag {
                // 窗口数组可能在拖拽中被修改（如 AI 关窗），索引失效即取消拖拽
                match desktop.wins.get_mut(i) {
                    Some(win) => {
                        win.rect.x += (mouse.0 - lx) as i32;
                        win.rect.y = (win.rect.y + (mouse.1 - ly) as i32).max(layout::TOP_BAR);
                        snap_zone = if win.floating {
                            layout::detect(mouse.0, mouse.1, w, h)
                        } else {
                            layout::Snap::None
                        };
                        drag = Some((i, mouse.0, mouse.1));
                    }
                    None => {
                        drag = None;
                        snap_zone = layout::Snap::None;
                    }
                }
            } else if let Some((idx, min, zoom, close)) = renderer
                .window_lights
                .iter()
                .rev() // 后画的窗口在上层，命中优先
                .find(|(_, mn, z, c)| {
                    mn.contains(mouse.0, mouse.1) || z.contains(mouse.0, mouse.1) || c.contains(mouse.0, mouse.1)
                })
                .copied()
            {
                // Windows 三键：最小化 / 最大化 / 关闭（**三键全部接线**）
                let msg = if min.contains(mouse.0, mouse.1) {
                    minimize_window(&mut desktop, idx)
                } else if close.contains(mouse.0, mouse.1) {
                    // §5.1：红灯关窗也要**可撤销**（fbdev 路径与预览路径同一套）——
                    // 三条关窗路径若各写一份，就会出现「用哪种方式关的决定能不能撤销」。
                    match close_window(&mut desktop, idx) {
                        Some((at, message, win)) => {
                            let title = win.title.clone();
                            let _ = undo.keep_closed(win);
                            undo.push(UndoStep::ClosedWindow { at, title });
                            Some(message)
                        }
                        None => None,
                    }
                } else if zoom.contains(mouse.0, mouse.1) {
                    toggle_zoom(&mut desktop, idx, layout::work_area(w, h))
                } else {
                    None
                };
                drag = None;
                snap_zone = layout::Snap::None;
                if let Some(m) = msg {
                    push_toast(&mut toast, &mut renderer.notifs, m, notifications::Kind::Info);
                }
            } else if focused_terminal(&desktop)
                && active_term_contains(&desktop, tr.as_ref(), mouse.0, mouse.1)
            {
                // 终端内容区：按下 = 开始拖选（不再走"点标题拖窗口"的分支）
                open_menu = None;
                drag = None;
                terminal_drag_select(&mut desktop, tr.as_ref(), mouse.0, mouse.1, true);
            } else if let Some((idx, edge)) = topmost_edge(&desktop, mouse.0, mouse.1) {
                // 窗口边缘 → 缩放（标题栏仍归拖动，所以上边不参与）
                open_menu = None;
                resize = begin_resize(&mut desktop, idx, edge);
                drag = None;
                snap_zone = layout::Snap::None;
            } else if mouse.1 < layout::TOP_BAR as f32 {
                let hit_menu = renderer
                    .menubar_menus
                    .iter()
                    .position(|r| r.contains(mouse.0, mouse.1));
                if let Some(mi) = hit_menu {
                    open_menu = if open_menu == Some(mi) { None } else { Some(mi) };
                } else if open_menu.is_some() {
                    open_menu = None;
                }
            } else if let Some((items, _)) = renderer.dropdown.clone() {
                if let Some(i) = items.iter().position(|r| r.contains(mouse.0, mouse.1)) {
                    let (t, r) = run_menu_item(&mut desktop, open_menu.unwrap_or(0), i);
                    if let Some(m) = t {
                        push_toast(&mut toast, &mut renderer.notifs, m, notifications::Kind::Info);
                    }
                    if let Some(r) = r {
                        ai_reply = Some((r, draw::BubbleKind::Ai, Instant::now()));
                    }
                }
                open_menu = None;
            } else if let Some(icon) = renderer.dock_icons.iter().position(|r| r.contains(mouse.0, mouse.1)) {
                // 图标区间：内建 0..DOCK_BUILTINS → 已装应用 → 最后是"安装"向导（仅 Live ISO）
                match dock_action(icon, &renderer.installed_apps, live_installer) {
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
                    DockAction::Installer => installer.open(),
                    DockAction::Nothing => {}
                }
            } else if let Some((_, i)) = renderer
                .sidebar_hits
                .iter()
                .find(|(r, _)| r.contains(mouse.0, mouse.1))
                .copied()
            {
                if let Some((label, path)) = desktop.sidebar.get(i).cloned() {
                    if let Some(msg) = navigate_to(&mut desktop, &path, &label, &label) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                }
            } else if let Some((_, path)) = renderer
                .crumb_hits
                .iter()
                .find(|(r, _)| r.contains(mouse.0, mouse.1))
                .cloned()
            {
                let label = draw::path_leaf(&path);
                if let Some(msg) = navigate_to(&mut desktop, &path, &label, &label) {
                    push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                }
            } else if renderer.file_up.contains(mouse.0, mouse.1) {
                // 返回上级目录
                open_menu = None;
                if let Some(msg) = go_up(&mut desktop) {
                    push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                }
            } else if let Some((_, idx)) = renderer.file_cells.iter().find(|(r, _)| r.contains(mouse.0, mouse.1)).copied() {
                // 文件管理器图标：点一次选中，再点一次打开。
                // 用"二次点击"而不是双击，是为了不引入双击计时器（那会让单击延迟生效）。
                open_menu = None;
                if desktop.selected == Some(idx) {
                    if let Some(msg) = open_entry(&mut desktop, idx) {
                        push_toast(&mut toast, &mut renderer.notifs, msg, notifications::Kind::Info);
                    }
                } else {
                    desktop.selected = Some(idx);
                }
            } else {
                open_menu = None;
                for i in (0..desktop.wins.len()).rev() {
                    let r = desktop.wins[i].rect;
                    let title_hit = draw::Rect { x: r.x, y: r.y, w: r.w, h: metric::TITLE_HIT_H };
                    let body_hit = r.contains(mouse.0, mouse.1);
                    if title_hit.contains(mouse.0, mouse.1) || body_hit {
                        let clicked = desktop.wins.remove(i);
                        desktop.wins.push(clicked);
                        desktop.active = desktop.wins.len() - 1;
                        // 点击（tap）只抬升置顶；按住才进入拖拽
                        if mouse_down {
                            let wi = desktop.wins.len() - 1;
                            desktop.wins[wi].floating = true;
                            desktop.wins[wi].target = None;
                            drag = Some((wi, mouse.0, mouse.1));
                        }
                        break;
                    }
                }
            }
        }

        // ---- 4. 布局目标 → 缓动动画 ----
        let lay = desktop.layout;
        let work = layout::work_area(w, h);
        let tiled = layout::tiled_targets(desktop.wins.len(), lay, work);
        for (i, win) in desktop.wins.iter_mut().enumerate() {
            if lay != layout::Layout::Float && !win.floating {
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
        let ui = draw::UiState {
            snap: snap_preview,
            toast: toast_now.as_ref().map(|(m, a)| (m.text(), *a)),
            ai_input: &ai_input,
            ai_cursor: ai_cursor.pos(),
            ime: &ime,
            // 确认弹窗打开时指令条同时失焦：键盘已经归弹窗
            ai_focused: confirm.is_none(),
            ai_thinking: thinking,
            ai_status,
            ai_reply: reply_now.as_ref().map(|(m, k, a)| (m.as_str(), *k, *a)),
            // 真实音频状态（批次 K-2）：探测过一次，这里只读缓存，不在每帧起子进程
            audio_label: audio.state().label(crate::i18n::is_en()),
            audio_volume: audio.volume_pct(),
            audio_enabled: audio.state().controls_enabled(),
            mouse,
            mouse_down,
            open_menu,
            show_installer: live_installer,
            installer: installer.snapshot(),
            confirm: confirm_ui,
        };

        // ---- 5. 渲染 + 软件光标 + 上屏 ----
        refresh_apps_throttled(&mut renderer, &mut last_apps_scan);
        // 把真实的输入法状态同步给渲染器：控制中心的开关要显示它（真值在 `ime` 里）
        renderer.ime_on = ime.enabled();
        renderer.render_frame(&mut buf, w, h, t, &desktop, &ui, tr.as_ref());
        // 光标形态（P2 遗留补齐）：悬停在窗口边缘时给缩放双头箭头 —— 此前光标恒为箭头，
        // 用户不可能知道边缘能拖。判定复用与缩放**同一份** topmost_edge（6px 抓取带），
        // 提示与实际行为必须一致。
        match topmost_edge(&desktop, mouse.0, mouse.1) {
            Some((_, Edge::Left | Edge::Right)) => {
                draw::draw_resize_cursor(&mut buf, w, h, mouse.0, mouse.1, true)
            }
            Some((_, _)) => draw::draw_resize_cursor(&mut buf, w, h, mouse.0, mouse.1, false),
            None => draw::draw_cursor(&mut buf, w, h, mouse.0, mouse.1),
        }
        fb.blit_dirty(&buf, w, h);
        // 点击锁存只对本帧有效
        click_pending = false;

        frames += 1;
        // 心跳：确认渲染循环在持续推进（串口/文本控制台可见）
        if frames == 1 {
            println!("aether-compositor: 已渲染 1 帧");
            tty_log("首帧已写入 /dev/fb0");
        } else if frames % 100 == 0 {
            println!("aether-compositor: 已渲染 {frames} 帧");
        }
        // 帧率：按目标周期**补足**剩余时间，而不是固定睡 100ms。
        // 固定 100ms 会把帧率压到 5-6fps（渲染本身还要几十 ms），而鼠标指针
        // 只在渲染帧里更新 —— 这正是"鼠标很卡"的直接原因。
        // 渲染超出预算时不睡：跑满 CPU，让交互尽快跟上。
        const TARGET_FRAME_MS: u128 = 33; // ≈30fps
        let spent = frame_start.elapsed().as_millis();
        if spent < TARGET_FRAME_MS {
            std::thread::sleep(Duration::from_millis((TARGET_FRAME_MS - spent) as u64));
        }
    }
}
