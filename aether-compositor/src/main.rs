//! aether-compositor — AetherOS 自研 Wayland 合成器。
//!
//! 当前阶段：主机渲染预览模式（M1–M4 联调形态）。
//! - 极光壁纸 + 磨砂窗口（拖拽/置顶/边缘吸附）+ 四种布局
//! - 底部 AI 指令条可真实输入：Enter 发送到 aetherd serve（127.0.0.1:7311），
//!   收到 ChatChunk 显示回复、收到 Action 执行桌面行为（切换布局等）
//! - `--shot [1-4]` 渲染指定布局的单帧供自检
//!
//! 目标形态（M1 后期，Linux VM）：基于 smithay 的真正 Wayland 合成器。

mod draw;
#[cfg(target_os = "linux")]
mod fbdev;
// 输入事件层：事件类型与键位翻译跨平台（可在开发机上单测），仅采集后端限 Linux
mod input;
// 中文输入法（2.3）：拼音 → 候选 → 上屏，词表自建
mod ime;
#[cfg(target_os = "linux")]
mod pty;
mod layout;
mod text;
// 终端：胶合层跨平台（Linux 走真 PTY，开发机喂真实 ANSI 脚本走同一解析路径）
mod term;
// 文本视图共享状态：光标 / 滚动 / 导航键映射（2.4 统一文本交互）
mod textview;
// VT/ANSI 解析器：纯逻辑跨平台（终端正确性靠它的单测保证）
mod vt;

/// 控件原语（P1）：按钮/开关/滑杆/分段/列表行/滚动条/徽标 + 共享命中表。
///
/// 2026-09-29 新增。设置中心（P3）接入前，大部分控件还没有调用点，
/// 因此模块内暂时允许 dead_code —— **P3 完成后必须移除那条 allow**。
mod widgets;

/// 用户设置的持久化与声明（P3 设置中心）：`/var/lib/aether/settings.json`。
mod settings;
/// 网络状态（P4.2 设置中心「网络」页）：读内核真值（/sys/class/net、/proc/net/*），不新增 IPC。
mod net;
/// 审计日志（P4.3 设置中心「权限与隐私」页）：读 aetherd 的审计日志，只读展示，不新增 IPC。
mod audit;
// Wayland 协议实现（3.1 spike）：wire/object 是纯逻辑，跨平台可测
mod wayland;

use aether_ipc::{Request, Response};
use draw::{Desktop, Rect, Win};
use draw::theme::metric;
#[cfg(target_os = "linux")]
use draw::{InstallerPhase, InstallerUi};
#[cfg(not(target_os = "linux"))]
use draw::UiState;
use layout::Layout;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// 预览/走查分辨率：仅非 Linux 路径使用（系统内走 fbdev 真实分辨率）
#[cfg_attr(target_os = "linux", allow(dead_code))]
const WIDTH: usize = 1280;
#[cfg_attr(target_os = "linux", allow(dead_code))]
const HEIGHT: usize = 760;

/// 把一行诊断写到 VGA 文本控制台（/dev/tty0）。
/// VBox 等环境的串口不可用时，这是唯一能在屏幕上看到启动失败原因的通路。
#[cfg(target_os = "linux")]
fn tty_log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/tty0") {
        let _ = write!(f, "\r\n[AETHER compositor] {msg}\r\n");
    }
}

/// 启动安装线程（按钮/键盘共用）；返回是否真正启动。
#[cfg(target_os = "linux")]
fn begin_install(installer: &mut Installer, tx: &mpsc::Sender<AiEvent>) -> bool {
    if installer.running || installer.disks.is_empty() {
        return false;
    }
    // 已成功完成时禁止再次发起（防误点造成二次整盘擦除）；失败后允许重试
    if matches!(&installer.message, Some((_, false))) {
        return false;
    }
    let disk = installer.disks[installer.selected].0.clone();
    installer.running = true;
    installer.message = None;
    let tx = tx.clone();
    std::thread::spawn(move || run_install(disk, tx));
    true
}

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
    let mut toast: Option<(String, Instant)> = None;
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

    loop {
        let frame_start = Instant::now();
        let t = start.elapsed().as_secs_f32();

        // 终端会话维护（读 PTY / 同步 winsize）—— 每帧一次
        maintain_terminals(&mut desktop, tr.as_ref());

        // ---- 1. evdev 输入（drain 本帧积累的全部事件）----
        while let Ok(ev) = input_rx.try_recv() {
            // 终端拿到焦点时按键归 shell（否则 Ctrl+C 会被当成关窗、方向键会去翻文件列表）。
            // 但模态弹窗优先：确认卡片/安装向导打开时，键盘仍归它们，否则用户无法回答。
            if confirm.is_none() && !installer.open && focused_terminal(&desktop) {
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
                }
                input::UiEvent::MouseDown => mouse_down = true,
                input::UiEvent::Wheel { dy } => {
                    // P1：滚轮此前**完全没有映射**（长列表只能一页页翻）。
                    // 归"当前活动窗口"的内容：文件网格滚动。
                    //
                    // `visible = 1`：滚轮可以一路滚到列表末尾，真实可见区间由状态条显示
                    // （"1–8 / 76"）。这里刻意**不复制一份网格页大小的算法**——那会与
                    // draw.rs 的布局常量各算一套、迟早漂移。
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
                input::UiEvent::MouseUp => {
                    mouse_down = false;
                let was_drag = drag.is_some();
                if let Some((i, _, _)) = drag.take() {
                    if snap_zone != layout::Snap::None {
                        if let Some(win) = desktop.wins.get_mut(i) {
                            win.target = Some(layout::rect(snap_zone, layout::work_area(w, h)));
                            toast = Some((snap_zone.label().to_string(), Instant::now()));
                        }
                    }
                }
                    snap_zone = layout::Snap::None;
                    // 非拖拽的按下-抬起 = 一次点击。QMP/真机都可能把 down+up
                    // 打进同一批事件，在帧间锁存，否则快速点击会被吞掉。
                    if !was_drag {
                        click_pending = true;
                    }
                }
                input::UiEvent::Char(c) => {
                    // Ctrl+Space 在 evdev 侧通常落成 NUL —— 用它当中英切换键。
                    // 换成显式的 UiEvent 变体更好，但那要改 input.rs 的采集层；
                    // 这里特判的代价小得多，且行为可预期。
                    if c == '\0' {
                        ime.toggle();
                        toast = Some((
                            if ime.enabled() { "中文输入：开" } else { "中文输入：关" }.into(),
                            Instant::now(),
                        ));
                    } else if let Some((req, echo)) = confirm.as_mut() {
                        // 确认弹窗独占键盘：L3 的回显输入写进弹窗而不是指令条
                        if req.echo_required.is_some() {
                            echo.push(c);
                        }
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
                    match confirm.as_mut() {
                        Some((_, echo)) => {
                            echo.pop();
                        }
                        None => {
                            // IME 正在拼字时，退格删拼音串而不是正文
                            if !ime.backspace() {
                                ai_cursor.backspace(&mut ai_input);
                            }
                        }
                    }
                }
                input::UiEvent::Escape => {
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
                        toast = Some((format!("已拒绝「{}」", req.tool), Instant::now()));
                    } else if installer.open && !installer.running {
                        installer.open = false;
                    }
                    }
                }
                input::UiEvent::LayoutKey(n) => {
                    if let Some(msg) = set_layout(&mut desktop, layout::Layout::ALL[(n - 1).clamp(0, 3)]) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                input::UiEvent::Ctrl(c, shift) => {
                    // 关窗会改动窗口数组：拖拽状态必须一起清掉（P2-3 同类越界）
                    let closed = !shift && c == 'w';
                    if let Some(msg) = apply_ctrl(&mut desktop, c, shift) {
                        if closed {
                            drag = None;
                            snap_zone = layout::Snap::None;
                        }
                        toast = Some((msg, Instant::now()));
                    }
                }
                input::UiEvent::Nav(n) => {
                    if let Some(msg) =
                        dispatch_nav(&mut desktop, n, &mut ai_input, &mut ai_cursor, &mut ime, &ai_tx)
                    {
                        toast = Some((msg, Instant::now()));
                    }
                }
                input::UiEvent::Enter => {
                    if let Some((req, echo)) = confirm.as_ref() {
                        // 回车 = 允许一次；L3 必须回显匹配才放行
                        if req.echo_ok_input(echo) {
                            let req = confirm.take().map(|(r, _)| r).unwrap();
                            tty_log(&format!("权限确认通过: {}", req.tool));
                            dispatch_approved(req, &ai_tx);
                        } else {
                            toast = Some(("回显不匹配：请原样输入目标名".into(), Instant::now()));
                        }
                    } else if installer.open {
                        if begin_install(&mut installer, &ai_tx) {
                            tty_log("安装向导 → 开始安装（键盘确认）");
                        }
                    } else if !ai_input.trim().is_empty() {
                        let text = ai_input.trim().to_string();
                        ai_input.clear();
                        ai_cursor.home(); // 发送后光标回到开头
                        // 先把自己的话显示成"用户"气泡，再进入思考态
                        ai_reply = Some((text.clone(), draw::BubbleKind::User, Instant::now()));
                        thinking = true;
                        let tx = ai_tx.clone();
                        std::thread::spawn(move || query_aether(text, 15, tx));
                    }
                }
            }
        }

        // ---- 2. AI 事件轮询（非阻塞）----
        while let Ok(ev) = ai_rx.try_recv() {
            match ev {
                AiEvent::Reply(text, channel) => {
                    ai_status = match channel.as_deref() {
                        Some("cloud") => draw::AiStatus::Cloud,
                        // 本地通道、未知通道均按本地展示（离线由 Error 事件接管）
                        _ => draw::AiStatus::Local,
                    };
                    thinking = false;
                    ai_reply = Some((text, draw::BubbleKind::Ai, Instant::now()));
                }
                AiEvent::Error(text) => {
                    // aetherd 不可达/通道故障 → 顶栏转"AI 离线"（灰）
                    ai_status = draw::AiStatus::Offline;
                    thinking = false;
                    ai_reply = Some((format!("⚠ {text}"), draw::BubbleKind::Ai, Instant::now()));
                }
                AiEvent::Action(name, args) => {
                    if let Some(msg) = apply_action(&mut desktop, &name, &args) {
                        toast = Some((format!("Aether 执行 · {msg}"), Instant::now()));
                    }
                }
                AiEvent::Confirm(req) => {
                    tty_log(&format!("权限确认请求: {} L{}", req.tool, req.level));
                    thinking = false;
                    confirm = Some((*req, String::new()));
                }
                AiEvent::ToolDone { ok, output, origin } => {
                    thinking = false;
                    let last = output.lines().last().unwrap_or("").to_string();
                    match origin {
                        ConfirmOrigin::Installer => {
                            installer.running = false;
                            installer.message = Some((if last.is_empty() { "安装完成".into() } else { last.clone() }, !ok));
                        }
                        ConfirmOrigin::Ai => {
                            // 工具调用结果单独一类气泡：让"AI 干了什么"可见
                            let mark = if ok { "✓" } else { "✗" };
                            ai_reply = Some((format!("{mark} {last}"), draw::BubbleKind::Tool, Instant::now()));
                        }
                    }
                    if ok {
                        toast = Some((format!("已执行 · {}", last.chars().take(24).collect::<String>()), Instant::now()));
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
                        toast = Some(("回显不匹配：请原样输入目标名".into(), Instant::now()));
                    }
                    Some(draw::ConfirmButton::Deny) => {
                        if let Some((req, _)) = confirm.take() {
                            tty_log(&format!("权限确认被拒绝: {}", req.tool));
                            cancel_confirm(&req.token);
                            toast = Some((format!("已拒绝「{}」", req.tool), Instant::now()));
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
        } else if control_center_click(&mut renderer, &mut desktop, &mut ime, mouse.0, mouse.1, click_pending) {
            // 控制中心是浮层：展开时它优先消费点击（含"点别处关闭"）。
            // 同样用一次性信号 —— 用电平信号会每帧翻转一次开关，面板疯狂闪。
            // 控制中心是浮层：展开时它优先消费点击（含"点别处关闭"）
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
                    // 动作类命中：打开内建应用（「应用」页的行）
                    if let settings::SettingsHit::OpenApp(i) = hit {
                        if let Some(msg) = open_app(&mut desktop, i) {
                            toast = Some((msg, Instant::now()));
                        }
                    }
                    // 「网络」页：重启网络服务 —— 唯一一个真正**改系统**的动作（P4.4）。
                    // 走 aetherd（ServiceControl）转 init，合成器不直接碰系统。
                    if matches!(hit, settings::SettingsHit::RestartNetwork) {
                        control_service(ai_tx.clone(), "network", aether_ipc::ServiceAction::Restart);
                        toast = Some(("正在重启网络服务…".to_string(), Instant::now()));
                    }
                    // 主题类改动要立刻作用到全局 MODE（并重建背景缓存），不能等重启
                    if matches!(hit, settings::SettingsHit::ToggleDark) {
                        let dark = renderer.settings.dark_mode; // 先取值，避免同时可变借用 renderer
                        apply_theme(&mut renderer, dark);
                    }
                    if let Err(e) = renderer.settings.save(settings::SETTINGS_PATH) {
                        // 写不进去要让用户知道（只读盘/没挂 /var），不能静默失败
                        toast = Some((format!("设置未能写入磁盘：{e}"), Instant::now()));
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
                    close_window(&mut desktop, idx)
                } else if zoom.contains(mouse.0, mouse.1) {
                    toggle_zoom(&mut desktop, idx, layout::work_area(w, h))
                } else {
                    None
                };
                drag = None;
                snap_zone = layout::Snap::None;
                if let Some(m) = msg {
                    toast = Some((m, Instant::now()));
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
                        toast = Some((m, Instant::now()));
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
                                    toast = Some((msg, Instant::now()));
                                }
                            }
                        }
                    }
                    DockAction::Installed(id, name) => {
                        if let Some(msg) = open_installed_app(&mut desktop, id, name) {
                            toast = Some((msg, Instant::now()));
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
                    if let Some(msg) = navigate_to(&mut desktop, &path, &label) {
                        toast = Some((msg, Instant::now()));
                    }
                }
            } else if let Some((_, path)) = renderer
                .crumb_hits
                .iter()
                .find(|(r, _)| r.contains(mouse.0, mouse.1))
                .cloned()
            {
                let label = draw::path_leaf(&path);
                if let Some(msg) = navigate_to(&mut desktop, &path, &label) {
                    toast = Some((msg, Instant::now()));
                }
            } else if renderer.file_up.contains(mouse.0, mouse.1) {
                // 返回上级目录
                open_menu = None;
                if let Some(msg) = go_up(&mut desktop) {
                    toast = Some((msg, Instant::now()));
                }
            } else if let Some((_, idx)) = renderer.file_cells.iter().find(|(r, _)| r.contains(mouse.0, mouse.1)).copied() {
                // 文件管理器图标：点一次选中，再点一次打开。
                // 用"二次点击"而不是双击，是为了不引入双击计时器（那会让单击延迟生效）。
                open_menu = None;
                if desktop.selected == Some(idx) {
                    if let Some(msg) = open_entry(&mut desktop, idx) {
                        toast = Some((msg, Instant::now()));
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
            toast: toast_now.as_ref().map(|(m, a)| (m.as_str(), *a)),
            ai_input: &ai_input,
            ai_cursor: ai_cursor.pos(),
            ime: &ime,
            // 确认弹窗打开时指令条同时失焦：键盘已经归弹窗
            ai_focused: confirm.is_none(),
            ai_thinking: thinking,
            ai_status,
            ai_reply: reply_now.as_ref().map(|(m, k, a)| (m.as_str(), *k, *a)),
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

/// 按任意分辨率重建**默认桌面**（P4.1 起不含任何窗口）。
///
/// 非 Linux 目标下没有调用点（预览/走查走 `demo_desktop()`），但单测要用它，
/// 所以按目标收敛而不是整段 cfg 掉。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn demo_desktop_sized(_w: usize, _h: usize) -> Desktop {
    // P4.1：**默认桌面不开任何窗口**（用户要求）—— 窗口改由桌面图标 / Dock 打开。
    // 走查与预览路径仍用 `demo_desktop()`（3 窗口布局演示），不受影响。
    let wins: Vec<Win> = Vec::new();
    let cwd = draw::default_cwd();
    let (entries, dir_error) = draw::read_dir_entries(&cwd);
    let mut d = Desktop { wins, active: 0, layout: Layout::TwoCol, cwd, entries, selected: None, scroll: 0, clipboard: String::new(), sidebar: sidebar_targets(), dir_error };
    sync_files_title(&mut d);
    d
}

#[cfg_attr(target_os = "linux", allow(dead_code))] // 仅预览/走查路径使用
fn demo_desktop() -> Desktop {
    let mut wins = vec![
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::WIN_FILES.to_string(), kind: draw::WinKind::Files, floating: false, restore: None, preview: None, term: None, wayland: None },
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::WIN_TERM.to_string(), kind: draw::WinKind::Terminal, floating: false, restore: None, preview: None, term: Some(term::Terminal::spawn(80, 24, None)) , wayland: None },
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::WIN_MUSIC.to_string(), kind: draw::WinKind::Music, floating: false, restore: None, preview: None, term: None, wayland: None },
    ];
    let work = layout::work_area(WIDTH, HEIGHT);
    let tg = layout::tiled_targets(wins.len(), Layout::TwoCol, work);
    for (i, w) in wins.iter_mut().enumerate() {
        w.rect = tg[i].unwrap();
    }
    let active = wins.len() - 1;
    let cwd = draw::default_cwd();
    let (entries, dir_error) = draw::read_dir_entries(&cwd);
    let mut d = Desktop { wins, active, layout: Layout::TwoCol, cwd, entries, selected: None, scroll: 0, clipboard: String::new(), sidebar: sidebar_targets(), dir_error };
    // 文件窗口的标题要跟随当前目录，而不是写死的"文件"
    sync_files_title(&mut d);
    d
}

/// L2+ 操作待用户确认（aetherd 签发的一次性令牌）。
/// 部分字段只在 fbdev 路径（Linux）被读取，Windows 预览构建下允许未使用。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct ConfirmRequest {
    tool: String,
    level: u8,
    /// 参数（已转成展示用的键值对）
    arguments: Vec<(String, String)>,
    /// 原始参数：重发时必须逐字不变（令牌绑定参数）
    raw_arguments: serde_json::Value,
    consequence: String,
    /// L3 需原样输入的目标；None = 只需点确认
    echo_required: Option<String>,
    /// 一次性确认令牌
    token: String,
    /// 发起方：决定执行结果回填给谁
    origin: ConfirmOrigin,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum ConfirmOrigin {
    /// AI 对话触发（结果进气泡）
    Ai,
    /// 安装向导触发（结果回填向导）
    Installer,
}

impl ConfirmRequest {
    /// 从 IPC 响应构造（参数值统一转成字符串展示）
    fn from_ipc(tool: String, level: u8, arguments: serde_json::Value, consequence: String, echo_required: Option<String>, token: String, origin: ConfirmOrigin) -> Self {
        let pairs = match &arguments {
            serde_json::Value::Object(map) => map
                .iter()
                .map(|(k, v)| {
                    let s = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), s)
                })
                .collect(),
            other => vec![("value".to_string(), other.to_string())],
        };
        Self { tool, level, arguments: pairs, raw_arguments: arguments, consequence, echo_required, token, origin }
    }

    /// L3 回显是否匹配（L2 恒为 true）。
    fn echo_ok_input(&self, echo: &str) -> bool {
        match &self.echo_required {
            Some(target) => echo.trim() == target,
            None => true,
        }
    }
}

