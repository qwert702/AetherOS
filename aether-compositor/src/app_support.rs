//! 桌面辅助函数（从 main.rs 拆出，2026-10-24；行为不变）。

use super::*;

// 预览/走查分辨率：仅非 Linux 路径使用（系统内走 fbdev 真实分辨率）
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) const WIDTH: usize = 1280;
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) const HEIGHT: usize = 760;

/// 把一行诊断写到 VGA 文本控制台（/dev/tty0）。
/// VBox 等环境的串口不可用时，这是唯一能在屏幕上看到启动失败原因的通路。
///
/// 内容**必须过滤控制字符**（2026-10-02 审计 L-13）：`msg` 里可能带 IPC 来源字符串
/// 或错误信息，而这些是外部可控的 —— 原样写进真终端就是转义序列注入
/// （清屏、藏光标、改标题），在一个"救援用"的通路上尤其糟糕。
#[cfg(target_os = "linux")]
pub(crate) fn tty_log(msg: &str) {
    use std::io::Write;
    let clean = sanitize_for_tty(msg);
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/tty0") {
        let _ = write!(f, "\r\n[AETHER compositor] {clean}\r\n");
    }
}

/// 非 Linux（开发机预览/单测）没有 VGA 文本控制台：**空实现**。
///
/// 为什么给空实现而不是让每个调用点写 `#[cfg(target_os = "linux")]`：
/// 调用点关心的是"记一笔"，不是"记到哪个设备"。散落的 cfg 会让主循环更难读，
/// 而且会让"这些代码在 Windows 上根本不参与编译"—— 那意味着拼写错误要等 CI
/// 才发现（本项目已经在别处吃过这个亏）。空实现让它们始终参与编译。
#[cfg(not(target_os = "linux"))]
pub(crate) fn tty_log(_msg: &str) {}

/// 净化一段要写进终端的文本：丢弃 ESC 序列、C0 与 **C1** 控制字符。
///
/// 与 `aether-init::logtee::sanitize_for_display` 同一套规则（那个在另一个 crate，
/// 不能直接复用；**两处都有各自的单测** —— 这里的测试在 `mod tests` 里）。
///
/// 这里是**字符级**（输入已是合法 UTF-8），所以 C1 区（U+0080–U+009F）可以安全丢弃：
/// `\u{9b}` 就是 8 位 CSI、`\u{9d}` 是 8 位 OSC。字节级那份不能这么做 ——
/// 直接丢 0x80–0x9F 的**字节**会把中文（UTF-8 续字节落在 0x80–0xBF）切碎。
///
/// 非 Linux 平台不写 `/dev/tty0`（没有 VGA 文本控制台），故允许 dead_code ——
/// 但**测试照跑**（测试在 `mod tests` 里，Windows 上也会验证它）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn sanitize_for_tty(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI：参数/中间字节后跟终止字节（0x40..=0x7E）
                    for c2 in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c2) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC：吃到 BEL 或 ESC \
                    while let Some(c2) = chars.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\n' | '\r' | '\t' => out.push(c),
            // C0（含 DEL）与 C1 一律丢弃
            c if (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
    }
    out
}

/// 单条 IPC 响应行的长度上限（与 aetherd 的 `MAX_LINE_BYTES` 对齐）。
///
/// 2026-10-02 审计 L-14：合成器里 6 处 `read_line` 都没有上限 ——
/// `BufRead::read_line` 是"对端发多少读多少"，一个失控的对端（或模型返回了
/// 超大 JSON）就能让**以 root 运行的合成器**把内存吃光。
pub(crate) const MAX_IPC_LINE: usize = 1024 * 1024;

/// AI 回复累计长度上限（多轮 ChatChunk 拼起来的总量）。
pub(crate) const MAX_CHAT_BYTES: usize = 4 * 1024 * 1024;

/// 读一行 IPC 响应，**带长度上限**：超限即报错断开，而不是继续吃内存。
///
/// EOF 时返回空串（与 `read_line` 的行为一致，调用方按空串判结束）。
pub(crate) fn read_line_capped<R: BufRead + ?Sized>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<String> {
    let mut line = String::new();
    let n = (&mut *reader).take(max as u64).read_line(&mut line)?;
    if n as usize >= max && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            pick_zh_en_1(
                "IPC 响应超过 {} 字节上限，已断开",
                "IPC response exceeded the {} byte cap, disconnecting",
                max,
            ),
        ));
    }
    Ok(line)
}

/// 启动安装线程（按钮/键盘共用）；返回是否真正启动。
#[cfg(target_os = "linux")]
pub(crate) fn begin_install(installer: &mut Installer, tx: &mpsc::Sender<AiEvent>) -> bool {
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


/// 按任意分辨率重建**默认桌面**（P4.1 起不含任何窗口）。
///
/// 非 Linux 目标下没有调用点（预览/走查走 `demo_desktop()`），但单测要用它，
/// 所以按目标收敛而不是整段 cfg 掉。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn demo_desktop_sized(_w: usize, _h: usize) -> Desktop {
    // P4.1：**默认桌面不开任何窗口**（用户要求）—— 窗口改由桌面图标 / Dock 打开。
    // 走查与预览路径仍用 `demo_desktop()`（3 窗口布局演示），不受影响。
    let wins: Vec<Win> = Vec::new();
    let (cwd, entries, dir_error) = match shot_fixture() {
        Some(f) => f,
        None => {
            let cwd = draw::default_cwd();
            let (entries, dir_error) = draw::read_dir_entries(&cwd);
            (cwd, entries, dir_error)
        }
    };
    let mut d = Desktop { wins, active: 0, layout: Layout::TwoCol, cwd, entries, selected: None, scroll: 0, clipboard: String::new(), sidebar: sidebar_targets(), dir_error };
    sync_files_title(&mut d);
    d
}

/// 走查夹具：一份**固定**的目录清单（§4.1 的截图回归判据需要可复现的输出）。
///
/// 为什么需要它：`demo_desktop()` 会去读 `$HOME` 的真实内容 ——
/// 于是同一个构建连跑两遍，文件窗口里看到的东西就不同，截图没法当"像素回归"的判据
/// （实测见审计附录 CD.3）。
///
/// 用 `AETHER_SHOT_FIXTURE=1` 打开（与 `AETHER_FAKE_UTC` 同一风格的环境钩子）。
/// 夹具是**中文名 + 英文名 + 不同大小**的组合：走查时要能看出列宽与大小格式化。
pub(crate) fn shot_fixture() -> Option<(String, Vec<draw::FsEntry>, Option<String>)> {
    if std::env::var("AETHER_SHOT_FIXTURE").is_err() {
        return None;
    }
    let f = |name: &str, is_dir: bool, size: u64| draw::FsEntry {
        name: name.to_string(),
        is_dir,
        size,
    };
    Some((
        "/home/aether".to_string(),
        vec![
            f("文档", true, 0),
            f("图片", true, 0),
            f("下载", true, 0),
            f("笔记.txt", false, 1284),
            f("预算.csv", false, 20480),
            f("安装说明.md", false, 3072),
            f("readme.md", false, 512),
        ],
        None,
    ))
}

#[cfg_attr(target_os = "linux", allow(dead_code))] // 仅预览/走查路径使用
pub(crate) fn demo_desktop() -> Desktop {
    let mut wins = vec![
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::win_label(text::strings::WIN_FILES).to_string(), kind: draw::WinKind::Files, floating: false, restore: None, preview: None, term: None, wayland: None },
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::win_label(text::strings::WIN_TERM).to_string(), kind: draw::WinKind::Terminal, floating: false, restore: None, preview: None, term: Some(term::Terminal::spawn(80, 24, None)) , wayland: None },
        Win { rect: Rect { x: 0, y: 0, w: 0, h: 0 }, target: None, title: text::strings::win_label(text::strings::WIN_MUSIC).to_string(), kind: draw::WinKind::Music, floating: false, restore: None, preview: None, term: None, wayland: None },
    ];
    let work = layout::work_area(WIDTH, HEIGHT);
    let tg = layout::tiled_targets(wins.len(), Layout::TwoCol, work);
    for (i, w) in wins.iter_mut().enumerate() {
        w.rect = tg[i].unwrap();
    }
    let active = wins.len() - 1;
    // 确定模式（`AETHER_SHOT_FIXTURE=1`）下用固定夹具，不去读真实目录
    let (cwd, entries, dir_error) = match shot_fixture() {
        Some(f) => f,
        None => {
            let cwd = draw::default_cwd();
            let (entries, dir_error) = draw::read_dir_entries(&cwd);
            (cwd, entries, dir_error)
        }
    };
    let mut d = Desktop { wins, active, layout: Layout::TwoCol, cwd, entries, selected: None, scroll: 0, clipboard: String::new(), sidebar: sidebar_targets(), dir_error };
    // 文件窗口的标题要跟随当前目录，而不是写死的"文件"
    sync_files_title(&mut d);
    d
}

/// L2+ 操作待用户确认（aetherd 签发的一次性令牌）。
/// 部分字段只在 fbdev 路径（Linux）被读取，Windows 预览构建下允许未使用。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct ConfirmRequest {
    pub(crate) tool: String,
    pub(crate) level: u8,
    /// 参数（已转成展示用的键值对）
    pub(crate) arguments: Vec<(String, String)>,
    /// 原始参数：重发时必须逐字不变（令牌绑定参数）
    pub(crate) raw_arguments: serde_json::Value,
    pub(crate) consequence: String,
    /// L3 需原样输入的目标；None = 只需点确认
    pub(crate) echo_required: Option<String>,
    /// 一次性确认令牌
    pub(crate) token: String,
    /// 发起方：决定执行结果回填给谁
    pub(crate) origin: ConfirmOrigin,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum ConfirmOrigin {
    /// AI 对话触发（结果进气泡）
    Ai,
    /// 安装向导触发（结果回填向导）
    Installer,
    /// 控制中心的电源入口触发（结果只进 toast —— 没有对话气泡可挂）
    Power,
    /// 「应用商店」点「安装」触发（结果进 toast，并刷新该行的状态）
    Store,
}

