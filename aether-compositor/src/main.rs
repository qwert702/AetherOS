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
#[cfg(target_os = "linux")]
mod input;
mod layout;
mod text;

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

const WIDTH: usize = 1280;
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
    let mut snap_zone = layout::Snap::None;
    let mut toast: Option<(String, Instant)> = None;
    let mut ai_input = String::new();
    let mut ai_reply: Option<(String, draw::BubbleKind, Instant)> = None;
    let mut open_menu: Option<usize> = None;
    // L2+ 权限确认：待确认请求 + 用户已输入的回显文本
    let mut confirm: Option<(ConfirmRequest, String)> = None;
    let mut ai_status = draw::AiStatus::Local;
    // 是否在等 AI 回复（指令条显示呼吸动效）
    let mut thinking = false;

    loop {
        let t = start.elapsed().as_secs_f32();

        // ---- 1. evdev 输入（drain 本帧积累的全部事件）----
        while let Ok(ev) = input_rx.try_recv() {
            match ev {
                input::UiEvent::MouseMove { dx, dy } => {
                    // 位移在事件流里累积，不受 10fps 渲染帧率影响；
                    // 倍率只是手感（PS/2 相对计数偏小）
                    mouse.0 = (mouse.0 + dx as f32 * 2.0).clamp(0.0, (w - 1) as f32);
                    mouse.1 = (mouse.1 + dy as f32 * 2.0).clamp(0.0, (h - 1) as f32);
                }
                input::UiEvent::MouseDown => mouse_down = true,
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
                    if let Some((req, echo)) = confirm.as_mut() {
                        // 确认弹窗独占键盘：L3 的回显输入写进弹窗而不是指令条
                        if req.echo_required.is_some() {
                            echo.push(c);
                        }
                    } else if !installer.open {
                        // 向导打开时键盘让位给向导，字符不进指令条
                        ai_input.push(c);
                    }
                }
                input::UiEvent::Backspace => {
                    match confirm.as_mut() {
                        Some((_, echo)) => {
                            echo.pop();
                        }
                        None => {
                            ai_input.pop();
                        }
                    }
                }
                input::UiEvent::Escape => {
                    open_menu = None;
                    if let Some((req, _)) = confirm.take() {
                        // 拒绝路径：关闭弹窗、审计由服务端记录（未授权即未执行）
                        tty_log(&format!("权限确认被拒绝: {}", req.tool));
                        toast = Some((format!("已拒绝「{}」", req.tool), Instant::now()));
                    } else if installer.open && !installer.running {
                        installer.open = false;
                    }
                }
                input::UiEvent::LayoutKey(n) => {
                    if let Some(msg) = set_layout(&mut desktop, layout::Layout::ALL[(n - 1).clamp(0, 3)]) {
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
                AiEvent::Reply(text) => {
                    ai_status = draw::AiStatus::Local;
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
                            toast = Some((format!("已拒绝「{}」", req.tool), Instant::now()));
                        }
                    }
                    None => {}
                }
            }
        } else if installer.open {
            if press && !installer.running {
                if let Some(i) = renderer
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
        } else if mouse_down || click_pending {
            if let Some((i, lx, ly)) = drag {
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
            } else if mouse.1 < layout::TOP_BAR as f32 {
                let hit_menu = renderer
                    .menubar_menus
                    .iter()
                    .position(|r| r.contains(mouse.0, mouse.1));
                if let Some(mi) = hit_menu {
                    open_menu = if open_menu == Some(mi) { None } else { Some(mi) };
                } else if renderer.search_pill.contains(mouse.0, mouse.1) {
                    toast = Some(("搜索：先试试下面的 AI 指令条".into(), Instant::now()));
                    open_menu = None;
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
                // Live ISO 时第 6 个图标是"安装"向导
                if live_installer && icon == APP_TITLES.len() {
                    installer.open();
                } else if let Some(msg) = open_app(&mut desktop, icon) {
                    toast = Some((msg, Instant::now()));
                }
            } else {
                open_menu = None;
                for i in (0..desktop.wins.len()).rev() {
                    let r = desktop.wins[i].rect;
                    let title_hit = draw::Rect { x: r.x, y: r.y, w: r.w, h: metric::TITLE_HIT_H };
                    if title_hit.contains(mouse.0, mouse.1) {
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
        renderer.render_frame(&mut buf, w, h, t, &desktop, &ui, tr.as_ref());
        draw::draw_cursor(&mut buf, w, h, mouse.0, mouse.1);
        fb.blit(&buf, w, h);
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
        std::thread::sleep(Duration::from_millis(100)); // 10fps；QEMU TCG 下渲染本身还要数秒
    }
}

/// 按任意分辨率重建演示桌面。
#[cfg(target_os = "linux")]
fn demo_desktop_sized(w: usize, h: usize) -> Desktop {
    let mut wins = vec![
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::WIN_FILES, floating: false },
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::WIN_TERM, floating: false },
    ];
    let work = layout::work_area(w, h);
    let tg = layout::tiled_targets(wins.len(), Layout::TwoCol, work);
    for (i, win) in wins.iter_mut().enumerate() {
        win.rect = tg[i].unwrap();
    }
    let active = wins.len() - 1;
    Desktop { wins, active, layout: Layout::TwoCol }
}

fn demo_desktop() -> Desktop {
    let mut wins = vec![
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::WIN_FILES, floating: false },
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::WIN_TERM, floating: false },
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: "音乐", floating: false },
    ];
    let work = layout::work_area(WIDTH, HEIGHT);
    let tg = layout::tiled_targets(wins.len(), Layout::TwoCol, work);
    for (i, w) in wins.iter_mut().enumerate() {
        w.rect = tg[i].unwrap();
    }
    let active = wins.len() - 1;
    Desktop { wins, active, layout: Layout::TwoCol }
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