/// 用户点"允许一次"：带一次性令牌重发工具调用。
/// 令牌由服务端签发并绑定 (tool, 参数)，服务端校验通过才真正执行——UI 这一步
/// 只是把"用户看过并同意"转达给闸门，而不是绕过闸门。
fn dispatch_approved(req: ConfirmRequest, tx: &mpsc::Sender<AiEvent>) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let origin = req.origin;
        if let Err(e) = send_tool_call(req.tool.clone(), req.raw_arguments.clone(), Some(req.token.clone()), origin, &tx) {
            let _ = tx.send(AiEvent::ToolDone { ok: false, output: e.to_string(), origin });
        }
    });
}

/// 请求删除文件管理器里选中的项（4.1）。
///
/// **只发起，不执行**：服务端把 `file_delete` 判为 L2 → 回 `NeedsConfirmation`
/// + 令牌 → 主循环弹确认卡片 → 用户点「允许一次」才真的删。
/// 所以这个函数做的事只有"把请求发出去"。
///
/// 不复用 `apply_nav`：那里拿不到 `ai_tx`，而删除必须走 IPC（不能本地直接删 ——
/// 那会绕开闸门与审计）。
fn request_delete(desktop: &Desktop, tx: &mpsc::Sender<AiEvent>) -> Option<String> {
    let i = desktop.selected?;
    let e = desktop.entries.get(i)?;
    let path = draw::join_path(&desktop.cwd, &e.name);
    let name = e.name.clone();
    let args = serde_json::json!({ "path": path });
    let tx = tx.clone();
    std::thread::spawn(move || {
        if let Err(err) = send_tool_call("file_delete".into(), args, None, ConfirmOrigin::Ai, &tx) {
            let _ = tx.send(AiEvent::ToolDone {
                ok: false,
                output: err.to_string(),
                origin: ConfirmOrigin::Ai,
            });
        }
    });
    Some(format!("请求删除「{name}」…"))
}

/// AI 事件（后台线程 → 渲染主循环）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum AiEvent {
    /// 回复文本 + 推理通道（Some("local")/Some("cloud")；None = 未知）。
    /// 顶栏 AI 三态（本地青/云端紫/离线灰）据此更新（ui-design-handover 4.1）。
    Reply(String, Option<String>),
    Action(String, serde_json::Value),
    Error(String),
    /// 工具调用需要用户确认（L2+）
    Confirm(Box<ConfirmRequest>),
    /// 确认后执行的结果（或安装结果）
    ToolDone { ok: bool, output: String, origin: ConfirmOrigin },
}

/// 安装向导状态（仅 Live ISO 会话；run_fbdev 持有）。
#[cfg(target_os = "linux")]
struct Installer {
    open: bool,
    disks: Vec<(String, u64)>,
    selected: usize,
    running: bool,
    /// (结果文案, 是否错误)
    message: Option<(String, bool)>,
}

#[cfg(target_os = "linux")]
impl Installer {
    fn new() -> Self {
        Self { open: false, disks: Vec::new(), selected: 0, running: false, message: None }
    }

    /// 打开向导并扫描磁盘（每次打开重扫，热插拔友好）。
    fn open(&mut self) {
        self.disks = scan_disks();
        if self.selected >= self.disks.len() {
            self.selected = 0;
        }
        self.open = true;
        tty_log(&format!("安装向导打开，发现 {} 块磁盘", self.disks.len()));
    }

    fn snapshot(&self) -> Option<InstallerUi<'_>> {
        if !self.open {
            return None;
        }
        let phase = if self.running {
            InstallerPhase::Running
        } else {
            match &self.message {
                Some((_, false)) => InstallerPhase::Done,
                Some((_, true)) => InstallerPhase::Failed,
                None => InstallerPhase::Idle,
            }
        };
        Some(InstallerUi {
            disks: &self.disks,
            selected: self.selected,
            phase,
            message: self.message.as_ref().map(|(m, _)| m.as_str()),
        })
    }
}