/// 控制中心交回给主循环的**意图**。
///
/// 为什么不让 `control_center_click` 直接执行：那个函数是纯逻辑、被单测覆盖
/// （Windows 上也会跑），而"关机"要发 IPC、"注销"要退出进程 —— 两件事都不该
/// 在单测里发生。所以它只**判定**，由主循环执行（与设置中心的动作类命中同一套规矩：
/// `settings.rs::apply_hit` 对 `RestartService` 也是只占位不执行）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum UiIntent {
    /// 关机 / 重启（经 system_power 工具 → 确认卡片 → aetherd → PID 1）
    Power(aether_ipc::PowerAction),
    /// 注销：重启合成器（由 aether-init 按 compositor.json 的 restart:true 拉起）
    Logout,
    /// 设定音量（0..=100）：由点击音量轨道的位置算出（批次 K-2）
    SetVolume(u8),
    /// 新增一个虚拟桌面（§5.4）。与电源同理：**判定与执行分开** ——
    /// `control_center_click` 是纯逻辑（单测覆盖、Windows 上也跑），
    /// 而增删要碰事件循环里的 `spaces` 与窗口数组。
    AddSpace,
    /// 移除当前虚拟桌面（§5.4）。里面有窗口时会被拒绝（并把原因告诉用户）。
    RemoveSpace,
}

impl ConfirmRequest {
    /// 从 IPC 响应构造（参数值统一转成字符串展示）
    pub(crate) fn from_ipc(tool: String, level: u8, arguments: serde_json::Value, consequence: String, echo_required: Option<String>, token: String, origin: ConfirmOrigin) -> Self {
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
    pub(crate) fn echo_ok_input(&self, echo: &str) -> bool {
        match &self.echo_required {
            Some(target) => echo.trim() == target,
            None => true,
        }
    }
}

/// 用户点"允许一次"：带一次性令牌重发工具调用。
/// 令牌由服务端签发并绑定 (tool, 参数)，服务端校验通过才真正执行——UI 这一步
/// 只是把"用户看过并同意"转达给闸门，而不是绕过闸门。
pub(crate) fn dispatch_approved(req: ConfirmRequest, tx: &mpsc::Sender<AiEvent>) {
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
pub(crate) fn request_delete(desktop: &Desktop, tx: &mpsc::Sender<AiEvent>) -> Option<ToastMsg> {
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
    Some(ToastMsg::new(format!("请求删除「{name}」…"), format!("Requesting deletion of \"{name}\"…")))
}

/// AI 事件（后台线程 → 渲染主循环）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// §2.3：口令表单不合规的**文案**（措辞按界面语言渲染，码来自 `settings`）。
pub(crate) fn form_problem_text(lang: crate::i18n::Lang, p: crate::settings::FormProblem) -> String {
    use crate::i18n::Lang;
    use crate::settings::FormProblem as F;
    match (p, lang) {
        (F::OldEmpty, Lang::En) => "enter your current password".to_string(),
        (F::OldEmpty, Lang::Zh) => "请先填旧口令".to_string(),
        (F::NewEmpty, Lang::En) => "enter a new password".to_string(),
        (F::NewEmpty, Lang::Zh) => "请填新口令".to_string(),
        (F::Mismatch, Lang::En) => "the two new passwords do not match".to_string(),
        (F::Mismatch, Lang::Zh) => "两次输入的新口令不一致".to_string(),
        (F::SameAsOld, Lang::En) => "the new password is the same as the old one".to_string(),
        (F::SameAsOld, Lang::Zh) => "新口令与旧口令相同（等于没改）".to_string(),
        (F::Policy(p), Lang::En) => format!("password rejected: {}", p.code()),
        (F::Policy(p), Lang::Zh) => format!("口令不合规：{}", p.code()),
    }
}

// 非 Linux 上不会被调用：设置命中的处理块在 Linux 专属路径里（非 Linux 不连 aetherd）。
// 如实标注而不是删掉功能 —— 这一页在目标平台上是要用的。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// §3.7：读一次外设检测（只读）。
///
/// 与 `fetch_removables` 同一架构：开一个线程发一次请求，把结果包成事件送回主循环。
pub(crate) fn fetch_peripherals(tx: mpsc::Sender<AiEvent>) {
    #[cfg(not(target_os = "linux"))]
    {
        // 非 Linux 不连 aetherd —— 如实报"检测不到"，而不是假装成功
        let _ = tx.send(AiEvent::PeripheralsLoaded { list: Vec::new() });
        return;
    }
    #[cfg(target_os = "linux")]
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::ListPeripherals) {
            Ok(Response::PeripheralList { list }) => AiEvent::PeripheralsLoaded { list },
            _ => AiEvent::PeripheralsLoaded { list: Vec::new() },
        };
        let _ = tx.send(ev);
    });
}

// 非 Linux 上不会被调用：设置命中的处理块在 Linux 专属路径里（非 Linux 不连 aetherd）。
// 如实标注而不是删掉功能 —— 这一页在目标平台上是要用的。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// §2.3：读一次账户列表。
///
/// 与 `fetch_removables` 同一架构：**开一个线程发一次请求**，把结果包成事件送回主循环。
pub(crate) fn fetch_users(tx: mpsc::Sender<AiEvent>) {
    // `admin_request` 是 Linux 专属（要走 Unix socket 连 aetherd）——
    // 非 Linux 上如实回一句"不适用"，而不是假装成功。
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tx.send(AiEvent::PasswordAck {
            ok: false,
            message: "账户列表需要 aetherd（本平台不连它）".to_string(),
        });
        return;
    }
    #[cfg(target_os = "linux")]
    std::thread::spawn(move || {
        let ev = match admin_request(&Request::ListUsers) {
            Ok(Response::UserList { list }) => AiEvent::UsersLoaded { list },
            Ok(Response::Error { code, message }) => AiEvent::PasswordAck {
                ok: false,
                message: format!("读取账户列表被拒绝（{code}）：{message}"),
            },
            Ok(_) => AiEvent::PasswordAck {
                ok: false,
                message: "读取账户列表得到意料之外的回复".to_string(),
            },
            Err(e) => AiEvent::PasswordAck {
                ok: false,
                message: format!("读取账户列表失败：{e}"),
            },
        };
        let _ = tx.send(ev);
    });
}

// 非 Linux 上不会被调用：设置命中的处理块在 Linux 专属路径里（非 Linux 不连 aetherd）。
// 如实标注而不是删掉功能 —— 这一页在目标平台上是要用的。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// §2.3：提交改口令。
///
/// **旧口令在这一侧校验**（合成器有 `sha512crypt`，锁屏在用）：
/// 先证明"你是这个人"，再让 aetherd 写新口令。
pub(crate) fn submit_password(
    renderer: &mut draw::Renderer,
    toast: &mut Option<(ToastMsg, Instant)>,
    tx: mpsc::Sender<AiEvent>,
) {
    if renderer.pass_form.pending {
        return;
    }
    // ① 本地表单校验（纯函数，规则与服务端共用同一份）
    if let Err(problem) = renderer.pass_form.check() {
        let msg = toast_msg_lang(|l| form_problem_text(l, problem));
        renderer.pass_form.feedback = Some(msg.text().to_string());
        push_toast(toast, &mut renderer.notifs, msg, notifications::Kind::Err);
        return;
    }
    // ② 目标账户：单用户模型下就是第一个人类账户
    let Some(user) = renderer.users.first().map(|u| u.name.clone()) else {
        let msg = toast_msg(
            "还没读到账户列表，请先点「重新读取」",
            "The account list is not loaded yet; click Reload first",
        );
        renderer.pass_form.feedback = Some(msg.text().to_string());
        push_toast(toast, &mut renderer.notifs, msg, notifications::Kind::Err);
        return;
    };
    // ③ 旧口令校验：读 shadow → 解析 → 判定（与锁屏同一套 `auth`）
    let old = renderer.pass_form.old.text.clone();
    let shadow = std::fs::read_to_string("/etc/shadow").unwrap_or_default();
    let entry = crate::auth::parse_shadow(&shadow, &user);
    let mut lockout = crate::auth::Lockout::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match crate::auth::decide(&entry, &old, &mut lockout, now) {
        crate::auth::Decision::Allow | crate::auth::Decision::AllowWithReason(_) => {}
        crate::auth::Decision::Deny(why) => {
            let msg = toast_msg_1("旧口令不对：{}", "current password rejected: {}", why);
            renderer.pass_form.feedback = Some(msg.text().to_string());
            push_toast(toast, &mut renderer.notifs, msg, notifications::Kind::Err);
            return;
        }
        crate::auth::Decision::Wait(secs) => {
            let msg = toast_msg_1("尝试过于频繁，请等 {} 秒", "too many attempts, wait {}s", secs);
            renderer.pass_form.feedback = Some(msg.text().to_string());
            push_toast(toast, &mut renderer.notifs, msg, notifications::Kind::Err);
            return;
        }
    }
    // ④ 交给 aetherd 写（新口令只在这条**已注册的 UI 通道**上传输）
    renderer.pass_form.pending = true;
    renderer.pass_form.feedback = Some(crate::i18n::tr("正在提交…", "Submitting…").to_string());
    let new_password = renderer.pass_form.new.text.clone();
    let en = crate::i18n::is_en();
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (user, new_password, en, tx);
        renderer.pass_form.pending = false;
        let msg = toast_msg(
            "改口令需要 aetherd（本平台不连它）",
            "Changing a password needs aetherd (not connected on this platform)",
        );
        renderer.pass_form.feedback = Some(msg.text().to_string());
        push_toast(toast, &mut renderer.notifs, msg, notifications::Kind::Err);
        return;
    }
    #[cfg(target_os = "linux")]
    std::thread::spawn(move || {
        let req = Request::SetPassword {
            user,
            new_password,
            en,
        };
        let ev = match admin_request(&req) {
            Ok(Response::PasswordAck { ok, message }) => AiEvent::PasswordAck { ok, message },
            Ok(Response::Error { code, message }) => AiEvent::PasswordAck {
                ok: false,
                message: format!("改口令被拒绝（{code}）：{message}"),
            },
            Ok(_) => AiEvent::PasswordAck {
                ok: false,
                message: "改口令得到意料之外的回复".to_string(),
            },
            Err(e) => AiEvent::PasswordAck {
                ok: false,
                message: format!("改口令失败：{e}"),
            },
        };
        let _ = tx.send(ev);
    });
}

