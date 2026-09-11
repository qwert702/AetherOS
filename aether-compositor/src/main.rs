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
mod layout;
mod text;

use aether_ipc::{Request, Response};
use draw::{Desktop, Rect, Win};
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

/// 系统内 framebuffer 模式：以屏幕真实分辨率渲染 Essence 桌面。
/// （键盘/鼠标的 evdev 接入在后续迭代；当前为持续渲染的演示桌面）
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
    let start = Instant::now();
    let mut frames: u64 = 0;
    loop {
        let t = start.elapsed().as_secs_f32();
        if frames < 6 {
            eprintln!("[c] 帧{frames} 开始 t={t:.2}");
        }
        let work = layout::work_area(w, h);
        let tiled = layout::tiled_targets(desktop.wins.len(), desktop.layout, work);
        for (i, win) in desktop.wins.iter_mut().enumerate() {
            if let Some(tg) = tiled[i] {
                win.rect = tg;
            }
        }
        renderer.render_frame(&mut buf, w, h, t, &desktop, &ui_stub(), tr.as_ref());
        if frames < 6 {
            eprintln!("[c] 帧{frames} 渲染完成");
        }
        fb.blit(&buf, w, h);
        if frames < 6 {
            eprintln!("[c] 帧{frames} blit 完成");
        }
        frames += 1;
        // 心跳：确认渲染循环在持续推进（串口/文本控制台可见）
        if frames == 1 {
            println!("aether-compositor: 已渲染 1 帧");
            tty_log("首帧已写入 /dev/fb0");
        } else if frames % 100 == 0 {
            println!("aether-compositor: 已渲染 {frames} 帧");
        }
        std::thread::sleep(Duration::from_millis(100)); // 10fps，无输入场景足够
        if frames < 6 {
            eprintln!("[c] 帧{frames} sleep 完成");
        }
    }
}


#[cfg(target_os = "linux")]
fn ui_stub<'a>() -> draw::UiState<'a> {
    draw::UiState {
        snap: None,
        toast: None,
        ai_input: text::strings::AI_BAR_HINT,
        ai_reply: None,
        mouse: (-1.0, -1.0),
        open_menu: None,
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

/// AI 事件（后台线程 → 渲染主循环）。
enum AiEvent {
    Reply(String),
    Action(String, serde_json::Value),
    Error(String),
}

/// 在后台线程里连接 aetherd、发送 Chat、收集响应直到 done。
fn query_aether(text: String, tx: mpsc::Sender<AiEvent>) {
    let run = || -> anyhow::Result<()> {
        let mut stream = TcpStream::connect(("127.0.0.1", aether_ipc::DEFAULT_PORT))
            .map_err(|e| anyhow::anyhow!("aetherd 未运行（127.0.0.1:{}）：{e}", aether_ipc::DEFAULT_PORT))?;
        stream.set_read_timeout(Some(Duration::from_secs(130))).ok();
        stream.write_all(
            aether_ipc::encode(&Request::Chat { session_id: "shell-preview".into(), text }).as_bytes(),
        )?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let mut reply = String::new();
        let mut action: Option<(String, serde_json::Value)> = None;
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
                Ok(Response::Action { name, arguments }) => action = Some((name, arguments)),
                Ok(Response::Error { message, .. }) => {
                    reply = format!("⚠ {message}");
                    break;
                }
                _ => {}
            }
        }
        let _ = tx.send(AiEvent::Reply(reply));
        if let Some((name, args)) = action {
            let _ = tx.send(AiEvent::Action(name, args));
        }
        Ok(())
    };
    if let Err(e) = run() {
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
            open_app(desktop, icon);
            Some(format!("已打开「{app}」"))
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
        let ui = draw::UiState {
            snap: None,
            toast: None,
            ai_input: sample_input,
            ai_reply: Some((sample_reply, 0.5)),
            mouse: if args.contains(&"--menu".to_string()) { (700.0, 120.0) } else { (0.0, 0.0) },
            open_menu: if args.contains(&"--menu".to_string()) { Some(2) } else { None },
        };
        renderer.render_frame(&mut buf, WIDTH, HEIGHT, 1.2, &desktop, &ui, tr.as_ref());
        draw::write_bmp("preview.bmp", &buf, WIDTH, HEIGHT)?;
        println!("preview.bmp written (layout: {})", lay.label());
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
    let mut ai_reply: Option<(String, Instant)> = None;
    let (tx, rx) = mpsc::channel::<AiEvent>();
    let mut open_menu: Option<usize> = None;

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
                        ai_reply = Some(("…思考中".into(), Instant::now()));
                        let tx = tx.clone();
                        std::thread::spawn(move || query_aether(text, tx));
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
                AiEvent::Reply(text) => ai_reply = Some((text, Instant::now())),
                AiEvent::Error(text) => ai_reply = Some((format!("⚠ {text}"), Instant::now())),
                AiEvent::Action(name, args) => {
                    if let Some(msg) = apply_action(&mut desktop, &name, &args) {
                        toast = Some((format!("Aether 执行 · {msg}"), Instant::now()));
                    }
                }
            }
        }

        // ---- 鼠标：拖拽优先，其次 UI 命中测试 ----
        if down {
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
                        ai_reply = Some((r, Instant::now()));
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
                    let title_hit = Rect { x: r.x, y: r.y, w: r.w, h: 34 };
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
            open_menu = open_menu.filter(|_| {
                // 松手时若鼠标不在任何 UI 上不主动关闭菜单（菜单靠再次点击关闭）
                true
            });
            if let Some((i, _, _)) = drag.take() {
                if snap_zone != layout::Snap::None {
                    let work = layout::work_area(WIDTH, HEIGHT);
                    desktop.wins[i].target = Some(layout::rect(snap_zone, work));
                    toast = Some((snap_zone.label().to_string(), Instant::now()));
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
            Some((msg, t0)) if t0.elapsed().as_secs_f32() < 6.0 => {
                Some((msg, t0.elapsed().as_secs_f32()))
            }
            _ => None,
        };

        let ui = UiState {
            snap: snap_preview,
            toast: toast_now.as_ref().map(|(m, a)| (m.as_str(), *a)),
            ai_input: &ai_input,
            ai_reply: reply_now.as_ref().map(|(m, a)| (m.as_str(), *a)),
            mouse: (mx, my),
            open_menu,
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