/// 扫描候选安装盘：/sys/block，排除 loop/ram/zram/sr/fd/dm。
#[cfg(target_os = "linux")]
fn scan_disks() -> Vec<(String, u64)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/block") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let skip = name.starts_with("loop")
                || name.starts_with("ram")
                || name.starts_with("zram")
                || name.starts_with("sr")
                || name.starts_with("fd")
                || name.starts_with("dm-");
            if skip {
                continue;
            }
            if let Ok(sz) = std::fs::read_to_string(format!("/sys/block/{name}/size")) {
                if let Ok(sectors) = sz.trim().parse::<u64>() {
                    let mb = sectors * 512 / 1024 / 1024;
                    if mb > 0 {
                        out.push((format!("/dev/{name}"), mb));
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// UI 通道密钥（P1-8）：与 aetherd 同源 —— 环境变量优先，否则读审计目录下的 ui.key。
fn ui_key() -> Option<String> {
    if let Ok(k) = std::env::var("AETHER_UI_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    std::fs::read_to_string("/var/log/aether/ui.key")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 在已建立的连接上注册为 UI 通道。
///
/// 这是 P1-8 的客户端侧配套：服务端只把确认令牌下发给**已注册的 UI 通道**，
/// 也只接受已注册通道的兑现请求。没有这一步，L2+ 操作会被服务端直接拒绝，
/// 令牌也不再有"任何人都能兑现"的漏洞。
fn register_ui(stream: &mut TcpStream, reader: &mut BufReader<TcpStream>) -> anyhow::Result<()> {
    let key = ui_key().ok_or_else(|| {
        anyhow::anyhow!(
            "未找到 UI 通道密钥（设置 AETHER_UI_KEY，或确认 aetherd 已生成 /var/log/aether/ui.key）"
        )
    })?;
    stream.write_all(aether_ipc::encode(&Request::RegisterUi { key }).as_bytes())?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    match aether_ipc::decode::<Response>(&line)? {
        Response::UiRegistered => Ok(()),
        Response::Error { message, .. } => anyhow::bail!("UI 通道注册失败：{message}"),
        other => anyhow::bail!("UI 通道注册收到意外响应：{other:?}"),
    }
}

/// 用户拒绝：把"拒绝"回传服务端（撤销令牌 + 落 `denied_by_user` 审计 + 记入拒绝冷却）。
///
/// 此前"拒绝"只存在于本地 UI 状态里：服务端的令牌会继续存活到过期，审计也无法
/// 区分"用户拒绝过"与"从未回答"（P1-9）。回传失败不阻断界面——日志会说明
/// 令牌将在 5 分钟后自然过期。
fn cancel_confirm(token: &str) {
    let token = token.to_string();
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<()> {
            let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::DEFAULT_PORT).into();
            let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(aether_ipc::encode(&Request::ConfirmCancel { token }).as_bytes())?;
            let mut line = String::new();
            reader.read_line(&mut line)?;
            match aether_ipc::decode::<Response>(&line)? {
                Response::ConfirmCancelled { .. } => Ok(()),
                Response::Error { message, .. } => anyhow::bail!("{message}"),
                other => anyhow::bail!("意外响应：{other:?}"),
            }
        };
        match run() {
            Ok(()) => println!("aether-compositor: 拒绝已回传服务端（令牌已撤销）"),
            Err(e) => println!("aether-compositor: 拒绝回传失败（{e}）；令牌将在 5 分钟后自然过期"),
        }
    });
}

/// 经 aetherd ToolCall 执行整盘安装。安装向导与 AI 走同一条通路：
/// 首次请求不带 approval（闸门必拦，L3 需确认），拿到确认令牌后由主循环
/// 在用户完成回显输入后重发——这样"用户在向导里点了按钮"不再等于"已授权"。
#[cfg(target_os = "linux")]
fn run_install(disk: String, tx: mpsc::Sender<AiEvent>) {
    let result = send_tool_call(
        "install_disk".to_string(),
        serde_json::json!({ "disk": disk.clone(), "confirm": disk }),
        None,
        ConfirmOrigin::Installer,
        &tx,
    );
    if let Err(e) = result {
        println!("aether-compositor: 安装器 → 失败: {e}");
        let _ = tx.send(AiEvent::ToolDone {
            ok: false,
            output: e.to_string(),
            origin: ConfirmOrigin::Installer,
        });
    }
}

/// 经 aetherd 执行一次工具调用。`approval` 为 None 时 L2+ 会被闸门拦下，
/// 此时服务端回 `NeedsConfirmation`——本函数把它转成 `AiEvent::Confirm`
/// 交给主循环弹窗；用户允许后带令牌重发即可真正执行。
fn send_tool_call(
    tool: String,
    arguments: serde_json::Value,
    approval: Option<String>,
    origin: ConfirmOrigin,
    tx: &mpsc::Sender<AiEvent>,
) -> anyhow::Result<()> {
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::DEFAULT_PORT).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(300))).ok();
    let mut reader = BufReader::new(stream.try_clone()?);
    // 先注册为 UI 通道：确认令牌只下发给已注册通道，也只被已注册通道兑现（P1-8）
    register_ui(&mut stream, &mut reader)?;
    let req = Request::ToolCall { session_id: "shell-preview".into(), tool, arguments, approval };
    stream.write_all(aether_ipc::encode(&req).as_bytes())?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    match aether_ipc::decode::<Response>(&line)? {
        Response::ToolResult { ok, output, .. } => {
            let _ = tx.send(AiEvent::ToolDone { ok, output, origin });
        }
        Response::NeedsConfirmation { tool, level, arguments, consequence, echo_required, token } => {
            println!("aether-compositor: 工具 {tool} 需 L{level} 确认，弹确认卡片");
            let _ = tx.send(AiEvent::Confirm(Box::new(ConfirmRequest::from_ipc(
                tool, level, arguments, consequence, echo_required, token, origin,
            ))));
        }
        Response::Error { message, .. } => {
            let _ = tx.send(AiEvent::ToolDone { ok: false, output: message, origin });
        }
        _ => {
            let _ = tx.send(AiEvent::ToolDone { ok: false, output: "aetherd 返回了意外响应".into(), origin });
        }
    }
    Ok(())
}

/// 在后台线程里连接 aetherd、发送 Chat、收集响应直到 done。
/// 每一步都打串口/控制台日志（QEMU TCG 下截图窗口太窄，日志是可靠诊断通路）。
fn query_aether(text: String, timeout_secs: u64, tx: mpsc::Sender<AiEvent>) {
    let run = || -> anyhow::Result<()> {
        println!("aether-compositor: AI 查询「{text}」连接 aetherd…");
        // connect_timeout：aetherd 不可达时快速失败，而不是无限阻塞
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::DEFAULT_PORT).into();
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
            .map_err(|e| anyhow::anyhow!("aetherd 不可达（127.0.0.1:{}）：{e}", aether_ipc::DEFAULT_PORT))?;
        println!("aether-compositor: AI 查询已连接，发送请求（超时 {timeout_secs}s）");
        stream.set_read_timeout(Some(Duration::from_secs(timeout_secs))).ok();
        let mut reader = BufReader::new(stream.try_clone()?);
        // 先注册为 UI 通道：否则 AI 触发的 L2+ 操作拿不到确认令牌（P1-8）
        register_ui(&mut stream, &mut reader)?;
        stream.write_all(
            aether_ipc::encode(&Request::Chat { session_id: "shell-preview".into(), text }).as_bytes(),
        )?;
        let mut line = String::new();
        let mut reply = String::new();
        let mut action: Option<(String, serde_json::Value)> = None;
        let mut confirm: Option<ConfirmRequest> = None;
        // 最后一条 ChatChunk 携带的推理通道（"local"/"cloud"），决定顶栏三态
        let mut channel: Option<String> = None;
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            match aether_ipc::decode::<Response>(&line) {
                Ok(Response::ChatChunk { delta, done, channel: ch, .. }) => {
                    reply.push_str(&delta);
                    if let Some(c) = ch {
                        channel = Some(c);
                    }
                    if done {
                        break;
                    }
                }
                Ok(Response::Action { name, arguments }) => {
                    println!("aether-compositor: 收到 Action {name} {arguments}");
                    action = Some((name, arguments));
                }
                // L2+ 操作：AI 想动手但需要用户点头，先弹确认卡片
                Ok(Response::NeedsConfirmation { tool, level, arguments, consequence, echo_required, token }) => {
                    println!("aether-compositor: AI 请求确认 {tool}（L{level}）");
                    confirm = Some(ConfirmRequest::from_ipc(
                        tool, level, arguments, consequence, echo_required, token, ConfirmOrigin::Ai,
                    ));
                }
                Ok(Response::Error { message, .. }) => {
                    reply = format!("⚠ {message}");
                    break;
                }
                _ => {}
            }
        }
        if let Some(c) = confirm {
            let _ = tx.send(AiEvent::Confirm(Box::new(c)));
            return Ok(());
        }
        println!("aether-compositor: AI 查询完成，回复 {} 字", reply.chars().count());
        let preview: String = reply.chars().take(96).collect();
        println!("aether-compositor: AI 回复: {preview}");
        let _ = tx.send(AiEvent::Reply(reply, channel));
        if let Some((name, args)) = action {
            let _ = tx.send(AiEvent::Action(name, args));
        }
        Ok(())
    };
    if let Err(e) = run() {
        println!("aether-compositor: AI 查询失败: {e}");
        let _ = tx.send(AiEvent::Error(e.to_string()));
    }
}

/// 应用来自 aetherd 的桌面行为（AI 操作桌面的落地端）。
fn apply_action(desktop: &mut Desktop, name: &str, args: &serde_json::Value) -> Option<String> {
    match name {
        "layout_set" => {
            let lay = match args.get("layout").and_then(|v| v.as_str())? {
                "two_col" => Layout::TwoCol,
                "three_col" => Layout::ThreeCol,
                "monocle" => Layout::Monocle,
                "float" => Layout::Float,
                _ => return None,
            };
            desktop.layout = lay;
            for w in &mut desktop.wins {
                w.floating = false;
                w.target = None;
            }
            Some(format!("布局：{}", lay.label()))
        }
        "open_app" => {
            let app = args.get("app").and_then(|v| v.as_str())?;
            let icon = APP_TITLES.iter().position(|t| *t == app)?;
            // open_app 返回 Some 时是拒绝/上限提示，不能谎报"已打开"（P3-3）
            Some(open_app(desktop, icon).unwrap_or_else(|| format!("已打开「{app}」")))
        }
        "close_active" => {
            if desktop.wins.len() > 1 {
                // 统一走 close_window：它有索引边界检查与 active 夹取，
                // 且不会像裸 pop 那样在"拖拽中"留下失效的 drag 索引（0.8 审计）
                let idx = desktop.active;
                close_window(desktop, idx)
            } else {
                Some("至少保留一个窗口".into())
            }
        }
        _ => None,
    }
}

/// 应用某个 Dock 图标 / 菜单项对应的应用窗口。
const APP_TITLES: [&str; 5] = [
    text::strings::WIN_FILES,
    text::strings::WIN_TERM,
    text::strings::WIN_BROWSER,
    text::strings::WIN_MUSIC,
    text::strings::WIN_SETTINGS,
];

/// 命中测试要靠 `DOCK_BUILTINS` 算"已装应用"的图标区间，两边必须一致。
const _: () = assert!(APP_TITLES.len() == draw::DOCK_BUILTINS);

/// 桌面图标点击：命中就打开对应应用（复用 `open_app` 的真实路径）。返回 `true` = 已消费。
///
/// 与 Dock 的差别：Dock 点已开的应用会**聚焦**，桌面图标一律**打开**
/// （桌面图标是"启动器"，不是"任务栏"）。
///
/// 调用点在 Linux 的 fbdev 循环里，非 Linux 目标允许未使用（逻辑由单测覆盖）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_icon_click(
    renderer: &draw::Renderer,
    desktop: &mut Desktop,
    mx: f32,
    my: f32,
    pressed: bool,
) -> bool {
    if !pressed {
        return false;
    }
    let hit = renderer
        .desktop_icons
        .iter()
        .find(|(r, _)| r.contains(mx, my))
        .map(|(_, i)| *i);
    match hit {
        Some(idx) => {
            let _ = open_app(desktop, idx);
            true
        }
        None => false,
    }
}

/// 切换主题：改全局 `MODE`（色板只读、每帧生效）+ 标记背景缓存作废。
///
/// 两个入口共用：设置中心「外观」页与控制中心的深色开关。
/// **不改 `settings.dark_mode`** —— 那是调用方的事（设置中心走 `apply_hit`，控制中心自己翻）。
fn apply_theme(renderer: &mut draw::Renderer, dark: bool) {
    draw::theme::color::set_mode(if dark {
        draw::theme::color::Mode::Dark
    } else {
        draw::theme::color::Mode::Light
    });
    renderer.bg_dirty = true; // 壁纸与投影合成层都依赖主题，必须重建
}

