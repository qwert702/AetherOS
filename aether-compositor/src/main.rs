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
mod layout;
mod text;

use aether_ipc::{Request, Response};
use draw::{Desktop, Rect, Win};
use layout::Layout;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const WIDTH: usize = 1280;
const HEIGHT: usize = 760;

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

/// 应用来自 aetherd 的桌面行为。
fn apply_action(desktop: &mut Desktop, name: &str, args: &serde_json::Value) -> Option<String> {
    if name == "layout_set" {
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
        return Some(format!("布局：{}", lay.label()));
    }
    None
}

fn main() -> anyhow::Result<()> {
    // 截图自检模式：`--shot [1-4]` 渲染指定布局单帧后退出
    let args: Vec<String> = std::env::args().collect();
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
        renderer.render_frame(
            &mut buf, WIDTH, HEIGHT, 1.2, &desktop, None, None,
            sample_input, Some((sample_reply, 0.5)), tr.as_ref(),
        );
        draw::write_bmp("preview.bmp", &buf, WIDTH, HEIGHT)?;
        println!("preview.bmp written (layout: {})", lay.label());
        return Ok(());
    }

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

        // ---- 鼠标：拖拽 / 置顶 / 吸附 ----
        if down {
            if drag.is_none() {
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
            if let Some((i, lx, ly)) = drag {
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
        } else {
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
            other => {
                // 未过期但要续期持有
                match other {
                    Some((msg, t0)) => {
                        let r = Some((msg.clone(), t0.elapsed().as_secs_f32()));
                        ai_reply = Some((msg, t0));
                        r
                    }
                    None => None,
                }
            }
        };

        // 空闲降帧：没有任何动画/交互时不必跑满 60fps
        let busy = drag.is_some()
            || desktop.wins.iter().any(|w| w.target.is_some())
            || toast_now.is_some()
            || reply_now.is_some()
            || snap_preview.is_some();
        if !busy {
            std::thread::sleep(Duration::from_millis(10));
        }

        renderer.render_frame(
            &mut buffer,
            WIDTH,
            HEIGHT,
            t,
            &desktop,
            snap_preview,
            toast_now.as_ref().map(|(m, a)| (m.as_str(), *a)),
            &ai_input,
            reply_now.as_ref().map(|(m, a)| (m.as_str(), *a)),
            tr.as_ref(),
        );
        window.update_with_buffer(&buffer, WIDTH, HEIGHT)?;
    }
    Ok(())
}

/// 按键 → 字符（预览期英文输入；数字键 1-4 保留给布局）。
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