/// 非 Linux 上，**有几个变体不会被构造**：它们都来自"要连 aetherd"的路径
/// （`admin_request` 是 Linux 专属）。这是事实，不是缺陷 —— 标注清楚，
/// 免得后来的人以为可以把它们删掉。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum AiEvent {
    /// 回复文本 + 推理通道（Some("local")/Some("cloud")；None = 未知）。
    /// 顶栏 AI 三态（本地青/云端紫/离线灰）据此更新（ui-design-handover 4.1）。
    Reply(String, Option<String>),
    Action(String, serde_json::Value),
    Error(String),
    /// 工具调用需要用户确认（L2+）
    Confirm(Box<ConfirmRequest>),
    /// 确认后执行的结果（或安装结果）
    ToolDone {
        ok: bool,
        output: String,
        /// 只在 Linux 的 fbdev 循环里被读（非 Linux 上是 dead code）——
        /// 如实标注而不是删掉：这是**目标平台**上要用的字段。
        #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
        origin: ConfirmOrigin,
    },

    /// DNS 写入的回执（§2.4）。**单独一个变体**，不复用 `ToolDone`：
    /// 那是"AI 工具调用完成"的语义，会让设置页的一次保存显示成 AI 回复。
    /// 只在 `set_dns` 里被构造，而它依赖 Linux 专属的 `admin_request`。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    DnsSaved { ok: bool, message: String },
    /// 权限策略列表取回来了（§5.3）。
    PolicyLoaded(Vec<aether_ipc::PolicyEntryWire>),
    /// 权限策略写入的回执（带上"写成了什么"，视图据此更新那一行）。
    PolicySaved {
        ok: bool,
        message: String,
        tool: String,
        policy: aether_ipc::ToolPolicyWire,
    },
    /// 取/写权限策略失败。
    PolicyFailed(String),
    /// 时间同步状态取回来了（§3.3）。
    TimeStatus {
        timezone: String,
        last_offset_ms: i64,
        last_sync_unix: i64,
    },
    /// 对时完成（或失败）。
    ///
    /// 不带偏移数值：服务端已经把它格式化进 `message`（如「偏移 12 ms · 已平滑调整」），
    /// 界面要的是那句话；而"上次偏移"另由 `TimeStatus` 提供（那才是要显示在页面上的）。
    TimeSynced {
        ok: bool,
        message: String,
    },
    /// 系统时区写入结果。
    TimezoneSet {
        ok: bool,
        message: String,
    },
    /// 快照清单取回来了（§5.6）。
    SnapshotsLoaded(Vec<aether_ipc::SnapshotWire>),
    /// 快照创建/回滚的回执（`notes` 是回滚时逐文件的说明）。
    SnapshotDone {
        ok: bool,
        message: String,
        notes: Vec<String>,
    },
    /// 更新列表取回来了（§3.1）。
    UpdatesLoaded {
        packages: Vec<String>,
        current: String,
        staged: Option<String>,
    },
    /// 暂存更新的回执。
    UpdateResult {
        ok: bool,
        message: String,
        version: Option<String>,
    },
    /// 显示输出取回来了（§3.8 只读报告）。
    DisplaysLoaded {
        driven_index: u32,
        outputs: Vec<aether_ipc::DisplayWire>,
    },
    /// 可移动设备扫描结果（§3.7）。
    /// §2.3：账户列表读到了
    ///
    /// 非 Linux 上不会被构造（不连 aetherd）—— 如实标注，不删。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    UsersLoaded { list: Vec<aether_ipc::UserInfo> },
    /// §2.3：改口令的结果（同上：非 Linux 不构造）
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    PasswordAck { ok: bool, message: String },
    /// §3.7：外设（蓝牙适配器 / 打印子系统）检测结果
    PeripheralsLoaded { list: Vec<aether_ipc::PeripheralWire> },
    RemovablesLoaded {
        devices: Vec<aether_ipc::RemovableWire>,
    },
    /// 挂载/卸载的回执（§3.7）。
    RemovableAck {
        ok: bool,
        message: String,
    },
}

/// 安装向导状态（仅 Live ISO 会话；run_fbdev 持有）。
#[cfg(target_os = "linux")]
pub(crate) struct Installer {
    pub(crate) open: bool,
    pub(crate) disks: Vec<(String, u64)>,
    pub(crate) selected: usize,
    pub(crate) running: bool,
    /// (结果文案, 是否错误)
    pub(crate) message: Option<(String, bool)>,
}

#[cfg(target_os = "linux")]
impl Installer {
    pub(crate) fn new() -> Self {
        Self { open: false, disks: Vec::new(), selected: 0, running: false, message: None }
    }

    /// 打开向导并扫描磁盘（每次打开重扫，热插拔友好）。
    pub(crate) fn open(&mut self) {
        self.disks = scan_disks();
        if self.selected >= self.disks.len() {
            self.selected = 0;
        }
        self.open = true;
        tty_log(&format!("安装向导打开，发现 {} 块磁盘", self.disks.len()));
    }