/// 控制中心（P4）的点击处理。返回 `true` = 这次点击已被浮层消费。
///
/// 抽成函数的原因：主循环那段 `if/else` 链已经很长，插进去会让缩进与所有权更难读；
/// 而且这段逻辑能单独测（见 `control_center_tests`）。
///
/// 三条规则：① 点状态簇 → 开关面板；② 面板内的命中项 → 执行真实动作并消费点击；
/// ③ 点面板以外 → 关闭；**点面板内空白只消费、不关闭**（否则误触会让人烦）。
///
/// 调用点在 Linux 的 fbdev 循环里（Windows 预览尚未接控制中心），故非 Linux 目标允许未使用；
/// 逻辑本身由 `control_center_tests` 覆盖（Windows 上也会跑）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn control_center_click(
    renderer: &mut draw::Renderer,
    desktop: &mut Desktop,
    ime: &mut ime::Ime,
    mx: f32,
    my: f32,
    pressed: bool,
) -> bool {
    use draw::ControlHit;
    if !pressed {
        return false;
    }
    if renderer.status_cluster.contains(mx, my) {
        renderer.control_open = !renderer.control_open;
        return true;
    }
    if !renderer.control_open {
        return false;
    }
    if let Some(hit) = renderer
        .control_hits
        .iter()
        .find(|(r, _)| r.contains(mx, my))
        .map(|(_, h)| *h)
    {
        match hit {
            ControlHit::ToggleIme => ime.toggle(),
            ControlHit::ToggleClock24h => {
                renderer.settings.clock_24h = !renderer.settings.clock_24h;
                if let Err(e) = renderer.settings.save(settings::SETTINGS_PATH) {
                    // 与设置中心同一处理：写不进去要让用户知道，不静默失败
                    eprintln!("aether-compositor: 设置未能写入磁盘: {e}");
                }
            }
            ControlHit::ToggleDark => {
                renderer.settings.dark_mode = !renderer.settings.dark_mode;
                let dark = renderer.settings.dark_mode; // 先取值，避免同时可变借用 renderer
                apply_theme(renderer, dark);
                if let Err(e) = renderer.settings.save(settings::SETTINGS_PATH) {
                    eprintln!("aether-compositor: 设置未能写入磁盘: {e}");
                }
            }
            ControlHit::OpenSettings => {
                let _ = open_app(desktop, 4);
            }
            ControlHit::OpenTerminal => {
                let _ = open_app(desktop, 1);
            }
        }
        return true;
    }
    if renderer.control_panel.contains(mx, my) {
        return true; // 面板内空白
    }
    renderer.control_open = false;
    true
}

/// 最小化窗口时把 `rect` 挪到的 x 坐标（屏外）。
///
/// 为什么用"挪出屏外"而不是给 `Win` 加 `minimized` 字段：
/// 挪出屏外后，**渲染、命中测试（标题栏/边缘/内容区）、平铺全都自然跳过它**，
/// 不需要改 9 处 `Win` 构造点，也不会漏掉某个循环。恢复时从 `restore` 取回原矩形。
/// 用一个具名常量而不是散落的魔数。
const MINIMIZED_X: i32 = -10_000;

/// 判断窗口是否处于最小化（屏外）。
fn is_minimized(win: &Win) -> bool {
    win.rect.x <= MINIMIZED_X
}

/// 最小化：移出屏外 + 脱离平铺（`floating`），并记住原矩形以便恢复。
fn minimize_window(desktop: &mut Desktop, idx: usize) -> Option<String> {
    let win = desktop.wins.get_mut(idx)?;
    if is_minimized(win) {
        return None;
    }
    win.restore = Some(win.rect);
    win.rect = Rect { x: MINIMIZED_X, y: win.rect.y, w: win.rect.w, h: win.rect.h };
    win.target = None;
    win.floating = true; // 脱离平铺，否则下一帧布局又把它拉回屏幕
    Some(format!("{} 已最小化（点 Dock 图标恢复）", win.title))
}

/// 若窗口已最小化则恢复（Dock 图标点击走这里）。返回 `true` = 确实恢复了。
///
/// 调用点在 Linux 的 fbdev 循环里；预览路径尚未接最小化（它只有走查用途）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn restore_if_minimized(desktop: &mut Desktop, idx: usize) -> bool {
    let Some(win) = desktop.wins.get_mut(idx) else {
        return false;
    };
    if !is_minimized(win) {
        return false;
    }
    let r = win.restore.take().unwrap_or(Rect { x: 200, y: 90, w: 560, h: 400 });
    win.rect = r;
    win.target = Some(r);
    win.floating = true; // 恢复后保持浮动（与 `open_app` 开出来的窗口一致）
    true
}

fn open_app(desktop: &mut Desktop, icon: usize) -> Option<String> {
    if icon >= APP_TITLES.len() {
        return Some("未知应用".into());
    }
    if desktop.wins.len() >= 8 {
        return Some("窗口数量已达上限（8）".into());
    }
    let idx = desktop.wins.len();
    // 2026-09-29 P3：**设置有了专门内容**（设置中心），不再复用文件管理器的纸面；
    // 浏览器仍暂用文件管理器纸面渲染。
    let kind = match icon {
        1 => draw::WinKind::Terminal,
        3 => draw::WinKind::Music,
        4 => draw::WinKind::Settings,
        _ => draw::WinKind::Files,
    };
    let win = Win {
        rect: Rect { x: 200 + (idx % 4) as i32 * 40, y: 90 + (idx % 3) as i32 * 30, w: 500, h: 380 },
        target: None,
        title: APP_TITLES[icon].to_string(),
        kind,
        floating: true,
        restore: None,
        preview: None,
        // 终端窗口在建立时就开会话（尺寸稍后由每帧维护同步给 PTY）
        term: if kind == draw::WinKind::Terminal {
            Some(term::Terminal::spawn(80, 24, Some(&desktop.cwd)))
        } else {
            None
        },
        wayland: None,
    };
    desktop.wins.push(win);
    desktop.active = desktop.wins.len() - 1;
    None
}

/// 已装应用的根目录。与 `aetherd/src/apps.rs` 同一约定（`AETHER_APPS_DIR` 可覆盖）。
fn apps_root() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("AETHER_APPS_DIR") {
        if !p.is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    #[cfg(target_os = "linux")]
    {
        std::path::PathBuf::from("/var/apps")
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::temp_dir().join("aether-apps")
    }
}

/// 扫描已装应用，供 Dock 显示。返回 `(id, 显示名)`，按 id 排序。
///
/// 为什么合成器自己读、而不问 aetherd：Dock 在**启动第一帧**就要画出来，
/// 不该依赖另一个服务在线（aetherd 挂了也该先看见桌面）。只读 `id`/`name`，
/// 与 `aetherd/src/apps.rs` 的清单是同一份约定，但这里**不做校验** ——
/// 装的时候已经校验过，这里只负责"显示得出来"，读不出的目录跳过就行。
fn load_installed_apps(root: &std::path::Path) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let reserved = [
        text::strings::WIN_FILES,
        text::strings::WIN_TERM,
        text::strings::WIN_BROWSER,
        text::strings::WIN_MUSIC,
        text::strings::WIN_SETTINGS,
        text::strings::INSTALLER,
    ];
    let mut out = Vec::new();
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(txt) = std::fs::read_to_string(dir.join("app.json")) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default();
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or_default();
        // 这几个名字在 Dock 里有保留含义（内建图标 + 安装向导）：撞名会让图标被读成
        // 别的功能，所以不显示。应用本身照常能在终端里用。
        if id.is_empty() || name.is_empty() || reserved.contains(&name) {
            continue;
        }
        out.push((id.to_string(), name.to_string()));
    }
    out.sort();
    out
}

/// 从 Dock 启动一个已装应用：**开一个终端窗口，让它在里面跑**。
///
/// 为什么是终端窗口而不是独立窗口：这系统上的应用现在都是终端程序 ——
/// 没有 Wayland 客户端支持，别的程序也没法往这块 framebuffer 上画。
/// 所以"启动应用"＝"开个终端把它跑起来"，这也正好复用已经审过的 PTY 通路
/// （而不是新开一条"让合成器 fork 任意程序"的路径 —— 那会是个新的逃逸面）。
fn open_installed_app(desktop: &mut Desktop, id: &str, name: &str) -> Option<String> {
    if desktop.wins.len() >= 8 {
        return Some("窗口数量已达上限（8）".into());
    }
    let idx = desktop.wins.len();
    let mut term = term::Terminal::spawn(80, 24, None);
    // 把 `exec <id>` 写进 PTY：shell 起来后立刻执行。
    // 早写不会丢 —— 行规程会缓冲，shell 读到就读（真实终端也是这个行为）。
    term.write(format!("exec {id}\r").as_bytes());
    desktop.wins.push(Win {
        rect: Rect { x: 200 + (idx % 4) as i32 * 40, y: 90 + (idx % 3) as i32 * 30, w: 500, h: 380 },
        target: None,
        title: name.to_string(),
        kind: draw::WinKind::Terminal,
        floating: true,
        restore: None,
        preview: None,
        term: Some(term),
        wayland: None,
    });
    desktop.active = desktop.wins.len() - 1;
    Some(format!("启动应用：{name}"))
}

/// 每 ~3 秒重扫一次已装应用。
///
/// 为什么不每帧扫：`read_dir` + 几个小文件读，每帧做是浪费（60fps 下就是每秒 60 次）。
/// 为什么不只在启动时扫：装应用最常见的路径是"跟 AI 说一句"，那时合成器已经在跑了，
/// 不重扫就得重启才能看见 —— 那不像一个能用的系统。
fn refresh_apps_throttled(renderer: &mut draw::Renderer, last: &mut Instant) {
    if last.elapsed() < std::time::Duration::from_secs(3) {
        return;
    }
    *last = Instant::now();
    let found = load_installed_apps(&apps_root());
    if found != renderer.installed_apps {
        renderer.installed_apps = found;
    }
}

/// Dock 图标被点击后该做什么。
///
/// 抽成纯函数是为了**能测**：这里的区间算术（内建 5 个 → 已装应用 → 安装向导）
/// 一旦错位，症状是"点了 A 打开 B"或"点安装却启动了个应用"，而且只有真的点过才发现。
/// 单测能挡住它，也省得每次都靠 QEMU 里挪光标去撞。
#[derive(Debug, PartialEq, Eq)]
enum DockAction<'a> {
    /// 内建应用（文件/终端/浏览器/音乐/设置），索引进 `APP_TITLES`
    Builtin(usize),
    /// 已装应用：`(id, 显示名)`
    Installed(&'a str, &'a str),
    /// 安装向导（仅 Live ISO 显示）
    Installer,
    /// 空处：安装向导没显示，或者越界
    Nothing,
}

fn dock_action<'a>(
    icon: usize,
    installed: &'a [(String, String)],
    live_installer: bool,
) -> DockAction<'a> {
    if icon < APP_TITLES.len() {
        return DockAction::Builtin(icon);
    }
    let i = icon - APP_TITLES.len();
    if i < installed.len() {
        return DockAction::Installed(&installed[i].0, &installed[i].1);
    }
    if live_installer && i == installed.len() {
        return DockAction::Installer;
    }
    DockAction::Nothing
}

/// 返回上级目录。已在根目录或读不到时返回提示。
fn go_up(desktop: &mut Desktop) -> Option<String> {
    let up = draw::parent_of(&desktop.cwd);
    if up == desktop.cwd {
        return Some("已在最上层目录".into());
    }
    navigate_to(desktop, &up, "上级目录")
}

/// 导航到指定目录（侧栏、面包屑、上级都走这一条路径）。
///
/// 只有一处实现，才不会出现"从侧栏进去没重置选中项、从面包屑进去重置了"这类差异。
fn navigate_to(desktop: &mut Desktop, path: &str, label: &str) -> Option<String> {
    let (entries, err) = draw::read_dir_entries(path);
    if let Some(e) = err {
        // 保留原目录与旧条目：导航失败不该把用户丢到一个读不了的目录里
        desktop.dir_error = Some(format!("无法打开「{label}」：{e}"));
        return Some(format!("无法打开「{label}」"));
    }
    desktop.cwd = path.to_string();
    desktop.entries = entries;
    // 换目录必须重置选中与滚动，否则会指向上一个目录的第 N 项
    desktop.selected = None;
    desktop.scroll = 0;
    desktop.dir_error = None;
    sync_files_title(desktop);
    None
}

/// 侧栏"位置"列表：只列**真实存在**的目录，不存在的不显示（而不是画一个点了没反应的项）。
fn sidebar_targets() -> Vec<(String, String)> {
    let home = draw::default_cwd();
    let mut out = vec![("主目录".to_string(), home.clone())];
    for (label, sub) in [
        ("文档", "Documents"),
        ("图片", "Pictures"),
        ("音乐", "Music"),
        ("下载", "Downloads"),
        ("桌面", "Desktop"),
    ] {
        let p = draw::join_path(&home, sub);
        if std::path::Path::new(&p).is_dir() {
            out.push((label.to_string(), p));
        }
    }
    out
}

/// 让文件窗口的标题跟随当前目录（只显示末段 —— 全路径会挤掉红绿灯）。
fn sync_files_title(desktop: &mut Desktop) {
    let leaf = draw::path_leaf(&desktop.cwd);
    for w in desktop.wins.iter_mut() {
        if w.kind == draw::WinKind::Files {
            w.title = leaf.clone();
        }
    }
}