/// AI 事件（后台线程 → 渲染主循环）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum AiEvent {
    Reply(String),
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
    let req = Request::ToolCall { session_id: "shell-preview".into(), tool, arguments, approval };
    stream.write_all(aether_ipc::encode(&req).as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
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
        stream.write_all(
            aether_ipc::encode(&Request::Chat { session_id: "shell-preview".into(), text }).as_bytes(),
        )?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let mut reply = String::new();
        let mut action: Option<(String, serde_json::Value)> = None;
        let mut confirm: Option<ConfirmRequest> = None;
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            match aether_ipc::decode::<Response>(&line) {
                Ok(Response::ChatChunk { delta, done, .. }) => {
                    reply.push_str(&delta);
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
        let _ = tx.send(AiEvent::Reply(reply));
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
                desktop.wins.pop();
                desktop.active = desktop.wins.len() - 1;
                Some("已关闭活动窗口".into())
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

fn open_app(desktop: &mut Desktop, icon: usize) -> Option<String> {
    if icon >= APP_TITLES.len() {
        return Some("未知应用".into());
    }
    if desktop.wins.len() >= 8 {
        return Some("窗口数量已达上限（8）".into());
    }
    let idx = desktop.wins.len();
    let win = Win {
        rect: Rect { x: 200 + (idx % 4) as i32 * 40, y: 90 + (idx % 3) as i32 * 30, w: 500, h: 380 },
        target: None,
        title: APP_TITLES[icon],
        floating: true,
    };
    desktop.wins.push(win);
    desktop.active = desktop.wins.len() - 1;
    None
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
                desktop.wins.pop();
                desktop.active = desktop.wins.len() - 1;
                (Some("窗口已关闭".into()), None)
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
        snap_now(&mut desktop, lay);
        let tr = text::TextRenderer::load();
        let mut renderer = draw::Renderer::new(WIDTH, HEIGHT);
        let sample_input = "把窗口排成两列";
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
        let ui = draw::UiState {
            snap: None,
            toast: None,
            ai_input: sample_input,
            ai_focused: true,
            ai_thinking: args.iter().any(|a| a == "--thinking"),
            ai_status: draw::AiStatus::Local,
            ai_reply: Some((sample_reply, draw::BubbleKind::Ai, 0.5)),
            mouse,
            mouse_down: false,
            open_menu: if args.contains(&"--menu".to_string()) { Some(2) } else { None },
            show_installer: args.iter().any(|a| a == "--installer"),
            installer,
            confirm,
        };
        renderer.render_frame(&mut buf, WIDTH, HEIGHT, 1.2, &desktop, &ui, tr.as_ref());
        draw::write_bmp("preview.bmp", &buf, WIDTH, HEIGHT)?;
        println!("preview.bmp written (layout: {})", lay.label());
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
            *p = (28 << 16) | (29 << 8) | 36;
        }
        let sentence = "中文清晰度：把窗口排成两列，打开终端与文件管理器。";
        let latin = "Aether 0.1.0 — install_disk /dev/vda [OK]";
        let mut y = 20.0f32;
        for px in [11.0f32, 12.0, 13.0, 14.0, 15.0] {
            for (bold, label) in [(false, "常规"), (true, "粗体")] {
                let tag = format!("{px:.0}px {label}");
                tr.draw(&mut buf, fw, fh, 16.0, y, &tag, 11.0, draw::theme::color::TEXT_DIM, 0.9);
                let x = 96.0;
                let x = tr.draw(&mut buf, fw, fh, x, y, sentence, px, draw::theme::color::TEXT, 0.98);
                if bold {
                    tr.draw_bold(&mut buf, fw, fh, x + 12.0, y, latin, px, draw::theme::color::TEXT, 0.98);
                } else {
                    tr.draw(&mut buf, fw, fh, x + 12.0, y, latin, px, draw::theme::color::TEXT_DIM, 0.9);
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
    Ok(())
}

/// minifb 预览主循环（Windows 主机开发迭代形态，M1–M4 联调）。
#[cfg(not(target_os = "linux"))]
fn preview_main() -> anyhow::Result<()> {
    let tr = text::TextRenderer::load();
    let mut renderer = draw::Renderer::new(WIDTH, HEIGHT);
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
    let mut snap_zone = layout::Snap::None;
    let mut toast: Option<(String, Instant)> = None;
    let mut ai_input = String::new();
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

        // ---- 键盘：AI 指令输入 + 布局切换 ----
        // minifb 无字符级输入 API，预览期用按键映射（字母/空格/常用符号）；
        // 数字键 1-4 保留给布局切换，中文指令可用 aetherd chat CLI
        let held = window.get_keys_pressed(minifb::KeyRepeat::Yes);
        for k in &held {
            if let Some(ch) = key_to_char(k) {
                ai_input.push(ch);
            }
        }
        if held.contains(&minifb::Key::Backspace) {
            ai_input.pop();
        }
        let keys = window.get_keys_pressed(minifb::KeyRepeat::No);
        for k in &keys {
            match k {
                minifb::Key::Enter => {
                    if !ai_input.trim().is_empty() {
                        let text = ai_input.trim().to_string();
                        ai_input.clear();
                        // 先把自己的话显示成"用户"气泡，再进入思考态
                        ai_reply = Some((text.clone(), draw::BubbleKind::User, Instant::now()));
                        thinking = true;
                        let tx = tx.clone();
                        std::thread::spawn(move || query_aether(text, 130, tx));
                    }
                }
                minifb::Key::Key1 => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::Float) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                minifb::Key::Key2 => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::TwoCol) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                minifb::Key::Key3 => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::ThreeCol) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                minifb::Key::Key4 => {
                    if let Some(msg) = set_layout(&mut desktop, Layout::Monocle) {
                        toast = Some((msg, Instant::now()));
                    }
                }
                _ => {}
            }
        }

        // ---- AI 事件轮询（非阻塞）----
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AiEvent::Reply(text) => {
                    ai_status = draw::AiStatus::Local;
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
                        toast = Some((format!("已拒绝「{}」", req.tool), Instant::now()));
                    }
                }
                None => {}
            }
        } else if down {
            if let Some((i, lx, ly)) = drag {
                // 拖拽进行中：移动窗口 + 更新吸附区
                let w = &mut desktop.wins[i].rect;
                w.x += (mx - lx) as i32;
                w.y = (w.y + (my - ly) as i32).max(layout::TOP_BAR);
                drag = Some((i, mx, my));
                snap_zone = if desktop.wins[i].floating {
                    layout::detect(mx, my, WIDTH, HEIGHT)
                } else {
                    layout::Snap::None
                };
            } else if my < layout::TOP_BAR as f32 {
                // 1. 菜单栏区域
                let hit_menu = renderer
                    .menubar_menus
                    .iter()
                    .position(|r| r.contains(mx, my));
                if let Some(mi) = hit_menu {
                    open_menu = if open_menu == Some(mi) { None } else { Some(mi) };
                } else if renderer.search_pill.contains(mx, my) {
                    toast = Some(("搜索：M2 应用启动器，先试试下面的 AI 指令条".into(), Instant::now()));
                    open_menu = None;
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
                if let Some(msg) = open_app(&mut desktop, icon) {
                    toast = Some((msg, Instant::now()));
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
        _ => return None,
    };
    Some(c)
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

/// 截图模式下立即就位（无缓动）。
fn snap_now(desktop: &mut Desktop, lay: Layout) {
    let work = layout::work_area(WIDTH, HEIGHT);
    let tg = layout::tiled_targets(desktop.wins.len(), lay, work);
    for (i, w) in desktop.wins.iter_mut().enumerate() {
        if let Some(r) = tg[i] {
            w.rect = r;
        }
    }
}