    pub(crate) fn snapshot(&self) -> Option<InstallerUi<'_>> {
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
pub(crate) fn scan_disks() -> Vec<(String, u64)> {
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

/// UI 通道密钥（P1-8）：与 aetherd 同源 —— 环境变量优先，否则读 `/run/aether/ui.key`。
///
/// 路径在 2026-10-02 安全审计（H-1）中从 `/var/log/aether/` 迁出：旧位置在
/// `read_file` 的读取白名单内，任意本机进程可用 AI 工具把密钥取走。
pub(crate) fn ui_key() -> Option<String> {
    if let Ok(k) = std::env::var("AETHER_UI_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    std::fs::read_to_string(aether_ipc::UI_KEY_PATH)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 在已建立的连接上注册为 UI 通道。
///
/// 这是 P1-8 的客户端侧配套：服务端只把确认令牌下发给**已注册的 UI 通道**，
/// 也只接受已注册通道的兑现请求。没有这一步，L2+ 操作会被服务端直接拒绝，
/// 令牌也不再有"任何人都能兑现"的漏洞。
pub(crate) fn register_ui(stream: &mut IpcStream, reader: &mut BufReader<IpcStream>) -> anyhow::Result<()> {
    let key = ui_key().ok_or_else(|| {
        anyhow::anyhow!(
            "未找到 UI 通道密钥（设置 AETHER_UI_KEY，或确认 aetherd 已生成 /run/aether/ui.key）"
        )
    })?;
    stream.write_all(aether_ipc::encode(&Request::RegisterUi {
        key,
        en: crate::i18n::is_en(),
    })
    .as_bytes())?;
    let line = read_line_capped(reader, MAX_IPC_LINE)?;
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
pub(crate) fn cancel_confirm(token: &str) {
    let token = token.to_string();
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<()> {
            let mut stream = IpcStream::connect(Duration::from_secs(3))?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut reader = BufReader::new(stream.try_clone()?);
            register_ui(&mut stream, &mut reader)?;
            stream.write_all(aether_ipc::encode(&Request::ConfirmCancel { token }).as_bytes())?;
            let line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
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
pub(crate) fn run_install(disk: String, tx: mpsc::Sender<AiEvent>) {
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
pub(crate) fn send_tool_call(
    tool: String,
    arguments: serde_json::Value,
    approval: Option<String>,
    origin: ConfirmOrigin,
    tx: &mpsc::Sender<AiEvent>,
) -> anyhow::Result<()> {
    let mut stream = IpcStream::connect(Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(300))).ok();
    let mut reader = BufReader::new(stream.try_clone()?);
    // 先注册为 UI 通道：确认令牌只下发给已注册通道，也只被已注册通道兑现（P1-8）
    register_ui(&mut stream, &mut reader)?;
    let req = Request::ToolCall { session_id: "shell-preview".into(), tool, arguments, approval };
    stream.write_all(aether_ipc::encode(&req).as_bytes())?;
    let line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
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

/// §4.1 自动 e2e：**无界面**执行一段动作脚本，逐条输出结果（JSONL）。
///
/// 为什么需要它：FEATURE-GAPS §4.1 说"现在每个功能都要人工点一次"，
/// 于是所有验证都退化成"我看着没问题"，而 Linux 专属路径在开发机上根本跑不到。
/// 这个运行器把"动作 → 结果"变成可脚本化、可比对的文本，
/// 让 e2e 至少覆盖**不依赖真实显示与硬件**的那一半（布局 / 开窗 / 撤销 / 查询）。
///
/// 三条自我约束：
/// 1. **只暴露真实动作**（`apply_action` 的那几个 + `undo` + `query`）——
///    不新造"只在这个模式里存在"的动作，否则 e2e 通过而真实点击失败；
/// 2. 非法输入（不是 JSON / 未知动作 / 参数不合法）**逐条如实报错并继续**，
///    不整批中止 —— 一次跑完看到全部问题，比修一个跑一次省时间；
/// 3. 输出是**机器可读**的 JSONL（Python 侧断言用），不是给人看的散文。
/// 画一帧，把**命中表**刷新到当前状态（§4.1）。
///
/// 点击与 `query: hits` 共用它：命中区是画帧时登记的，
/// 而"展开控制中心"这个状态变化发生在渲染**之后** ——
/// 不重画就会读到上一帧的空表。
pub(crate) fn inj_render_once(
    renderer: &mut draw::Renderer,
    buf: &mut [u32],
    tr: Option<&text::TextRenderer>,
    desktop: &Desktop,
    ai_input: &str,
    ime: &ime::Ime,
    mouse: (f32, f32),
) {
    let ui = draw::UiState {
        snap: None,
        toast: None,
        ai_input,
        ime,
        ai_cursor: 0,
        ai_focused: false,
        ai_thinking: false,
        ai_status: draw::AiStatus::Offline,
        ai_reply: None,
        mouse,
        mouse_down: false,
        audio_label: "",
        audio_volume: 0,
        audio_enabled: false,
        open_menu: None,
        show_installer: false,
        installer: None,
        confirm: None,
    };
    renderer.prepare_background(WIDTH, HEIGHT, 1.2);
    renderer.render_frame(buf, WIDTH, HEIGHT, 1.2, desktop, &ui, tr);
}

pub(crate) fn run_e2e_script(
    script: &str,
    desktop: &mut Desktop,
    undo: &mut UndoStack,
    spaces: &mut spaces::Spaces<draw::Win>,
) -> Vec<String> {
    let mut out = Vec::new();
    // §4.1：注入用的 UI 侧状态。
    // 为什么只到这里为止：`dispatch_nav` 是**两条真实输入路径共用**的分流器，
    // 所以用同一个函数就等于注入到了真实逻辑上，而不是另写一套按键处理。
    let mut inj_input = String::new();
    let mut inj_cursor = textview::TextCursor::default();
    let mut inj_ime = ime::Ime::default();
    let (inj_tx, _inj_rx) = mpsc::channel::<AiEvent>();
    // 点击要用：命中表是**画帧时**填的，所以这里要有一个真实的 Renderer。
    // 无显示设备也能画（画进内存缓冲）—— 这正是 `--shot` 那条路径的做法。
    let mut inj_renderer = draw::Renderer::new(WIDTH, HEIGHT);
    let inj_tr = text::TextRenderer::load();
    let mut inj_buf = vec![0u32; WIDTH * HEIGHT];
    let mut inj_toast: Option<(ToastMsg, Instant)> = None;
    // 通知历史也要有一份：`push_toast` 是"显示 + 记历史"的同一个入口，
    // 少一半就不是真实路径了（§4.1 的原则：注入要打在真实函数上）。
    let mut inj_notifs = notifications::Notifications::new();
    // `probe()` 是真实的构造入口（会去问一次 mixer；拿不到就如实标未检测）
    let mut inj_audio = audio::Audio::probe();
    for (idx, raw) in script.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let emit = |action: &str, ok: bool, result: &str, out: &mut Vec<String>| {
            let res = serde_json::to_string(result).unwrap_or_else(|_| "\"\"".into());
            out.push(format!(
                "{{\"line\":{lineno},\"action\":\"{action}\",\"ok\":{ok},\"result\":{res}}}"
            ));
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            emit("(parse)", false, "不是合法 JSON", &mut out);
            continue;
        };
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("").to_string();
        let args = v.get("args").cloned().unwrap_or(serde_json::Value::Null);
        match action.as_str() {
            // 查询：让脚本能**断言状态**（否则只能断言"动作返回了成功"）
            "query" => {
                let what = args.get("what").and_then(|w| w.as_str()).unwrap_or("");
                match what {
                    "layout" => emit("query", true, desktop.layout.label(), &mut out),
                    "windows" => emit("query", true, &desktop.wins.len().to_string(), &mut out),
                    "active" => emit("query", true, &desktop.active.to_string(), &mut out),
                    // §4.1：注入之后要**断得准**，所以把注入产生的结果也做成可查询状态
                    "ai_input" => emit("query", true, &inj_input, &mut out),
                    "space" => emit("query", true, &spaces.current().to_string(), &mut out),
                    // §3.5 遗留项 #43：通知正文现在存的是**结构化双语**，
                    // 所以"当前这条提示用哪种语言、有没有漏译"可以被脚本断言。
                    "toast" => {
                        let t = inj_toast.as_ref().map(|(m, _)| m.text().to_string());
                        emit("query", true, t.as_deref().unwrap_or(""), &mut out)
                    }
                    // §4.1：点击的效果要能断言
                    "dark" => emit(
                        "query",
                        true,
                        if inj_renderer.settings.dark_mode { "true" } else { "false" },
                        &mut out,
                    ),
                    "control_open" => emit(
                        "query",
                        true,
                        if inj_renderer.control_open { "true" } else { "false" },
                        &mut out,
                    ),
                    // §4.1：把**真实**可点区域报出来。
                    // 脚本是静态的、测试是动态的 —— 后者据此算出坐标再点，
                    // 所以"点得到"这件事有据可依，而不是把坐标写死。
                    "status_cluster" => {
                        // 与 hits 同理：状态簇的位置也是**画帧时**定的，不先画就是 0,0,0,0
                        inj_render_once(
                            &mut inj_renderer,
                            &mut inj_buf,
                            inj_tr.as_ref(),
                            desktop,
                            &inj_input,
                            &inj_ime,
                            (0.0, 0.0),
                        );
                        let r = inj_renderer.status_cluster;
                        emit("query", true, &format!("{},{},{},{}", r.x, r.y, r.w, r.h), &mut out)
                    }
                    "hits" => {
                        // 先刷新到当前状态，否则读到的是"上一次点击之前"的命中表
                        inj_render_once(
                            &mut inj_renderer,
                            &mut inj_buf,
                            inj_tr.as_ref(),
                            desktop,
                            &inj_input,
                            &inj_ime,
                            (0.0, 0.0),
                        );
                        let list: Vec<String> = inj_renderer
                            .control_hits
                            .iter()
                            .map(|(r, h)| format!("{},{},{},{},{:?}", r.x, r.y, r.w, r.h, h))
                            .collect();
                        emit("query", true, &list.join("|"), &mut out)
                    }
                    // 窗口标题用连接符分隔：脚本据此判断"打开的是不是设置窗口"
                    "titles" => {
                        let names: Vec<&str> =
                            desktop.wins.iter().map(|w| w.title.as_str()).collect();
                        emit("query", true, &names.join("|"), &mut out)
                    }
                    other => emit(
                        "query",
                        false,
                        &format!("未知查询「{other}」（可用：layout / windows / active / titles）"),
                        &mut out,
                    ),
                }
            }
            // §4.1 键盘注入：`{"action":"nav","args":{"key":"down"}}`
            // 走 `dispatch_nav`（真机 evdev 与预览 minifb 共用它），
            // 所以"注入"跑的就是真实分流，不是平行实现。
            "nav" => {
                let key = args.get("key").and_then(|k| k.as_str()).unwrap_or("");
                let nav = match key {
                    "up" => Some(input::NavKey::Up),
                    "down" => Some(input::NavKey::Down),
                    "left" => Some(input::NavKey::Left),
                    "right" => Some(input::NavKey::Right),
                    "home" => Some(input::NavKey::Home),
                    "end" => Some(input::NavKey::End),
                    "pageup" | "pgup" => Some(input::NavKey::PageUp),
                    "pagedown" | "pgdn" => Some(input::NavKey::PageDown),
                    "delete" | "del" => Some(input::NavKey::Delete),
                    _ => None,
                };
                match nav {
                    Some(nav) => {
                        let _ = dispatch_nav(
                            desktop,
                            nav,
                            &mut inj_input,
                            &mut inj_cursor,
                            &mut inj_ime,
                            &inj_tx,
                        );
                        emit("nav", true, key, &mut out);
                    }
                    None => emit(
                        "nav",
                        false,
                        &format!("未知导航键「{key}」（up/down/left/right/home/end/pageup/pagedown/delete）"),
                        &mut out,
                    ),
                }
            }
            // §4.1 坐标点击：`{"action":"click","args":{"x":120,"y":8}}`
            // 命中表由 `render_frame` 填写 → 找命中的控件 → `control_center_click`
            // （主循环用的同一个函数）→ 产生的 `UiIntent` 交给 `apply_ui_intent`（同一个）。
            "click" => {
                let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
                let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
                inj_render_once(
                    &mut inj_renderer,
                    &mut inj_buf,
                    inj_tr.as_ref(),
                    desktop,
                    &inj_input,
                    &inj_ime,
                    (x, y),
                );
                let mut intent = None;
                let handled = control_center_click(
                    &mut inj_renderer,
                    desktop,
                    &mut inj_ime,
                    x,
                    y,
                    true,
                    &mut intent,
                );
                if let Some(intent) = intent {
                    apply_ui_intent(intent, &inj_tx, &mut inj_toast, &mut inj_renderer, &mut inj_audio);
                }
                if handled {
                    emit("click", true, &format!("{x},{y}"), &mut out);
                } else {
                    emit("click", false, &format!("({x},{y}) 处没有可点的控件"), &mut out);
                }
            }
            // §4.1 字符注入：`{"action":"key","args":{"ch":"a"}}` —— 打进 AI 指令条
            "key" => {
                let ch = args.get("ch").and_then(|c| c.as_str()).unwrap_or("");
                let mut it = ch.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => {
                        inj_input.push(c);
                        inj_cursor.end(inj_input.chars().count());
                        emit("key", true, ch, &mut out);
                    }
                    _ => emit("key", false, "ch 必须是恰好一个字符", &mut out),
                }
            }
            "undo" => match undo_last(desktop, undo) {
                Some(msg) => emit("undo", true, msg.text(), &mut out),
                None => emit("undo", false, "没有可撤销的动作", &mut out),
            },
            // §3.5 遗留项 #43：脚本内切语言（**不是 CLI 顺序问题**：
            // `--e2e` 在 main 里提前 return，`--lang` 那种走查参数够不着这里）。
            "lang" => {
                let want = args.get("lang").and_then(|v| v.as_str()).unwrap_or("zh");
                let l = if want.eq_ignore_ascii_case("en") {
                    crate::i18n::Lang::En
                } else {
                    crate::i18n::Lang::Zh
                };
                crate::i18n::set_lang(l);
                emit("lang", true, if l == crate::i18n::Lang::En { "en" } else { "zh" }, &mut out);
            }
            _ => match apply_action(desktop, &action, &args, undo) {
                Some(msg) => {
                    // 真实产出的通知正文同时进 toast 槽与历史 —— `query: toast`
                    // 断言的因此是**主循环同一个 `push_toast`** 的结果。
                    push_toast(&mut inj_toast, &mut inj_notifs, msg.clone(), notifications::Kind::Info);
                    emit(&action, true, msg.text(), &mut out)
                }
                None => emit(
                    &action,
                    false,
                    &format!("未知动作或参数不合法：{action}"),
                    &mut out,
                ),
            },
        }
    }
    out
}

/// 在后台线程里连接 aetherd、发送 Chat、收集响应直到 done。
/// 每一步都打串口/控制台日志（QEMU TCG 下截图窗口太窄，日志是可靠诊断通路）。
pub(crate) fn query_aether(text: String, timeout_secs: u64, tx: mpsc::Sender<AiEvent>) {
    let run = || -> anyhow::Result<()> {
        println!("aether-compositor: AI 查询「{text}」连接 aetherd…");
        // connect_timeout：aetherd 不可达时快速失败，而不是无限阻塞
        let mut stream = IpcStream::connect(Duration::from_secs(3))
            .map_err(|e| anyhow::anyhow!("aetherd 不可达（127.0.0.1:{}）：{e}", aether_ipc::DEFAULT_PORT))?;
        println!("aether-compositor: AI 查询已连接，发送请求（超时 {timeout_secs}s）");
        stream.set_read_timeout(Some(Duration::from_secs(timeout_secs))).ok();
        let mut reader = BufReader::new(stream.try_clone()?);
        // 先注册为 UI 通道：否则 AI 触发的 L2+ 操作拿不到确认令牌（P1-8）
        register_ui(&mut stream, &mut reader)?;
        stream.write_all(
            aether_ipc::encode(&Request::Chat {
        session_id: "shell-preview".into(),
        text,
        en: crate::i18n::is_en(),
    }).as_bytes(),
        )?;
        // 响应行在循环里读（每轮覆盖），这里不预置初值 —— 预置了反而会得到
        // "赋值后从未读取"的警告，而且容易让人以为它有意义。
        let mut line: String;
        let mut reply = String::new();
        let mut action: Option<(String, serde_json::Value)> = None;
        let mut confirm: Option<ConfirmRequest> = None;
        // 最后一条 ChatChunk 携带的推理通道（"local"/"cloud"），决定顶栏三态
        let mut channel: Option<String> = None;
        loop {
            line = read_line_capped(&mut reader, MAX_IPC_LINE)?;
            if line.is_empty() {
                break;
            }
            match aether_ipc::decode::<Response>(&line) {
                Ok(Response::ChatChunk { delta, done, channel: ch, .. }) => {
                    // 累计上限（审计 L-14）：模型可以一直吐，合成器不能一直吃
                    if reply.len() + delta.len() > MAX_CHAT_BYTES {
                        anyhow::bail!(
                            "AI 回复超过 {} 字节上限，已中止本轮（防内存耗尽）",
                            MAX_CHAT_BYTES
                        );
                    }
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

/// 撤销栈上限。**有界**是硬要求：无界栈会随运行时长无限增长，
/// 而"撤销到很久以前"本来也没意义（那是快照/回滚 §5.6 的事）。
pub(crate) const UNDO_CAP: usize = 20;

/// 一步可撤销的 AI 动作（§5.1）。
///
/// **不存窗口**：`Win.term` 是活的 PTY 会话（`term::Terminal`），本质不可克隆 ——
/// 所以"撤销关窗"不能靠存整份窗口实现。宁可**如实说撤不了**，也不做假撤销。
pub(crate) enum UndoStep {
    /// 布局被改：记上一份布局 + 每个窗口原来的浮动标志。
    /// （正向动作会把所有窗口的 `floating` 清掉；只还原布局会让原本浮动的窗口继续被
    ///  平铺，用户会认为"撤销没生效"。）
    Layout { prev: Layout, floating: Vec<bool> },
    /// 打开了窗口：记下标（撤销 = 关掉它）
    OpenedWindow(usize),
    /// 关掉了窗口。**可以撤销** —— 窗口（连同它的活会话）被移进了"最近关闭"托盘，
    /// 撤销就是把它放回原位。记下标是为了放回**原来的位置**，而不是一律扔到末尾。
    ClosedWindow { at: usize, title: String },
}

/// 「最近关闭」托盘的容量。
///
/// 为什么是 3：托盘里的窗口**还活着**（PTY 进程继续跑、继续占内存）。
/// 它能撤销，代价就是这些资源不释放 —— 所以必须小。
/// 满了丢**最旧**的（那时它那一步撤销会因为标题对不上而如实报"无法撤销"）。
pub(crate) const CLOSED_CAP: usize = 3;

/// 往有界托盘里放一个窗口，返回被挤出去的那个（没有就是 `None`）。
///
/// 抽成纯函数是为了**能测**：容量为 0、恰好满、超出一个，
/// 这三种情形用 `u32` 就能验，不必构造真窗口。
pub(crate) fn tray_push<T>(tray: &mut Vec<T>, item: T, cap: usize) -> Option<T> {
    if cap == 0 {
        return Some(item); // 容量为 0 = 不保留
    }
    let evicted = if tray.len() >= cap {
        Some(tray.remove(0))
    } else {
        None
    };
    tray.push(item);
    evicted
}

/// 撤销栈（有界，新的在后）。
pub(crate) struct UndoStack {
    pub(crate) steps: std::collections::VecDeque<UndoStep>,
    /// 「最近关闭」托盘（§5.1）：关窗时窗口被移到这里**而不是释放**，
    /// 所以撤销能把**同一个窗口连同活会话**放回来。
    ///
    /// 放在 `UndoStack` 而不是 `Desktop`：两条关窗路径（用户的 `Ctrl+W` 与
    /// AI 的 `close_active` 工具）都拿得到 `UndoStack`，而它们**都必须**把窗口
    /// 交到这里 —— 否则会出现"能撤销却没有窗口可放回"的不一致。
    pub(crate) closed: Vec<draw::Win>,
}

impl UndoStack {
    pub(crate) fn new() -> Self {
        Self {
            steps: std::collections::VecDeque::new(),
            closed: Vec::new(),
        }
    }
    /// 收下一个刚关掉的窗口（超出容量时丢最旧的，返回被丢的那个）。
    pub(crate) fn keep_closed(&mut self, win: draw::Win) -> Option<draw::Win> {
        tray_push(&mut self.closed, win, CLOSED_CAP)
    }
    pub(crate) fn push(&mut self, step: UndoStep) {
        if self.steps.len() >= UNDO_CAP {
            self.steps.pop_front(); // 栈满后"最近 20 步"仍可撤销
        }
        self.steps.push_back(step);
    }
    pub(crate) fn pop(&mut self) -> Option<UndoStep> {
        self.steps.pop_back()
    }
    /// 栈深度（**仅测试与诊断**）。生产路径不需要它 —— 撤销只管"能不能弹出"。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.steps.len()
    }
}

/// 执行一次撤销。返回给用户的说明；`None` = 没有可撤销的动作。
pub(crate) fn undo_last(desktop: &mut Desktop, stack: &mut UndoStack) -> Option<ToastMsg> {
    match stack.pop()? {
        UndoStep::Layout { prev, floating } => {
            desktop.layout = prev;
            for (i, f) in floating.iter().enumerate() {
                if let Some(w) = desktop.wins.get_mut(i) {
                    w.floating = *f;
                    w.target = None;
                }
            }
            // 撤销后重新平铺：否则窗口还停在"改布局之后"的位置上，看起来没撤干净
            snap_now(desktop, prev);
            Some(ToastMsg::new(format!("已撤销：布局回到 {}", prev.label_in(crate::i18n::Lang::Zh)), format!("Undone: layout back to {}", prev.label_in(crate::i18n::Lang::En))))
        }
        UndoStep::OpenedWindow(idx) => {
            if idx < desktop.wins.len() {
                close_window(desktop, idx);
                Some(toast_msg("已撤销：关掉刚打开的窗口", "Undone: closed the window that was just opened"))
            } else {
                Some(toast_msg("已撤销：那个窗口已经不在了", "Undone: that window is no longer there"))
            }
        }
        UndoStep::ClosedWindow { at, title } => {
            // 只有"最近关闭"里**最新的那一个**才放得回去：`Win` 没有唯一 id，
            // 只能用标题判断。对不上说明这一步已被更晚的关闭顶掉（托盘满了）——
            // 那就如实说撤不了，**不把别的窗口放回去**。
            let matches = stack
                .closed
                .last()
                .map(|w| w.title == title)
                .unwrap_or(false);
            if !matches {
                return Some(toast_msg(
                "这一步无法撤销：该窗口已被更晚的关闭顶掉",
                "Cannot undo: that window was pushed out by a later close",
            ));
            }
            let w = stack.closed.pop().expect("刚判断过非空");
            let at = at.min(desktop.wins.len());
            desktop.wins.insert(at, w);
            // 插在中间会让原来的活动窗口下标失效 —— 与关窗同一处理
            desktop.active = at;
            snap_now(desktop, desktop.layout);
            Some(ToastMsg::new(format!("已撤销：重新打开「{title}」（会话仍在）"), format!("Undone: reopened \"{title}\" (session intact)")))
        }
    }
}

/// 应用来自 aetherd 的桌面行为（AI 操作桌面的落地端）。
pub(crate) fn apply_action(
    desktop: &mut Desktop,
    name: &str,
    args: &serde_json::Value,
    undo: &mut UndoStack,
) -> Option<ToastMsg> {
    // 每个分支在**改动之前**记逆操作（改完再记，记到的就是新状态）
    match name {
        "layout_set" => {
            let lay = match args.get("layout").and_then(|v| v.as_str())? {
                "two_col" => Layout::TwoCol,
                "three_col" => Layout::ThreeCol,
                "monocle" => Layout::Monocle,
                "float" => Layout::Float,
                _ => return None,
            };
            undo.push(UndoStep::Layout {
                prev: desktop.layout,
                floating: desktop.wins.iter().map(|w| w.floating).collect(),
            });
            desktop.layout = lay;
            for w in &mut desktop.wins {
                w.floating = false;
                w.target = None;
            }
            Some(ToastMsg::new(format!("布局：{}", lay.label_in(crate::i18n::Lang::Zh)), format!("Layout: {}", lay.label_in(crate::i18n::Lang::En))))
        }
        "open_app" => {
            let app = args.get("app").and_then(|v| v.as_str())?;
            let icon = APP_TITLES.iter().position(|t| *t == app)?;
            // open_app 返回 Some 时是拒绝/上限提示，不能谎报"已打开"（P3-3）
            let before = desktop.wins.len();
            let msg = open_app(desktop, icon);
            // **只在真的多了一个窗口时**才记：被拒绝/达上限时记了会变成
            // "撤销一次什么都没发生"，那种撤销比没有更让人困惑
            if msg.is_none() && desktop.wins.len() > before {
                undo.push(UndoStep::OpenedWindow(desktop.wins.len() - 1));
            }
            Some(msg.unwrap_or_else(|| ToastMsg::new(format!("已打开「{app}」"), format!("Opened \"{app}\""))))
        }
        "close_active" => {
            // 关窗**不可撤销**（窗口含活的终端会话，存不下来）——记标记，如实告知
            // §5.1：关窗**可撤销** —— 窗口进"最近关闭"托盘（连活会话）。
            if desktop.wins.len() > 1 {
                // 统一走 close_window：它有索引边界检查与 active 夹取，
                // 且不会像裸 pop 那样在"拖拽中"留下失效的 drag 索引（0.8 审计）
                let idx = desktop.active;
                match close_window(desktop, idx) {
                    Some((at, message, win)) => {
                        let title = win.title.clone();
                        let _ = undo.keep_closed(win);
                        undo.push(UndoStep::ClosedWindow { at, title });
                        Some(message)
                    }
                    None => None,
                }
            } else {
                Some(toast_msg("至少保留一个窗口", "At least one window must remain"))
            }
        }
        _ => None,
    }
}

/// 应用某个 Dock 图标 / 菜单项对应的应用窗口。
pub(crate) const APP_TITLES: [&str; 5] = [
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
pub(crate) fn desktop_icon_click(
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
pub(crate) fn apply_theme(renderer: &mut draw::Renderer, dark: bool, high_contrast: bool) {
    draw::theme::color::set_mode(if dark {
        draw::theme::color::Mode::Dark
    } else {
        draw::theme::color::Mode::Light
    });
    // 高对比度与主题模式**正交**：两种主题都能开（§3.4）。
    draw::theme::color::set_high_contrast(high_contrast);
    renderer.bg_dirty = true; // 壁纸与投影合成层都依赖主题，必须重建
}

/// 拼 HUD 文本（§5.10）。纯函数：**可单测**，且把"0 毫秒"这个边界钉住。
///
/// 0 毫秒是会真的出现的（第一帧、或计时精度不足）：直接做除法会得到 `inf`，
/// 界面上显示 "inf fps" 比不显示还糟 —— 所以显式判断并显示 0。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 调用点在 Linux 专属循环里
pub(crate) fn perf_hud_text(frame_ms: f32, rss: &str) -> String {
    let fps = if frame_ms > 0.0 { 1000.0 / frame_ms } else { 0.0 };
    format!("{frame_ms:.1} ms/帧（{fps:.0} fps） · 内存 {rss}")
}

/// 当前进程常驻内存（§5.10 HUD 用）。**拿不到就说拿不到** ——
/// 不显示一个猜测值：HUD 的价值全在"数字是真的"，编一个数字等于把工具废掉。
#[cfg(target_os = "linux")]
pub(crate) fn rss_label() -> String {
    // /proc/self/statm 第 2 个字段 = 常驻页数
    let Ok(s) = std::fs::read_to_string("/proc/self/statm") else {
        return "不可用（读不到 /proc/self/statm）".into();
    };
    let pages: u64 = match s.split_whitespace().nth(1).and_then(|v| v.parse().ok()) {
        Some(v) => v,
        None => return "不可用（statm 格式意外）".into(),
    };
    // 页大小固定 4096：不引入 libc 只为拿 sysconf，错了也只是显示偏差
    format!("{:.1} MB", pages as f64 * 4096.0 / 1024.0 / 1024.0)
}

#[cfg(not(target_os = "linux"))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn rss_label() -> String {
    "不可用（非 Linux：无 /proc）".into()
}

/// 息屏 / 亮屏（§3.6）：写内核的 fb blank 接口。
///
/// **不自己在帧缓冲上涂黑**：那只让"画面看起来黑了"，输出与背光照旧 ——
/// 在实机上等于没省电，而用户会以为系统卡死了。写 `blank` 才是真的关输出。
///
/// 失败**不静默**（本项目硬要求）：路径不存在（有些机器是 fb1，或用了别的显示后端）
/// 时打一行日志，让人知道"息屏没生效"，而不是以为设置坏了。
#[cfg(target_os = "linux")]
pub(crate) fn set_fb_blank(on: bool, blanked: &mut bool) {
    if *blanked == on {
        return; // 状态没变就不写：每帧写 sysfs 是无谓的系统调用
    }
    // fb0 覆盖绝大多数单屏机器；找不到就退到 fb1/fb2，都不行则如实报错
    const PATHS: [&str; 3] = [
        "/sys/class/graphics/fb0/blank",
        "/sys/class/graphics/fb1/blank",
        "/sys/class/graphics/fb2/blank",
    ];
    // 4 = FB_BLANK_POWERDOWN（真关输出），0 = FB_BLANK_UNBLANK
    let val = if on { "4" } else { "0" };
    for path in PATHS {
        if std::path::Path::new(path).exists() {
            match std::fs::write(path, val) {
                Ok(()) => {
                    *blanked = on;
                    return;
                }
                Err(e) => {
                    eprintln!("aether-compositor: 息屏写入 {path} 失败: {e}");
                    return;
                }
            }
        }
    }
    eprintln!("aether-compositor: 未找到 /sys/class/graphics/fb*/blank —— 息屏在当前显示后端不生效");
}

/// 非 Linux（开发机预览）：没有 fb 设备，空实现。
#[cfg(not(target_os = "linux"))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn set_fb_blank(_on: bool, _blanked: &mut bool) {}

/// 打开通知中心（§3.2）：**复用文本预览窗口**，不新造 WinKind。
///
/// 为什么不新造一种窗口类型：多一种就要多一套绘制、滚动、命中处理，
/// 而通知列表本来就是"一段只读文本 + 时间戳"—— 预览窗口已经把这套做完了
/// （含长列表滚动）。把它命名为"通知"是**用户可见**的部分，内部复用是实现细节。
///
/// 打开即**清未读**（与所有系统的通知中心一致）：用户既然点开看了，就不该继续标未读。
/// 历史本身**不清**（清空是设置里的另一个动作）。
pub(crate) fn open_notifications(
    desktop: &mut Desktop,
    notifs: &mut notifications::Notifications,
    tz_min: i32,
) -> Option<ToastMsg> {
    let lines: Vec<String> = notifs
        .iter()
        .map(|n| {
            // 时间用用户当前时区显示（与顶栏同一份设置，不另算一套）
            let at = crate::text::clock_fmt(n.at, tz_min, true, false);
            let body = if n.repeat > 1 {
                format!("{}（×{}）", n.msg.text(), n.repeat)
            } else {
                n.msg.text().to_string()
            };
            format!("{} {} {}", at, n.kind.mark(), body)
        })
        .collect();
    let empty = notifs.is_empty();
    notifs.mark_all_read();
    let data = draw::PreviewData {
        // 标题里带总数：用户一眼知道"历史攒了多少"
        path: if crate::i18n::is_en() {
                format!("Notifications ({} total)", notifs.len())
            } else {
                format!("通知（共 {} 条）", notifs.len())
            },
        lines,
        // 空历史显示一句说明，而不是一片空白（让人以为界面坏了）。
        // **两种空要分开说**：从没记过 vs 用户自己关了记录 —— 后者如果只说"还没有通知"，
        // 用户会以为是功能坏了，而其实是他自己关的。
        error: if empty {
            // §3.5：这两句是**界面**说的（通知中心的空态），所以在这里按语言取好，
            // 再交给 `PreviewError::Message` 带着走
            Some(if notifs.enabled() {
                draw::PreviewError::Message(
                    crate::i18n::pick(
                        crate::i18n::lang(),
                        "还没有通知",
                        "No notifications yet",
                    )
                    .to_string(),
                )
            } else {
                draw::PreviewError::Message(
                    crate::i18n::pick(
                        crate::i18n::lang(),
                        "通知记录已关闭（可在 设置 → 通知 打开）",
                        "Notification history is off (enable it in Settings → Notifications)",
                    )
                    .to_string(),
                )
            })
        } else {
            None
        },
        truncated: false,
        scroll: Default::default(),
    };
    // 已经开着通知窗口 → 就地刷新内容（不叠第二个"通知"窗口）
    if let Some(w) = desktop
        .wins
        .iter_mut()
        .find(|w| w.kind == draw::WinKind::Preview && w.title == "通知")
    {
        w.preview = Some(data);
        return None;
    }
    if desktop.wins.len() >= 8 {
        return Some(toast_msg("窗口数量已达上限（8）", "Window limit reached (8)"));
    }
    desktop.wins.push(draw::Win {
        rect: Rect { x: 300, y: 120, w: 560, h: 420 },
        target: None,
        title: "通知".to_string(),
        kind: draw::WinKind::Preview,
        floating: true,
        restore: None,
        preview: Some(data),
        term: None,
        wayland: None,
    });
    None
}

/// 导出审计日志（§5.2）：写到审计日志同目录，返回**确切路径**。
///
/// 与审计日志同目录而不是 `/tmp`：Live ISO 的 `/tmp` 是内存盘，
/// 重启即失，而"导出给别人看"通常要跨越一次重启。
///
/// 非 Linux 目标**如实报错**而不是悄悄写到某个 Windows 路径 ——
/// 预览机上写出来的文件对用户没有任何意义，还可能在他机器上留下垃圾。
#[cfg(target_os = "linux")]
pub(crate) fn export_audit() -> Result<String, String> {
    let now = crate::text::now_utc_secs().max(0) as u64;
    let info = crate::audit::read(crate::audit::AUDIT_PATH, 200);
    // §3.5：导出也要跟着界面语言 —— 否则会出现「页面英文、导出中文」的错位
    let text = crate::audit::export_text(&info, now, crate::i18n::is_en());
    let dir = std::path::Path::new(crate::audit::AUDIT_PATH)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/var/log/aether"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建 {}：{e}", dir.display()))?;
    let path = dir.join(format!("audit-export-{now}.txt"));
    std::fs::write(&path, text).map_err(|e| format!("写入失败：{e}"))?;
    Ok(path.display().to_string())
}

#[cfg(not(target_os = "linux"))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn export_audit() -> Result<String, String> {
    Err("导出仅在目标系统上可用（预览机没有审计日志目录）".into())
}

/// 设置一条 toast，**同时记入通知历史**（§3.2）。
///
/// 为什么不直接在 47 个产生点各写两行：那样迟早会有人只改一处（新加的提示不进历史），
/// 而"历史不全"比"没有历史"更糟 —— 用户会以为没发生过。统一走这个函数，
/// 新增提示只要用它就自动进历史。
///
/// `kind` 由调用点给：历史里失败必须与成功**可区分**（标记 + 颜色），
/// 而"这条是不是失败"只有产生点知道，不该在这里猜文本。
pub(crate) fn push_toast(
    toast: &mut Option<(ToastMsg, Instant)>,
    notifs: &mut notifications::Notifications,
    msg: ToastMsg,
    kind: notifications::Kind,
) {
    notifs.push(msg.clone(), kind, crate::text::now_utc_secs());
    *toast = Some((msg, Instant::now()));
}

/// 同 `crate::i18n::tr`，但**带一个占位符**（模板里写 `{}`）。
///
/// **注意：参数是按值收的** —— 传 `String` 字段会把字段移动走。
/// 若后面还要用它（或它来自 `&[T]` 切片），调用点要 `clone()`。
/// （这个坑只在 Linux 目标上编译才会暴露：`format!` 是按引用取参，而本助手不是 —— musl 检查抓到过。）
///
/// 存在的理由：`format!` 的模板必须是**字面量**，所以不能把模板当变量传 ——
/// 只能按语言各写一次。这个助手让两种语言的模板**并排在调用点**上，
/// 改一处不会漏另一处（代价是多一次 String 分配，可忽略）。
pub(crate) fn pick_zh_en_1(zh: &'static str, en: &'static str, arg: impl std::fmt::Display) -> String {
    crate::i18n::pick(crate::i18n::lang(), zh, en).replacen("{}", &arg.to_string(), 1)
}

/// 通知正文的构造助手（FEATURE-GAPS §3.5 遗留项 #43）。
///
/// 存在的理由与上面的 `pick_zh_en*` 一样（两种语言并排写在调用点上），
/// 区别是**返回结构化类型** `ToastMsg`：通知要被记进历史、还可能中途切语言，
/// 所以不能当场渲染成 `String`。
///
/// 为什么不直接把 `push_toast` 的参数写成 `impl Into<String>` 了事：
/// 那样"忘了英文"在编译期仍然看不见 —— 实测 117 个调用点里就有 5 处只有中文。
pub(crate) fn toast_msg(zh: &'static str, en: &'static str) -> ToastMsg {
    ToastMsg::new(zh, en)
}

/// 一个占位符。见 [`toast_msg`]。
pub(crate) fn toast_msg_1(zh: &'static str, en: &'static str, a: impl std::fmt::Display) -> ToastMsg {
    ToastMsg::new_1(zh, en, a)
}

/// 由"按语言生成文案"的函数造一条双语正文（`f` 被调用两次：中、英）。
///
/// 本仓库有一批「`fn xxx_text(lang, …) -> String`」形态的文案函数
/// （`form_problem_text`、`dns_error_text`、`preview_error_text` …），
/// 它们只认一种语言。用它就能把这些函数的产出**一次凑齐两种语言**，
/// 而不必在每个调用点把中文文案抄第二遍。
pub(crate) fn toast_msg_lang(f: impl Fn(crate::i18n::Lang) -> String) -> ToastMsg {
    ToastMsg::new(f(crate::i18n::Lang::Zh), f(crate::i18n::Lang::En))
}

/// 一个输入事件该交给谁 —— **守卫顺序的唯一真相**（§4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Linux 事件循环（fbdev_app）使用；Windows 构建只有测试引用。
pub enum KeyRoute {
    /// 锁屏：**最高优先级**，吞掉一切输入（这是"锁屏"的定义）。
    Lock,
    /// 首次启动向导（模态，高于命令面板与 DNS）。
    Setup,
    /// 命令面板（模态）。
    Palette,
    /// DNS 输入框（设置页里正在编辑）。
    Dns,
    /// 终端（终端有焦点时按键归 shell，否则 Ctrl+C 会被当成关窗）。
    Terminal,
    /// 其余：全局快捷键、窗口操作、AI 条。
    Global,
}

/// 决定一个输入事件交给谁。
///
/// **顺序本身就是安全决策**，不是随手排的：
///
/// 1. **锁屏在最前** —— 它必须排在终端之前。原来终端守卫在最上面，
///    结果是"锁着屏但终端有焦点"时按键进到背后的 shell：既是**锁屏旁路**
///    （锁屏状态下能执行命令），又让用户按不出解锁密码（见安全审计附录 K）；
/// 2. 向导与面板是**模态**：开着的时候别的浮层不该抢键；
/// 3. DNS 是**编辑态**：正在输入框里打字时按键不该跑到别处；
/// 4. 终端只在前四者都不成立时接手 —— 但**只要没有模态弹窗**（`modal_open` 为假）
///    它就接手，因为终端有焦点时 Ctrl+C 等该归 shell。
#[allow(dead_code)] // 同上：Windows 构建没有 fbdev 事件循环调用点。
pub fn key_route(
    locked: bool,
    setup_open: bool,
    palette_open: bool,
    dns_editing: bool,
    modal_open: bool,
    terminal_focused: bool,
) -> KeyRoute {
    if locked {
        return KeyRoute::Lock;
    }
    if setup_open {
        return KeyRoute::Setup;
    }
    if palette_open {
        return KeyRoute::Palette;
    }
    if dns_editing {
        return KeyRoute::Dns;
    }
    if terminal_focused && !modal_open {
        return KeyRoute::Terminal;
    }
    KeyRoute::Global
}

/// 执行控制中心交回来的意图（**有副作用的部分**）。
///
/// 与 `control_center_click` 分开的理由见 `UiIntent`：判定要能在单测里跑，
/// 而"发 IPC / 退出进程"不能。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn apply_ui_intent(
    intent: UiIntent,
    ai_tx: &mpsc::Sender<AiEvent>,
    toast: &mut Option<(ToastMsg, Instant)>,
    renderer: &mut draw::Renderer,
    audio: &mut audio::Audio,
) {
    match intent {
        // 这两个意图**在调用点处理**（见那里的注释）：它们要动的是循环里的
        // `desktop`/`spaces`/`drag`/`snap_zone`，本函数一个都拿不到。
        // 这里显式列出并忽略 —— 不是"忘了处理"，而是**明确说明它们不属于这里**；
        // 若将来有人直接调用本函数传入它们，编译器不会拦，但代码里写着它不做。
        UiIntent::AddSpace | UiIntent::RemoveSpace => {
            debug_assert!(false, "这两个意图应当在调用点处理（见 fbdev 循环里的注释）");
        }
        UiIntent::Power(action) => {
            let a = action.as_str();
            // label 同时给日志与通知用：日志是中文（串口/本机诊断），通知要双语
            let (label, label_en) = if action == aether_ipc::PowerAction::PowerOff {
                ("关机", "Shut down")
            } else {
                ("重启", "Restart")
            };
            // 关掉控制中心：确认卡片要盖在最上层，面板留着会挡住它（也会让人以为没反应）
            renderer.control_open = false;
            // 中英的**占位内容不同**（动作名各用各的语言），所以这里不能用 `toast_msg_2`
            // 的"同一批参数填两个模板" —— 直接构造两条完整文案。
            push_toast(
                toast,
                &mut renderer.notifs,
                ToastMsg::new(
                    format!("{label}：请在确认卡片里输入 {a} 以确认"),
                    format!("{label_en}: type {a} in the confirmation card to confirm"),
                ),
                notifications::Kind::Info,
            );
            tty_log(&format!("控制中心电源入口 → {label}（等待确认卡片）"));
            spawn_power_tool(action, ai_tx);
        }
        UiIntent::SetVolume(pct) => {
            // 写真实 mixer；**失败必须如实提示**（不假装"音量已调"）
            match audio.set_volume(pct) {
                Ok(()) => {
                    push_toast(
                        toast,
                        &mut renderer.notifs,
                        toast_msg_1("音量 {}%", "Volume {}%", audio.volume_pct()),
                        notifications::Kind::Ok,
                    );
                }
                Err(e) => {
                    push_toast(
                        toast,
                        &mut renderer.notifs,
                        // 错误文本来自 alsa 子进程（外部），两种语言同一份
                        ToastMsg::from_external(format!("音量未改变：{e}")),
                        notifications::Kind::Err,
                    );
                }
            }
        }
        UiIntent::Logout => {
            // 注销 = 重启合成器：**不经过 IPC**（这是合成器自己的生命周期，
            // 不需要整机权限）。aether-init 按 `compositor.json` 的 `restart: true`
            // 会把它拉起来，所以用户看到的是"桌面重新出现"。
            //
            // 诚实边界：当前**没有账户体系**（见 FEATURE-GAPS §2.3），所以这不是
            // "切换用户"，而是"把桌面重启一遍" —— 窗口会没，但不会有任何隐私边界。
            tty_log("注销：退出合成器，由 aether-init 重新拉起（当前无账户体系）");
            std::process::exit(0);
        }
    }
}

/// 在后台线程里发起 `system_power` 工具调用。
///
/// 独立线程 + 超时（与 `control_service` 同一套路）：IPC 往返绝不阻塞渲染。
/// `approval` 传 None —— L3 会被闸门拦下并回 `NeedsConfirmation`，
/// 主循环据此弹出确认卡片；用户输入回显后带令牌重发才会真正执行。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn spawn_power_tool(action: aether_ipc::PowerAction, tx: &mpsc::Sender<AiEvent>) {
    let a = action.as_str();
    // confirm 与 action 必须逐字相同：这是 aetherd 侧的回显校验（防误点）
    let args = serde_json::json!({ "action": a, "confirm": a });
    let tx = tx.clone();
    std::thread::spawn(move || {
        if let Err(e) = send_tool_call("system_power".into(), args, None, ConfirmOrigin::Power, &tx) {
            let _ = tx.send(AiEvent::ToolDone {
                ok: false,
                output: format!("电源请求未能发出：{e}"),
                origin: ConfirmOrigin::Power,
            });
        }
    });
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
pub(crate) fn control_center_click(
    renderer: &mut draw::Renderer,
    desktop: &mut Desktop,
    ime: &mut ime::Ime,
    mx: f32,
    my: f32,
    pressed: bool,
    intent: &mut Option<UiIntent>,
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
    if let Some((rect, hit)) = renderer
        .control_hits
        .iter()
        .find(|(r, _)| r.contains(mx, my))
        .map(|(r, h)| (*r, *h))
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
                // 先取值，避免同时可变借用 renderer
                let dark = renderer.settings.dark_mode;
                let hc = renderer.settings.high_contrast;
                apply_theme(renderer, dark, hc);
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
            ControlHit::Notifications => {
                // 通知历史在 renderer 上、桌面在 desktop 上：两个不相交的可变借用
                let tz = renderer.settings.tz_offset_min;
                let _ = open_notifications(desktop, &mut renderer.notifs, tz);
            }
            // §5.4 虚拟桌面的增删同属"判定与执行分开"：这里只登记意图
            //（执行要碰 `spaces` 与窗口数组，那些住在事件循环里）。
            ControlHit::AddSpace => {
                *intent = Some(UiIntent::AddSpace);
            }
            ControlHit::RemoveSpace => {
                *intent = Some(UiIntent::RemoveSpace);
            }
            // 电源三个入口**只判定意图**，执行交给主循环（理由见 UiIntent 的说明）：
            // 单测里点一下"关机"绝不该真的去发 IPC 或退出进程。
            ControlHit::PowerOff => {
                *intent = Some(UiIntent::Power(aether_ipc::PowerAction::PowerOff));
            }
            ControlHit::Reboot => {
                *intent = Some(UiIntent::Power(aether_ipc::PowerAction::Reboot));
            }
            ControlHit::Logout => {
                *intent = Some(UiIntent::Logout);
            }
            // 音量轨道：**点击位置**决定音量（0..=100）。
            // 用比例而不是"每次 ±5"：点击是绝对定位操作，用户点哪儿就该是哪儿。
            ControlHit::VolumeTrack => {
                let pct = ((mx - rect.x as f32) / rect.w as f32 * 100.0).round();
                *intent = Some(UiIntent::SetVolume(audio::clamp_volume(pct as i32)));
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
pub(crate) const MINIMIZED_X: i32 = -10_000;

/// 判断窗口是否处于最小化（屏外）。
pub(crate) fn is_minimized(win: &Win) -> bool {
    win.rect.x <= MINIMIZED_X
}

/// 最小化：移出屏外 + 脱离平铺（`floating`），并记住原矩形以便恢复。
pub(crate) fn minimize_window(desktop: &mut Desktop, idx: usize) -> Option<ToastMsg> {
    let win = desktop.wins.get_mut(idx)?;
    if is_minimized(win) {
        return None;
    }
    win.restore = Some(win.rect);
    win.rect = Rect { x: MINIMIZED_X, y: win.rect.y, w: win.rect.w, h: win.rect.h };
    win.target = None;
    win.floating = true; // 脱离平铺，否则下一帧布局又把它拉回屏幕
    Some(ToastMsg::new(format!("{} 已最小化（点 Dock 图标恢复）", win.title), format!("\"{}\" minimized (click its Dock icon to restore)", win.title)))
}

/// 若窗口已最小化则恢复（Dock 图标点击走这里）。返回 `true` = 确实恢复了。
///
/// 调用点在 Linux 的 fbdev 循环里；预览路径尚未接最小化（它只有走查用途）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn restore_if_minimized(desktop: &mut Desktop, idx: usize) -> bool {
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

pub(crate) fn open_app(desktop: &mut Desktop, icon: usize) -> Option<ToastMsg> {
    if icon >= APP_TITLES.len() {
        return Some(toast_msg("未知应用", "Unknown app"));
    }
    if desktop.wins.len() >= 8 {
        return Some(toast_msg("窗口数量已达上限（8）", "Window limit reached (8)"));
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
        rect: Rect { x: 200 + (idx % 4) as i32 * 40, y: 90 + (idx % 3) as i32 * 30, w: draw::theme::size::px(500), h: draw::theme::size::px(380) },
        target: None,
        // 标题走 i18n（英文实拍：设置窗口标题栏仍是「设置」，附录 DN）。
        // 注意：APP_TITLES **列表本身不能翻译** —— 3311 行用它反查图标索引（`*t == app`）。
        // 翻译只发生在显示这一步。
        title: text::strings::win_label(APP_TITLES[icon]).to_string(),
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
pub(crate) fn apps_root() -> std::path::PathBuf {
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
pub(crate) fn load_installed_apps(root: &std::path::Path) -> Vec<(String, String)> {
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
pub(crate) fn open_installed_app(desktop: &mut Desktop, id: &str, name: &str) -> Option<ToastMsg> {
    if desktop.wins.len() >= 8 {
        return Some(toast_msg("窗口数量已达上限（8）", "Window limit reached (8)"));
    }
    let idx = desktop.wins.len();
    let mut term = term::Terminal::spawn(80, 24, None);
    // 把 `exec <id>` 写进 PTY：shell 起来后立刻执行。
    // 早写不会丢 —— 行规程会缓冲，shell 读到就读（真实终端也是这个行为）。
    term.write(format!("exec {id}\r").as_bytes());
    desktop.wins.push(Win {
        rect: Rect { x: 200 + (idx % 4) as i32 * 40, y: 90 + (idx % 3) as i32 * 30, w: draw::theme::size::px(500), h: draw::theme::size::px(380) },
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
    Some(ToastMsg::new(format!("启动应用：{name}"), format!("Launching app: {name}")))
}

/// 每 ~3 秒重扫一次已装应用。
///
/// 为什么不每帧扫：`read_dir` + 几个小文件读，每帧做是浪费（60fps 下就是每秒 60 次）。
/// 为什么不只在启动时扫：装应用最常见的路径是"跟 AI 说一句"，那时合成器已经在跑了，
/// 不重扫就得重启才能看见 —— 那不像一个能用的系统。
pub(crate) fn refresh_apps_throttled(renderer: &mut draw::Renderer, last: &mut Instant) {
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
pub(crate) enum DockAction<'a> {
    /// 内建应用（文件/终端/浏览器/音乐/设置），索引进 `APP_TITLES`
    Builtin(usize),
    /// 已装应用：`(id, 显示名)`
    Installed(&'a str, &'a str),
    /// 安装向导（仅 Live ISO 显示）
    Installer,
    /// 空处：安装向导没显示，或者越界
    Nothing,
}

pub(crate) fn dock_action<'a>(
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
pub(crate) fn go_up(desktop: &mut Desktop) -> Option<ToastMsg> {
    let up = draw::parent_of(&desktop.cwd);
    if up == desktop.cwd {
        return Some(toast_msg("已在最上层目录", "Already at the top level"));
    }
    navigate_to(desktop, &up, "上级目录", "Parent folder")
}

/// 导航到指定目录（侧栏、面包屑、上级都走这一条路径）。
///
/// 只有一处实现，才不会出现"从侧栏进去没重置选中项、从面包屑进去重置了"这类差异。
pub(crate) fn navigate_to(desktop: &mut Desktop, path: &str, label_zh: &str, label_en: &str) -> Option<ToastMsg> {
    let (entries, err) = draw::read_dir_entries(path);
    if let Some(e) = err {
        // 保留原目录与旧条目：导航失败不该把用户丢到一个读不了的目录里
        desktop.dir_error = Some(format!("无法打开「{label_zh}」：{e}"));
        return Some(ToastMsg::new(
            format!("无法打开「{label_zh}」"),
            format!("Cannot open \"{label_en}\""),
        ));
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
pub(crate) fn sidebar_targets() -> Vec<(String, String)> {
    let home = draw::default_cwd();
    // 走 i18n（英文实拍发现同一栏里 "Parent folder" 是英文、「主目录」是中文，审计附录 DL/DM）。
    let mut out = vec![(
        crate::i18n::pick(crate::i18n::lang(), "主目录", "Home").to_string(),
        home.clone(),
    )];
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
pub(crate) fn sync_files_title(desktop: &mut Desktop) {
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
pub(crate) fn open_entry(desktop: &mut Desktop, idx: usize) -> Option<ToastMsg> {
    let ent = desktop.entries.get(idx).cloned()?;
    if ent.is_dir {
        let next = draw::join_path(&desktop.cwd, &ent.name);
        let (entries, err) = draw::read_dir_entries(&next);
        if let Some(e) = err {
            // 进不去就留在原地并说明原因，而不是进一个空目录让人猜
            desktop.dir_error = Some(format!("无法进入 {}：{e}", ent.name));
            return Some(ToastMsg::new(
                format!("无法进入「{}」", ent.name),
                format!("Cannot enter \"{}\"", ent.name),
            ));
        }
        desktop.cwd = next;
        desktop.entries = entries;
        desktop.selected = None;
        desktop.dir_error = None;
        sync_files_title(desktop);
        None
    } else {
        if desktop.wins.len() >= 8 {
            return Some(toast_msg("窗口数量已达上限（8）", "Window limit reached (8)"));
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
            Some(ToastMsg::new(format!("「{}」无法以文本预览", ent.name), format!("\"{}\" cannot be previewed as text", ent.name)))
        } else {
            None
        }
    }
}

/// 执行菜单项；返回 (toast, reply-bubble) 反馈。
pub(crate) fn run_menu_item(desktop: &mut Desktop, menu: usize, item: usize) -> (Option<ToastMsg>, Option<String>) {
    match (menu, item) {
        (0, 0) => {
            let msg = open_app(desktop, 0);
            (msg.or(Some(toast_msg("已新建窗口", "New window created"))), None)
        }
        (0, 1) => {
            if desktop.wins.len() > 1 {
                // 同 close_active：统一走 close_window，避免裸 pop 留下失效 drag（0.8 审计）
                let idx = desktop.active;
                // 脚本化路径没有撤销栈：这里**确实**释放窗口（不是"忘了"）——
                // 它跑的是 e2e 动作，没有 Ctrl+Z 可用。
                (close_window(desktop, idx).map(|(_, m, _)| m), None)
            } else {
                (Some(toast_msg("至少保留一个窗口", "At least one window must remain")), None)
            }
        }
        (0, 2) => (Some(toast_msg("预览版请用 Esc 退出", "Preview build: press Esc to quit")), None),
        (1, _) => (Some(toast_msg("编辑操作将在应用内生效（M2）", "Editing takes effect in-app (M2)")), None),
        (2, 0) => (set_layout(desktop, Layout::TwoCol), None),
        (2, 1) => (set_layout(desktop, Layout::ThreeCol), None),
        (2, 2) => (set_layout(desktop, Layout::Monocle), None),
        (2, 3) => (set_layout(desktop, Layout::Float), None),
        (3, 0) => (None, Some("AetherOS 0.1.0 预览版 · Linux 内核 + 全自研用户态 + AI 中枢".into())),
        _ => (None, None),
    }
}

/// 指针位置更新（与"事件交给谁"无关的**纯状态**）。
///
/// 为什么单独抽出来：主循环里 KeyRoute::Lock / KeyRoute::Setup 这类分支会 continue 掉事件 ——
/// 位置更新原本只在下面的 match ev 里做，于是**向导或锁屏开着时光标完全不动**
/// （2026-10-17 实测：首启向导吞掉全部输入，鼠标键盘都像失灵，桌面却照常显示）。
/// 这些分支必须也调用它。注意只在 continue 的分支里调用，避免与 match 里的更新重复（否则速度翻倍）。
// 与 input.rs 同一约定：该函数只在 Linux 目标的主循环里被调用，
// Windows 上编译测试目标时它没有调用点 —— 用 allow 而不是 #[cfg] 保持定义可见。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn apply_pointer(ev: &input::UiEvent, mouse: &mut (f32, f32), spd: f32, w: usize, h: usize) {
    match ev {
        input::UiEvent::MouseMove { dx, dy } => {
            mouse.0 = (mouse.0 + *dx as f32 * spd).clamp(0.0, (w - 1) as f32);
            mouse.1 = (mouse.1 + *dy as f32 * spd).clamp(0.0, (h - 1) as f32);
        }
        input::UiEvent::MouseAbs { x, y, min_x, max_x, min_y, max_y } => {
            let (sx, sy) = ((*max_x - *min_x) as f32, (*max_y - *min_y) as f32);
            if sx > 0.0 && sy > 0.0 {
                mouse.0 = ((*x - *min_x) as f32 / sx * (w - 1) as f32).clamp(0.0, (w - 1) as f32);
                mouse.1 = ((*y - *min_y) as f32 / sy * (h - 1) as f32).clamp(0.0, (h - 1) as f32);
            }
        }
        _ => {}
    }
}