/// 打开文件管理器里选中的条目：目录则进入，文件则开预览窗口。
///
/// 返回 `Some(msg)` 表示需要提示用户（进不去 / 打不开），`None` 表示静默成功。
fn open_entry(desktop: &mut Desktop, idx: usize) -> Option<String> {
    let ent = desktop.entries.get(idx).cloned()?;
    if ent.is_dir {
        let next = draw::join_path(&desktop.cwd, &ent.name);
        let (entries, err) = draw::read_dir_entries(&next);
        if let Some(e) = err {
            // 进不去就留在原地并说明原因，而不是进一个空目录让人猜
            desktop.dir_error = Some(format!("无法进入 {}：{e}", ent.name));
            return Some(format!("无法进入「{}」", ent.name));
        }
        desktop.cwd = next;
        desktop.entries = entries;
        desktop.selected = None;
        desktop.dir_error = None;
        sync_files_title(desktop);
        None
    } else {
        if desktop.wins.len() >= 8 {
            return Some("窗口数量已达上限（8）".into());
        }
        let path = draw::join_path(&desktop.cwd, &ent.name);
        let data = draw::read_preview(&path);
        let failed = data.error.is_some();
        let win = Win {
            rect: Rect { x: 240, y: 110, w: 620, h: 420 },
            target: None,
            title: ent.name.clone(),
            kind: draw::WinKind::Preview,
            floating: true,
            restore: None,
            preview: Some(data),
            term: None,
            wayland: None,
        };
        desktop.wins.push(win);
        desktop.active = desktop.wins.len() - 1;
        if failed {
            Some(format!("「{}」无法以文本预览", ent.name))
        } else {
            None
        }
    }
}

/// 执行菜单项；返回 (toast, reply-bubble) 反馈。
fn run_menu_item(desktop: &mut Desktop, menu: usize, item: usize) -> (Option<String>, Option<String>) {
    match (menu, item) {
        (0, 0) => {
            let msg = open_app(desktop, 0);
            (msg.or(Some("已新建窗口".into())), None)
        }
        (0, 1) => {
            if desktop.wins.len() > 1 {
                // 同 close_active：统一走 close_window，避免裸 pop 留下失效 drag（0.8 审计）
                let idx = desktop.active;
                (close_window(desktop, idx), None)
            } else {
                (Some("至少保留一个窗口".into()), None)
            }
        }
        (0, 2) => (Some("预览版请用 Esc 退出".into()), None),
        (1, _) => (Some("编辑操作将在应用内生效（M2）".into()), None),
        (2, 0) => (set_layout(desktop, Layout::TwoCol), None),
        (2, 1) => (set_layout(desktop, Layout::ThreeCol), None),
        (2, 2) => (set_layout(desktop, Layout::Monocle), None),
        (2, 3) => (set_layout(desktop, Layout::Float), None),
        (3, 0) => (None, Some("AetherOS 0.1.0 预览版 · Linux 内核 + 全自研用户态 + AI 中枢".into())),
        _ => (None, None),
    }
}

fn main() -> anyhow::Result<()> {
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
            if saved.dark_mode {
                draw::theme::color::Mode::Dark
            } else {
                draw::theme::color::Mode::Light
            }
        }
    };
    draw::theme::color::set_mode(mode);

    // AetherOS 系统内：直接绘制到 Linux framebuffer（无输入，演示桌面）
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/dev/fb0").exists() && !std::env::args().any(|a| a == "--preview") {
        return run_fbdev();
    }

    // 截图自检模式：`--shot [1-4]` 渲染指定布局单帧后退出
    #[cfg(not(target_os = "linux"))]
    let args: Vec<String> = std::env::args().collect();
    #[cfg(not(target_os = "linux"))]
    if let Some(pos) = args.iter().position(|a| a == "--shot") {
        let lay = args
            .get(pos + 1)
            .and_then(|s| s.parse::<usize>().ok())
            .map(|i| Layout::ALL[(i - 1).clamp(0, 3)])
            .unwrap_or(Layout::TwoCol);
        let mut buf = vec![0u32; WIDTH * HEIGHT];
        let mut desktop = demo_desktop();
        desktop.layout = lay;
        // `--settings`：打开设置中心再截图（设置窗口不在默认演示桌面上）。
        // 走查图用 `AETHER_FAKE_UTC` 固定时钟，否则顶栏与设置页预览每分钟都会变。
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
            open_menu: if args.contains(&"--menu".to_string()) { Some(2) } else { None },
            show_installer: args.iter().any(|a| a == "--installer"),
            installer,
            confirm,
        };
        // 单帧输出必须先把壁纸补全（交互路径是分帧生成的）
        renderer.prepare_background(WIDTH, HEIGHT, 1.2);
        renderer.render_frame(&mut buf, WIDTH, HEIGHT, 1.2, &desktop, &ui, tr.as_ref());
        draw::write_bmp("preview.bmp", &buf, WIDTH, HEIGHT)?;
        println!("preview.bmp written (layout: {})", lay.label());
        return Ok(());
    }

    // 帧耗时基准：`--bench [N]` 连续渲染 N 帧并输出平均耗时（性能回归用）。
    // 只测 CPU 侧绘制成本（Windows 无 /dev/fb0，上屏路径另计）。
    #[cfg(not(target_os = "linux"))]
    if let Some(pos) = args.iter().position(|a| a == "--bench") {
        let n: u32 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(120);
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
        let t0 = std::time::Instant::now();
        for i in 0..n {
            renderer.render_frame(
                &mut buf, WIDTH, HEIGHT, 1.2 + i as f32 * 0.016, &desktop, &ui, tr.as_ref(),
            );
            draw::draw_cursor(&mut buf, WIDTH, HEIGHT, 640.0, 400.0);
        }
        let el = t0.elapsed();
        let per = el.as_secs_f64() * 1000.0 / n as f64;
        // 分帧生成：整屏行数 ÷ 每帧行数
        let bg_frames = (HEIGHT as f64 / draw::BG_ROWS_PER_FRAME as f64).ceil();
        println!("bench @ {WIDTH}x{HEIGHT}（两列 + 鼠标）");
        println!("  壁纸整屏生成（一次性）: {bg_ms:.2} ms");
        println!("  分帧后单帧增量: ≈{:.2} ms（{bg_frames:.0} 帧铺完，不再阻塞首帧）", bg_ms / bg_frames);
        println!("  **交互路径首帧**: {inc_first:.2} ms（分帧生效；改动前 = 首帧 + 整屏生成）");
        println!("  首帧（壁纸已就绪）: {first_ms:.2} ms");
        println!("  稳态 {n} 帧: 平均 {per:.2} ms/帧 → 上限 {:.0} fps", 1000.0 / per);
        println!("  理论 30fps 预算 33.3ms/帧，当前占用 {:.0}%", per / 33.3 * 100.0);
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
fn preview_main() -> anyhow::Result<()> {
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
    let mut toast: Option<(String, Instant)> = None;
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

    while window.is_open() && !window.is_key_down(minifb::Key::Escape) {
        let t = start.elapsed().as_secs_f32();
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
                toast = Some((
                    if ime.enabled() { "中文输入：开" } else { "中文输入：关" }.into(),
                    Instant::now(),
                ));
                continue;
            }
            // Ctrl 组合键：Ctrl+W 关窗、Ctrl+Shift+C/V 复制粘贴
            if ctrl {
                if let Some(ch) = key_to_char(k) {
                    let shift = window.is_key_down(minifb::Key::LeftShift)
                        || window.is_key_down(minifb::Key::RightShift);
                    if let Some(msg) = apply_ctrl(&mut desktop, ch.to_ascii_lowercase(), shift) {
                        drag = None;
                        snap_zone = layout::Snap::None;
                        toast = Some((msg, Instant::now()));
                        continue;
                    }
                }
            }
            // 导航键：三级分流（指令条编辑 → 预览滚动 → 文件列表），与真机同一条实现
            if let Some(nav) = minifb_nav(k) {
                if let Some(msg) =
                    dispatch_nav(&mut desktop, nav, &mut ai_input, &mut ai_cursor, &mut ime, &tx)
                {
                    toast = Some((msg, Instant::now()));
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
                        toast = Some((msg, Instant::now()));
                    }
                }
                minifb::Key::Key2 if alt => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::TwoCol) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                minifb::Key::Key3 if alt => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::ThreeCol) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                minifb::Key::Key4 if alt => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::Monocle) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                _ => {}
            }
        }
        }

        // ---- AI 事件轮询（非阻塞）----
        while let Ok(ev) = rx.try_recv() {
            match ev {
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
                    if let Some(msg) = apply_action(&mut desktop, &name, &args) {
                        toast = Some((format!("Aether 执行 · {msg}"), Instant::now()));
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
                    toast = Some(("回显不匹配：请原样输入目标名".into(), Instant::now()));
                }
                Some(draw::ConfirmButton::Deny) => {
                    if let Some((req, _)) = confirm.take() {
                        // 拒绝要回传服务端（撤销令牌 + 留痕），不能只关本地弹窗
                        cancel_confirm(&req.token);
                        toast = Some((format!("已拒绝「{}」", req.tool), Instant::now()));
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
                    close_window(&mut desktop, idx)
                } else if zoom.contains(mx, my) {
                    toggle_zoom(&mut desktop, idx, layout::work_area(WIDTH, HEIGHT))
                } else {
                    None
                };
                drag = None;
                snap_zone = layout::Snap::None;
                if let Some(m) = msg {
                    toast = Some((m, Instant::now()));
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
                        toast = Some((m, Instant::now()));
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
                                    toast = Some((msg, Instant::now()));
                                }
                            }
                        }
                    }
                    DockAction::Installed(id, name) => {
                        if let Some(msg) = open_installed_app(&mut desktop, id, name) {
                            toast = Some((msg, Instant::now()));
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
                    if let Some(msg) = navigate_to(&mut desktop, &path, &label) {
                        toast = Some((msg, Instant::now()));
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
                if let Some(msg) = navigate_to(&mut desktop, &path, &label) {
                    toast = Some((msg, Instant::now()));
                }
            }
            else if renderer.file_up.contains(mx, my) {
                open_menu = None;
                if let Some(msg) = go_up(&mut desktop) {
                    toast = Some((msg, Instant::now()));
                }
            }
            // 3.5 文件管理器图标 → 点一次选中，再点一次打开
            else if let Some((_, idx)) = renderer.file_cells.iter().find(|(r, _)| r.contains(mx, my)).copied() {
                open_menu = None;
                if desktop.selected == Some(idx) {
                    if let Some(msg) = open_entry(&mut desktop, idx) {
                        toast = Some((msg, Instant::now()));
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
                        toast = Some((snap_zone.label().to_string(), Instant::now()));
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
            toast: toast_now.as_ref().map(|(m, a)| (m.as_str(), *a)),
            ai_input: &ai_input,
            ai_cursor: ai_cursor.pos(),
            ime: &ime,
            ai_focused: confirm.is_none(),
            ai_thinking: thinking,
            ai_status,
            ai_reply: reply_now.as_ref().map(|(m, k, a)| (m.as_str(), *k, *a)),
            mouse: (mx, my),
            mouse_down: down,
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
fn key_to_char(k: &minifb::Key) -> Option<char> {
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
fn minifb_nav(k: &minifb::Key) -> Option<input::NavKey> {
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
enum Edge {
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
fn edge_at(win: &Win, mx: f32, my: f32) -> Option<Edge> {
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
fn topmost_edge(desktop: &Desktop, mx: f32, my: f32) -> Option<(usize, Edge)> {
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
fn apply_resize(win: &mut Win, edge: Edge, orig: draw::Rect, mx: f32, my: f32) {
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
fn begin_resize(desktop: &mut Desktop, idx: usize, edge: Edge) -> Option<(usize, Edge, draw::Rect)> {
    let win = desktop.wins.get_mut(idx)?;
    win.floating = true;
    win.target = None;
    desktop.active = idx;
    Some((idx, edge, win.rect))
}

/// 点击是否落在**活动终端**的内容区里（决定按下是"选择文本"还是别的交互）。
fn active_term_contains(desktop: &Desktop, _tr: Option<&text::TextRenderer>, mx: f32, my: f32) -> bool {
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
fn terminal_drag_select(
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
fn focused_terminal(desktop: &Desktop) -> bool {
    desktop.wins.get(desktop.active).map(|w| w.term.is_some()).unwrap_or(false)
}

/// 每帧维护终端会话：读 PTY、并把窗口尺寸同步成 winsize。
///
/// 两条渲染路径共用一份（否则会出现"真机能刷新、预览不刷新"这类只在一条路径成立的差异）。
fn maintain_terminals(desktop: &mut Desktop, tr: Option<&text::TextRenderer>) {
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
fn term_key_from_ui(ev: &input::UiEvent) -> Option<term::TermKey> {
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
fn feed_terminal(desktop: &mut Desktop, key: term::TermKey, ime: &mut ime::Ime) -> bool {
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
fn set_layout(desktop: &mut Desktop, lay: Layout) -> Option<String> {
    if lay == desktop.layout {
        return None;
    }
    desktop.layout = lay;
    for w in &mut desktop.wins {
        w.floating = false;
        w.target = None;
    }
    Some(format!("布局：{}", lay.label()))
}

// ---------------------------------------------------------------------------
// 窗口管理与键盘导航：**两条渲染路径（预览 / fbdev）共用同一份实现**。
// 写在各自的事件循环里迟早会漂移成"预览能关窗、真机不能"这类只在一条路径成立的差异。
// ---------------------------------------------------------------------------

/// 关闭窗口。返回 toast 文案。
///
/// **必须同时修正 `active`**：窗口数组被移除后旧索引会越界。调用方还负责清 `drag`——
/// 拖拽中关掉被拖的窗口正是 P2-3 那类 panic 的来源。
fn close_window(desktop: &mut Desktop, idx: usize) -> Option<String> {
    if idx >= desktop.wins.len() {
        return None;
    }
    let title = desktop.wins.remove(idx).title;
    if desktop.wins.is_empty() {
        desktop.active = 0;
    } else {
        // 关掉的不在活动项之前时，焦点前移一位；总体再夹一次，防越界
        if desktop.active > idx {
            desktop.active -= 1;
        }
        desktop.active = desktop.active.min(desktop.wins.len() - 1);
    }
    Some(format!("已关闭「{title}」"))
}

/// 最大化 / 恢复。用 `restore` 记住原矩形，再点一次回到原位。
fn toggle_zoom(desktop: &mut Desktop, idx: usize, work: draw::Rect) -> Option<String> {
    let win = desktop.wins.get_mut(idx)?;
    match win.restore.take() {
        Some(prev) => {
            win.target = Some(prev);
            Some(format!("已恢复「{}」", win.title))
        }
        None => {
            win.restore = Some(win.rect);
            win.floating = true; // 脱离平铺，否则下一帧会被布局覆盖回去
            win.target = Some(work);
            Some(format!("已最大化「{}」", win.title))
        }
    }
}

/// 活动窗口若是文件管理器，返回它的网格布局（键盘导航要与画出来的一致）。
fn active_files_grid(desktop: &Desktop) -> Option<draw::GridLayout> {
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
fn push_str(input: &mut String, cursor: &mut textview::TextCursor, s: &str) {
    for ch in s.chars() {
        cursor.insert(input, ch);
    }
}

/// 活动窗口若是文本预览，返回 (滚动视图, 总行数, 一屏行数)。
///
/// 三个值一起返回是因为它们必须来自**同一个**窗口状态 —— 分两次借用拿不到。
fn focused_preview(desktop: &mut Desktop) -> Option<(&mut textview::ScrollView, usize, usize)> {
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
fn dispatch_nav(
    desktop: &mut Desktop,
    nav: input::NavKey,
    ai_input: &mut String,
    ai_cursor: &mut textview::TextCursor,
    ime: &mut ime::Ime,
    tx: &mpsc::Sender<AiEvent>,
) -> Option<String> {
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
fn apply_nav(desktop: &mut Desktop, key: input::NavKey) -> Option<String> {
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
        return Some(format!("焦点：{}", desktop.wins.get(desktop.active)?.title));
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
fn control_service(tx: mpsc::Sender<AiEvent>, unit: &'static str, action: aether_ipc::ServiceAction) {
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<Response> {
            let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::DEFAULT_PORT).into();
            let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(
                aether_ipc::encode(&Request::ServiceControl { unit: unit.to_string(), action }).as_bytes(),
            )?;
            let mut line = String::new();
            reader.read_line(&mut line)?;
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

/// 把本地剪贴板同步给 aetherd（2.2 跨进程剪贴板的落地点）。
///
/// 尽力而为：连不上/超时都**静默放弃** —— 剪贴板同步失败不该打扰用户，
/// 更不该在交互线程里等网络。AI 的 `clipboard_read` 工具读的就是这份状态。
fn sync_clipboard_to_daemon(text: String) {
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<()> {
            let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::DEFAULT_PORT).into();
            let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            // 剪贴板 IPC 要求已注册 UI 通道（第四轮审查 P1-1）：不注册会被 403。
            //
            // 顺带消掉了"端口劫持"（P2-3）：注册需要 ui.key，冒充 aetherd 的本机进程
            // 拿不到密钥，所以即便抢到 7311 也无法让这条推送成功 —— 服务端那侧会
            // 因密钥不匹配而拒绝。此前这里是"连上就发"，对端身份完全不校验。
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(aether_ipc::encode(&Request::ClipboardSet { text }).as_bytes())?;
            let mut line = String::new();
            reader.read_line(&mut line)?;
            Ok(())
        };
        let _ = run();
    });
}

/// 复制到剪贴板。目标是终端就复制可见内容，是文件管理器就复制选中项路径。
/// 返回 toast 文案。
fn clipboard_copy(desktop: &mut Desktop) -> Option<String> {
    // 终端优先：它的"内容"就是屏幕
    if let Some(t) = desktop.wins.get(desktop.active).and_then(|w| w.term.as_ref()) {
        // 有拖选复制选区，没有则退化为整屏（两条路都不让 Ctrl+Shift+C 落空）
        let text = t.selection_text();
        let trimmed = text.trim_end().to_string();
        let lines = trimmed.lines().count();
        let explicit = t.sel.is_some();
        let what = if explicit { "选区" } else { "终端内容" };
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
        return Some(format!("已复制{what}（{lines} 行）"));
    }
    // 文件管理器：复制选中项的完整路径（比复制文件名有用得多）
    if let Some(i) = desktop.selected {
        if let Some(e) = desktop.entries.get(i) {
            desktop.clipboard = draw::join_path(&desktop.cwd, &e.name);
            sync_clipboard_to_daemon(desktop.clipboard.clone());
            return Some(format!("已复制路径「{}」", e.name));
        }
    }
    None
}

/// 粘贴。目标是终端就直接送进 PTY（返回 None 表示已消费）；
/// 否则把文本交回调用方插入输入框。
fn clipboard_paste(desktop: &mut Desktop) -> Option<String> {
    if desktop.clipboard.is_empty() {
        return None;
    }
    let text = desktop.clipboard.clone();
    if let Some(t) = desktop.wins.get_mut(desktop.active).and_then(|w| w.term.as_mut()) {
        t.write(&term::paste_bytes(&text));
        return None;
    }
    Some(text)
}

/// Ctrl 组合键动作。
///
/// 规则：`Ctrl+W` 关窗；`Ctrl+Shift+C/V` 复制粘贴。**带 Shift 的才做剪贴板** ——
/// 终端里 `Ctrl+C` 必须留给 shell（中断命令），不区分 shift 就会两件事抢一个键。
fn apply_ctrl(desktop: &mut Desktop, c: char, shift: bool) -> Option<String> {
    match (c, shift) {
        ('c', true) => clipboard_copy(desktop),
        ('v', true) => clipboard_paste(desktop).map(|t| {
            // 粘贴到单行输入框：换行折成空格，避免出现看不见的换行
            let one_line: String = t.replace(['\r', '\n'], " ");
            let n = one_line.chars().count();
            desktop.clipboard = one_line;
            format!("已粘贴 {n} 个字符")
        }),
        ('w', false) => close_window(desktop, desktop.active),
        _ => None,
    }
}

/// 截图模式下立即就位（无缓动）。
#[cfg_attr(target_os = "linux", allow(dead_code))] // 仅预览/走查路径使用
fn snap_now(desktop: &mut Desktop, lay: Layout) {
    let work = layout::work_area(WIDTH, HEIGHT);
    let tg = layout::tiled_targets(desktop.wins.len(), lay, work);
    for (i, w) in desktop.wins.iter_mut().enumerate() {
        if let Some(r) = tg[i] {
            w.rect = r;
        }
    }
}

#[cfg(test)]
mod control_center_tests {
    use super::*;

    /// 控制中心的点击规则（三条）。刻意只测**不落盘**的动作（输入法切换、打开窗口），
    /// 避免单测去写 `/var/lib/aether`。
    #[test]
    fn click_rules() {
        let mut r = draw::Renderer::new(1280, 760);
        let mut d = demo_desktop();
        let mut i = ime::Ime::default();
        r.status_cluster = Rect { x: 1100, y: 2, w: 60, h: 22 };

        // ① 状态簇：第一次点开、再点收起
        assert!(
            control_center_click(&mut r, &mut d, &mut i, 1120.0, 10.0, true),
            "状态簇点击应被消费"
        );
        assert!(r.control_open, "第一次点状态簇应展开");
        assert!(control_center_click(&mut r, &mut d, &mut i, 1120.0, 10.0, true));
        assert!(!r.control_open, "再点应收起");

        // 未展开时其它位置**不**消费点击（要留给窗口内容）
        assert!(!control_center_click(&mut r, &mut d, &mut i, 400.0, 400.0, true));

        // ② 展开后命中开关 → 执行真实动作并消费
        r.control_open = true;
        r.control_panel = Rect { x: 1000, y: 40, w: 264, h: 244 };
        let ime_before = i.enabled();
        r.control_hits = vec![(
            Rect { x: 1200, y: 100, w: 46, h: 20 },
            draw::ControlHit::ToggleIme,
        )];
        assert!(control_center_click(&mut r, &mut d, &mut i, 1210.0, 110.0, true));
        assert_ne!(i.enabled(), ime_before, "点输入法开关应真的切换输入法");

        // 打开设置：窗口数 +1（复用 open_app 那条真实路径）
        let wins_before = d.wins.len();
        r.control_hits = vec![(
            Rect { x: 1004, y: 200, w: 256, h: 34 },
            draw::ControlHit::OpenSettings,
        )];
        assert!(control_center_click(&mut r, &mut d, &mut i, 1100.0, 210.0, true));
        assert_eq!(d.wins.len(), wins_before + 1, "点快捷跳转应真的开窗口");
        assert_eq!(
            d.wins.last().map(|w| w.kind),
            Some(draw::WinKind::Settings),
            "打开设置应得到设置窗口（而不是文件管理器）"
        );

        // ③ 面板内空白：消费但不关闭
        assert!(control_center_click(&mut r, &mut d, &mut i, 1050.0, 240.0, true));
        assert!(r.control_open, "点面板内空白不该关闭面板");
        // 面板以外：关闭
        assert!(control_center_click(&mut r, &mut d, &mut i, 100.0, 400.0, true));
        assert!(!r.control_open, "点面板以外应关闭");

        // 未按下（松开）不消费
        assert!(!control_center_click(&mut r, &mut d, &mut i, 1120.0, 10.0, false));
    }
}

#[cfg(test)]
mod desktop_icon_tests {
    use super::*;

    /// 默认桌面**必须没有窗口**（用户要求：启动后是干净桌面，窗口由图标/Dock 打开）。
    #[test]
    fn boot_desktop_has_no_windows() {
        let d = demo_desktop_sized(1280, 760);
        assert!(d.wins.is_empty(), "默认桌面不该预开窗口");
        assert!(
            d.wins.get(d.active).is_none(),
            "没有窗口时按 active 取窗口必须安全失败（不能越界 panic）"
        );
    }

    /// 点桌面图标 → 打开对应应用（桌面图标是**启动器**，不是任务栏）。
    #[test]
    fn click_opens_app() {
        let mut r = draw::Renderer::new(1280, 760);
        let mut d = demo_desktop_sized(1280, 760);
        r.desktop_icons = vec![(Rect { x: 20, y: 60, w: 96, h: 82 }, 4)];
        assert!(desktop_icon_click(&r, &mut d, 40.0, 90.0, true));
        assert_eq!(d.wins.len(), 1, "点桌面图标应开一个窗口");
        assert_eq!(d.wins[0].kind, draw::WinKind::Settings, "序号 4 应打开设置中心");
        assert!(!desktop_icon_click(&r, &mut d, 40.0, 90.0, false), "没按下的移动不该消费");
        assert!(!desktop_icon_click(&r, &mut d, 800.0, 400.0, true), "空白处不该消费");
    }
    /// 最小化 → 恢复的往返：期间窗口必须在**屏外**（命中测试与平铺才会自然跳过它）。
    #[test]
    fn minimize_then_restore() {
        let mut d = demo_desktop();
        let before = d.wins[1].rect;
        let msg = minimize_window(&mut d, 1).expect("最小化应返回提示文案");
        assert!(msg.contains("最小化"), "提示要说明发生了什么：{msg}");
        assert!(is_minimized(&d.wins[1]), "最小化后必须在屏外");
        assert!(d.wins[1].floating, "最小化必须脱离平铺，否则下一帧布局又把它拉回屏幕");
        assert!(d.wins[1].target.is_none());
        assert!(!d.wins[1].rect.contains(640.0, 400.0), "屏外窗口不该被点中");
        // 重复最小化是空操作
        assert!(minimize_window(&mut d, 1).is_none());
        // 恢复
        assert!(restore_if_minimized(&mut d, 1));
        assert_eq!(d.wins[1].rect, before, "恢复必须回到原来的位置与大小");
        assert!(!is_minimized(&d.wins[1]));
        assert!(!restore_if_minimized(&mut d, 1), "未最小化的窗口恢复是空操作");
    }
}

/// 窗口管理测试：插控制中心的测试时**不要动这个属性** —— 少了它整个模块会被编进正式二进制，
/// 里面的测试辅助函数（`desk`/`files_desk`/`term_desk`）就会成为 dead_code 警告。
#[cfg(test)]
mod window_mgmt_tests {
    use super::*;
    use draw::{FsEntry, WinKind};

    fn mkwin(title: &str, kind: WinKind) -> Win {
        Win {
            rect: Rect { x: 100, y: 100, w: 600, h: 400 },
            target: None,
            title: title.to_string(),
            kind,
            floating: false,
            restore: None,
            preview: None,
            term: None,
            wayland: None,
        }
    }

    fn desk(n: usize) -> Desktop {
        let wins = (0..n).map(|i| mkwin(&format!("w{i}"), WinKind::Terminal)).collect();
        Desktop {
            wins,
            active: n.saturating_sub(1),
            layout: Layout::TwoCol,
            cwd: "/tmp".into(),
            entries: Vec::new(),
            selected: None,
            scroll: 0,
            clipboard: String::new(),
            sidebar: sidebar_targets(),
            dir_error: None,
        }
    }

    /// 只读目录：`n` 个条目，交替目录/文件
    fn files_desk(n: usize) -> Desktop {
        let mut d = desk(1);
        d.wins[0] = mkwin("文件", WinKind::Files);
        d.entries = (0..n)
            .map(|i| FsEntry { name: format!("e{i}"), is_dir: i % 2 == 0, size: 0 })
            .collect();
        d
    }

    #[test]
    fn close_keeps_active_in_range() {
        // 关掉最后一个窗口：active 不能越界（曾是最容易踩的 panic 源）
        let mut d = desk(3);
        close_window(&mut d, 2);
        assert_eq!(d.wins.len(), 2);
        assert!(d.active < d.wins.len(), "active={} len={}", d.active, d.wins.len());
    }

    #[test]
    fn close_shifts_focus_when_removing_before_active() {
        let mut d = desk(3);
        d.active = 2;
        close_window(&mut d, 0); // 关掉活动项之前的一个
        assert_eq!(d.wins.len(), 2);
        // 原来 active 指向 w2，删掉 w0 后 w2 落到索引 1
        assert_eq!(d.active, 1);
        assert_eq!(d.wins[d.active].title, "w2");
    }

    #[test]
    fn close_to_empty_is_safe() {
        let mut d = desk(1);
        assert!(close_window(&mut d, 0).is_some());
        assert!(d.wins.is_empty());
        assert_eq!(d.active, 0);
        assert!(close_window(&mut d, 0).is_none()); // 再关是空操作，不 panic
    }

    /// 0.8 审计：窗口可能在**拖拽进行中**被其它路径关掉
    /// （AI 的 `close_active`、菜单「关闭窗口」）。
    ///
    /// 拖拽分支用 `wins[drag.0]` 直接索引 —— 索引一旦失效就是越界 panic，
    /// 而 compositor 是 essential + `restart: false`，**panic 等于桌面永久死掉**。
    /// 这条测试锁住"关掉最后一个窗口后旧索引确实失效"这个前提
    /// （守卫条件本身在拖拽分支里，无法直接单测）。
    #[test]
    fn closing_last_window_invalidates_stale_drag_index() {
        let mut d = desk(3);
        d.active = 2;
        let stale = 2; // 拖拽开始时记下的索引
        let idx = d.active;
        close_window(&mut d, idx);
        assert_eq!(d.wins.len(), 2);
        assert!(d.active < d.wins.len(), "active 必须仍在界内");
        assert!(
            stale >= d.wins.len(),
            "旧拖拽索引应已失效，这正是要防的越界条件"
        );
    }

    #[test]
    fn zoom_toggles_and_restores_rect() {
        let mut d = desk(1);
        let before = d.wins[0].rect;
        let work = Rect { x: 16, y: 42, w: 1248, h: 602 };
        toggle_zoom(&mut d, 0, work);
        assert_eq!(d.wins[0].target, Some(work));
        assert!(d.wins[0].floating, "最大化必须脱离平铺，否则下一帧被布局覆盖");
        assert_eq!(d.wins[0].restore, Some(before));
        toggle_zoom(&mut d, 0, work);
        assert_eq!(d.wins[0].target, Some(before));
        assert_eq!(d.wins[0].restore, None);
    }

    /// Alt+Tab 切到已最小化的窗口时**必须恢复它**（Windows 语义）。
    ///
    /// 不处理的话焦点会落在不可见的窗口上（用户看到"按了没反应"）；
    /// 直接跳过的话最小化的窗口会变成键盘不可达。两个错法都要防住。
    #[test]
    fn tab_restores_minimized_target() {
        let mut d = desk(3);
        d.active = 0;
        minimize_window(&mut d, 1).expect("最小化应成功");
        assert!(is_minimized(&d.wins[1]));
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 1, "应切到下一个窗口");
        assert!(!is_minimized(&d.wins[1]), "切到已最小化的窗口必须把它恢复");
        // 再切一次（目标是正常窗口）不应产生副作用
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 2);
        assert!(!is_minimized(&d.wins[2]));
    }

    #[test]
    fn tab_cycles_focus_including_wrap() {
        let mut d = desk(3);
        d.active = 0;
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 1);
        apply_nav(&mut d, input::NavKey::Tab);
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 0, "应回绕到第一个");
        // 只有一个窗口时不切换（否则会出现"看不出发生了什么"的静默行为）
        let mut one = desk(1);
        assert!(apply_nav(&mut one, input::NavKey::Tab).is_none());
    }

    #[test]
    fn nav_moves_selection_by_grid() {
        use input::NavKey::*;
        let mut d = files_desk(20);
        // 600x400 的窗口 → 网格 3 列 × 3 行（见 grid_layout 的推导）
        let g = active_files_grid(&d).expect("文件窗口应有网格");
        assert_eq!((g.cols, g.rows), (3, 3));
        assert_eq!(g.page_size(), 9);

        apply_nav(&mut d, Down);
        assert_eq!(d.selected, Some(3), "Down 应下移一行（=cols）");
        apply_nav(&mut d, Up);
        assert_eq!(d.selected, Some(0));
        apply_nav(&mut d, Right);
        assert_eq!(d.selected, Some(1));
        apply_nav(&mut d, Left);
        assert_eq!(d.selected, Some(0));
        apply_nav(&mut d, End);
        assert_eq!(d.selected, Some(19));
        apply_nav(&mut d, Home);
        assert_eq!(d.selected, Some(0));
    }

    #[test]
    fn nav_keeps_selection_visible() {
        use input::NavKey::*;
        let mut d = files_desk(20);
        apply_nav(&mut d, PageDown); // 0 → 9
        assert_eq!(d.selected, Some(9));
        assert_eq!(d.scroll, 1, "第 9 项要落进可视页（9+1-9=1）");
        apply_nav(&mut d, End);
        assert_eq!(d.selected, Some(19));
        assert_eq!(d.scroll, 11);
        apply_nav(&mut d, Home);
        assert_eq!((d.selected, d.scroll), (Some(0), 0));
    }

    #[test]
    fn nav_on_non_files_window_is_noop() {
        use input::NavKey::*;
        let mut d = desk(1); // 终端窗口
        assert!(active_files_grid(&d).is_none());
        apply_nav(&mut d, Down);
        assert_eq!(d.selected, None);
    }

    /// `apply_nav` 绝不能自己处理 `Delete`。
    ///
    /// `Delete` 走的是 `dispatch_nav` → `request_delete` → 发一条**不带令牌**的
    /// `file_delete` ToolCall → 服务端判 L2 → 回确认卡片 → 用户点「允许一次」才真删。
    /// 这条测试守的是"绕过去"的那种写法：如果哪天有人图省事在 `apply_nav` 里直接
    /// 删条目，就会同时绕开闸门与审计 —— 那是权限模型的破口，不是少写一次 IPC。
    ///
    /// （这个测试原名叫 `delete_is_deliberately_not_implemented`，是 4.1 落地之前留下的，
    /// 名字已经和事实相反 —— 它容易被读成"Delete 没做"，从而误导文档。2026-09-28 改名。）
    #[test]
    fn apply_nav_must_not_handle_delete_locally() {
        let mut d = files_desk(3);
        d.selected = Some(1);
        assert!(apply_nav(&mut d, input::NavKey::Delete).is_none());
        assert_eq!(d.entries.len(), 3, "Delete 不得改动任何条目");
        assert_eq!(d.selected, Some(1));
    }

    /// Dock 的图标区间：内建 5 个 → 已装应用 → 安装向导。
    ///
    /// 为什么值得测：错位的症状是"点了 A 打开 B"，而且**只有真的用鼠标点过**才会发现；
    /// 在 QEMU 里靠相对坐标挪光标去撞图标非常不可靠（PS/2 相对事件 + 加速），
    /// 所以这段算术必须能在开发机上确定性验证。
    #[test]
    fn dock_action_maps_icon_ranges() {
        let inst = vec![("hello".to_string(), "Hello".to_string())];

        // 内建区间
        assert_eq!(dock_action(0, &inst, true), DockAction::Builtin(0));
        assert_eq!(dock_action(4, &inst, false), DockAction::Builtin(4));
        assert_eq!(dock_action(4, &inst, true), DockAction::Builtin(4));

        // 已装应用紧接在内建之后
        assert_eq!(dock_action(5, &inst, true), DockAction::Installed("hello", "Hello"));
        assert_eq!(dock_action(5, &inst, false), DockAction::Installed("hello", "Hello"));

        // 安装向导在**最后**；没有已装应用时它前移到第 6 个（不能串位）
        assert_eq!(dock_action(6, &inst, true), DockAction::Installer);
        assert_eq!(dock_action(5, &[], true), DockAction::Installer);

        // 边界：Live ISO 关了就没有安装向导；越界落空
        assert_eq!(dock_action(6, &inst, false), DockAction::Nothing);
        assert_eq!(dock_action(5, &[], false), DockAction::Nothing);
        assert_eq!(dock_action(99, &inst, true), DockAction::Nothing);

        // 多个已装应用时逐个对应，不能都指到同一个
        let two = vec![
            ("a".to_string(), "甲".to_string()),
            ("b".to_string(), "乙".to_string()),
        ];
        assert_eq!(dock_action(5, &two, true), DockAction::Installed("a", "甲"));
        assert_eq!(dock_action(6, &two, true), DockAction::Installed("b", "乙"));
        assert_eq!(dock_action(7, &two, true), DockAction::Installer);
    }

    /// `APP_TITLES` 与 `draw::DOCK_BUILTINS` 必须一致 —— 命中测试靠后者算区间。
    #[test]
    fn app_titles_match_dock_builtins() {
        assert_eq!(APP_TITLES.len(), draw::DOCK_BUILTINS);
    }

    fn term_desk() -> Desktop {
        let mut d = desk(1);
        d.wins[0] = Win {
            rect: Rect { x: 100, y: 100, w: 600, h: 400 },
            target: None,
            title: "终端".into(),
            kind: WinKind::Terminal,
            floating: false,
            restore: None,
            preview: None,
            term: Some(term::Terminal::spawn(40, 8, None)),
        wayland: None,
        };
        d
    }

    #[test]
    fn edge_detection_covers_sides_and_corners() {
        let w = mkwin("w", WinKind::Files); // rect = 100,100 600x400
        assert_eq!(edge_at(&w, 100.0, 300.0), Some(Edge::Left));
        assert_eq!(edge_at(&w, 700.0, 300.0), Some(Edge::Right));
        assert_eq!(edge_at(&w, 400.0, 500.0), Some(Edge::Bottom));
        assert_eq!(edge_at(&w, 700.0, 500.0), Some(Edge::BottomRight));
        assert_eq!(edge_at(&w, 100.0, 500.0), Some(Edge::BottomLeft));
        // 窗口内部与**上边**都不是缩放区：上边是标题栏，归拖动
        assert_eq!(edge_at(&w, 400.0, 300.0), None);
        assert_eq!(edge_at(&w, 400.0, 100.0), None, "上边必须留给拖动");
        assert_eq!(edge_at(&w, 400.0, 600.0), None, "窗口外远端不算边缘");
    }

    #[test]
    fn resize_right_edge_changes_width_only() {
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        apply_resize(&mut w, Edge::Right, orig, 760.0, 300.0);
        assert_eq!(w.rect.x, orig.x);
        assert_eq!(w.rect.w, orig.w + 60);
        assert_eq!(w.rect.h, orig.h, "只有右边时高度不应变");
    }

    #[test]
    fn resize_left_edge_keeps_right_border_fixed() {
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        let right_before = orig.x + orig.w;
        apply_resize(&mut w, Edge::Left, orig, 200.0, 300.0);
        assert_eq!(w.rect.x + w.rect.w, right_before, "拖左边时右边必须固定");
        assert_eq!(w.rect.w, orig.w - 100);
    }

    #[test]
    fn resize_clamps_to_min_size() {
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        apply_resize(&mut w, Edge::Right, orig, 110.0, 300.0);
        assert!(w.rect.w >= 320, "不能小于最小宽度，实际 {}", w.rect.w);
        apply_resize(&mut w, Edge::Bottom, orig, 400.0, 120.0);
        assert!(w.rect.h >= 200, "不能小于最小高度，实际 {}", w.rect.h);
    }

    #[test]
    fn resize_uses_original_rect_so_no_drift() {
        // 以"按下的原始矩形"为基准：同一目标位置重复应用结果一致（不会累积漂移）
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        apply_resize(&mut w, Edge::Right, orig, 700.0, 0.0);
        let once = w.rect.w;
        apply_resize(&mut w, Edge::Right, orig, 700.0, 0.0);
        assert_eq!(w.rect.w, once, "重复应用同一拖拽量不应继续变宽");
    }

    #[test]
    fn begin_resize_detaches_from_tiling() {
        let mut d = desk(1);
        let r = begin_resize(&mut d, 0, Edge::BottomRight).expect("应能开始缩放");
        assert_eq!(r.0, 0);
        assert!(d.wins[0].floating, "缩放必须脱离平铺，否则下一帧被布局改回");
        assert_eq!(d.wins[0].target, None);
        assert_eq!(d.active, 0);
    }

    #[test]
    fn crumbs_split_windows_and_unix_paths() {
        // 用原始字符串写 Windows 路径，避免转义把反斜杠吃掉
        let win = r"C:\Users\cbn";
        let w = draw::crumbs(win);
        assert_eq!(
            w.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["C:", "Users", "cbn"]
        );
        assert_eq!(w[2].1, win, "末段应等于原路径");
        assert_eq!(w[1].1, r"C:\Users");
        // 正斜杠形式的 Windows 路径（有些 API 会返回这种）同样要能拆
        let fwd = draw::crumbs("C:/Users/cbn");
        assert_eq!(fwd.len(), 3, "正斜杠路径也应拆出三段");
        assert_eq!(fwd[2].1, "C:/Users/cbn");
        let u = draw::crumbs("/home/u");
        assert_eq!(
            u.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["home", "u"]
        );
        assert_eq!(u[1].1, "/home/u");
        assert!(draw::crumbs("").is_empty(), "空路径不应产生面包屑");
    }

    #[test]
    fn navigate_resets_selection_and_scroll() {
        let mut d = files_desk(5);
        d.selected = Some(3);
        d.scroll = 2;
        let tmp = std::env::temp_dir();
        let msg = navigate_to(&mut d, &tmp.to_string_lossy(), "测试目录");
        assert!(msg.is_none(), "系统临时目录应可读：{msg:?}");
        assert_eq!(d.selected, None, "换目录后选中项必须重置");
        assert_eq!(d.scroll, 0, "换目录后滚动位置必须重置");
    }

    #[test]
    fn navigate_failure_keeps_current_dir() {
        let mut d = files_desk(3);
        let before = d.cwd.clone();
        let msg = navigate_to(&mut d, "__aether_absent_dir__", "不存在的目录");
        assert!(msg.is_some(), "失败要有反馈");
        assert_eq!(d.cwd, before, "导航失败不应把用户丢到读不了的目录");
        assert!(d.dir_error.is_some(), "失败原因要能显示在界面上");
    }

    #[test]
    fn clipboard_copies_selected_entry_path() {
        let mut d = files_desk(3);
        d.selected = Some(1);
        let msg = clipboard_copy(&mut d).expect("应有复制反馈");
        assert!(msg.contains("路径"), "{}", msg);
        // 复制的是**完整路径**，不是文件名 —— 粘贴出去才有用
        assert_eq!(d.clipboard, draw::join_path("/tmp", "e1"));
    }

    // 只在开发机跑：Linux 下 `Terminal::spawn` 开的是**真 PTY**（/bin/sh），
    // 屏幕初始是空的；这些测试验的是"演示脚本能被正确解析"，而演示脚本
    // 只在非 Linux 路径存在。Linux 侧的终端行为由实机验证覆盖。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn clipboard_copies_terminal_visible_content() {
        let mut d = term_desk();
        let msg = clipboard_copy(&mut d).expect("终端应可复制");
        assert!(msg.contains("行"), "{}", msg);
        assert!(d.clipboard.contains("aether@localhost"), "应包含演示会话内容");
        assert!(
            !d.clipboard.contains("\u{0}"),
            "复制内容不应带控制字符残留"
        );
    }

    #[test]
    fn clipboard_paste_into_terminal_is_consumed() {
        let mut d = term_desk();
        d.clipboard = "echo hi\n".into();
        // 粘贴进终端：由终端消费（返回 None + 不再回给输入框）
        assert!(clipboard_paste(&mut d).is_none());
    }

    #[test]
    fn clipboard_paste_to_input_returns_text_for_ai_bar() {
        let mut d = files_desk(1);
        d.clipboard = "/tmp/x\n".into();
        let text = clipboard_paste(&mut d).expect("非终端目标应把文本交回调用方");
        assert_eq!(text, "/tmp/x\n");
    }

    #[test]
    fn ctrl_shift_c_copies_and_does_not_close_window() {
        // 这条是 shift 区分的意义所在：Ctrl+Shift+C 不能变成"关窗"
        let mut d = term_desk();
        let before = d.wins.len();
        assert!(apply_ctrl(&mut d, 'c', true).is_some(), "Ctrl+Shift+C 应复制");
        assert_eq!(d.wins.len(), before, "Ctrl+Shift+C 不得关窗");
    }

    #[test]
    fn ctrl_w_closes_active_window() {
        let mut d = desk(2);
        d.active = 1;
        assert!(apply_ctrl(&mut d, 'w', false).is_some());
        assert_eq!(d.wins.len(), 1);
        assert_eq!(d.active, 0);
        assert!(apply_ctrl(&mut d, 'q', false).is_none(), "未绑定的组合键不应有副作用");
    }
}

/// W4：`sync_wayland` 的身份同步测试。
#[cfg(test)]
mod wayland_sync_tests {
    use super::*;
    use wayland::session::{Role, Session, SurfaceState};
    use wayland::shm::BufferInfo;

    const SURFACE: u32 = 6;
    const BUFFER: u32 = 7;

    /// 手工构造一个"已 map"的 surface 状态（协议流程在 session 的测试里验过，
    /// 这里只需要终态）。
    fn mapped_session(title: &str, pixels: Vec<u8>) -> Session {
        let mut s = Session::new();
        s.objects.insert(SURFACE, "wl_surface", 4, 0);
        s.objects.insert(BUFFER, "wl_buffer", 1, 0);
        s.surfaces.insert(
            SURFACE,
            SurfaceState {
                role: Some(Role {
                    title: title.into(),
                    app_id: "test-app".into(),
                    toplevel: Some(9),
                    acked_serial: Some(1),
                }),
                has_buffer: true,
                attached_buffer: Some(BUFFER),
                xdg_surface: Some(8),
                pixels: Some(pixels),
            },
        );
        s.buffers.insert(
            BUFFER,
            BufferInfo { pool_id: 10, offset: 0, width: 1, height: 1, stride: 4, format: 1 },
        );
        s.mapped.push(SURFACE);
        s
    }

    #[test]
    fn new_mapped_surface_becomes_window() {
        let mut desktop = demo_desktop();
        let n0 = desktop.wins.len();
        let s = mapped_session("测试窗口", vec![0xff, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        assert_eq!(desktop.wins.len(), n0 + 1, "应新增一个窗口");
        let w = desktop.wins.last().unwrap();
        assert_eq!(w.kind, draw::WinKind::Wayland);
        assert_eq!(w.title, "测试窗口");
        assert_eq!(w.wayland.as_ref().unwrap().surface_id, SURFACE);
    }

    /// 同步必须**幂等**：同一状态同步两遍不产生重复窗口（每帧都会调）。
    #[test]
    fn sync_is_idempotent() {
        let mut desktop = demo_desktop();
        let s = mapped_session("w", vec![0, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        let n1 = desktop.wins.len();
        sync_wayland(&mut desktop, &s);
        assert_eq!(desktop.wins.len(), n1, "第二次同步不该新建窗口");
    }

    #[test]
    fn unmap_removes_window_but_keeps_others() {
        let mut desktop = demo_desktop();
        let n0 = desktop.wins.len();
        let s = mapped_session("w", vec![0, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        assert_eq!(desktop.wins.len(), n0 + 1);
        // surface 消失（unmap/destroy）
        let mut s2 = mapped_session("w", vec![0, 0, 0, 255]);
        s2.mapped.clear();
        sync_wayland(&mut desktop, &s2);
        assert_eq!(desktop.wins.len(), n0, "Wayland 窗口应移除");
        // 原有窗口（demo 的 Files/Music/Terminal）必须完好
        assert!(
            desktop.wins.iter().all(|w| w.wayland.is_none()),
            "不该误删本地窗口"
        );
    }

    #[test]
    fn existing_window_updates_content_without_recreating() {
        let mut desktop = demo_desktop();
        let s = mapped_session("v1", vec![0xff, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        let w = desktop.wins.last().unwrap();
        let rect_before = w.rect;
        // 客户端改了标题和像素（下一帧 commit）
        let s2 = mapped_session("v2", vec![0, 0, 0xff, 255]);
        sync_wayland(&mut desktop, &s2);
        let w = desktop.wins.last().unwrap();
        assert_eq!(w.title, "v2", "标题应更新");
        assert_eq!(w.rect, rect_before, "窗口位置不应被重建冲掉");
        assert_eq!(
            w.wayland.as_ref().unwrap().pixels,
            vec![0, 0, 0xff, 255],
            "像素应更新"
        );
    }

    /// 红绿灯 / 拖拽等按 `wins[i]` 索引的逻辑依赖窗口顺序 —— 移除中间窗口时
    /// 不能让本地窗口跟着错位（P2 的教训在 Wayland 侧同样适用）。
    #[test]
    fn remove_middle_wayland_window_keeps_local_windows() {
        let mut desktop = demo_desktop();
        let n0 = desktop.wins.len();
        let s1 = mapped_session("wl-1", vec![0, 0, 0, 255]);
        sync_wayland(&mut desktop, &s1);
        // 两个 Wayland surface，只保留第一个 —— 第二个的窗口应被移除
        let mut s2 = mapped_session("wl-1", vec![0, 0, 0, 255]);
        s2.mapped.clear();
        sync_wayland(&mut desktop, &s2);
        assert_eq!(desktop.wins.len(), n0, "全部移除后回到初始数量");
    }
}
