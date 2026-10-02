//! aetherd serve：常驻 IPC 服务（TCP + aether-ipc NDJSON 协议）。
//!
//! 每连接一线程；Chat 请求先走离线快速意图，未命中再进 LLM agent；
//! 一个 Chat 请求可产生多条响应（ChatChunk + Action）。

use crate::perm::verdict;
use crate::{intent, perm::{Approvals, Gate, Level}, tools, Config};
use aether_ipc::{encode, Request, Response, SysReport};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 单条请求行的长度上限（NDJSON 帧协议，正常请求远小于此值）。
const MAX_LINE_BYTES: u64 = 1024 * 1024;
/// 同时服务的连接数上限：超出后立即拒绝，防资源耗尽。
const MAX_CONNECTIONS: usize = 32;
/// 单连接空闲读超时：半开连接不永久占用线程。
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// aetherd 的 Unix socket 路径（合成器与 e2e 脚本约定一致）。
///
/// **默认通道**：socket 由文件权限兜底（`/run` 属 root，socket 0600），因此
/// "对端是不是 aetherd"由内核保证 —— 而 TCP 上客户端**无法验证对端**，谁抢到
/// 7311 谁就能收到 `RegisterUi` 里的 UI 密钥（2026-10-02 审计 M-17）。
/// TCP 仍然保留（开发机预览与 hostfwd 调试），但客户端默认不再用它注册。
#[cfg(unix)]
const UNIX_SOCKET_PATH: &str = "/run/aetherd.sock";

/// UI 通道密钥的落盘位置。///
/// **为什么从 `/var/log/aether/` 迁到 `/run/aether/`**（2026-10-02 审计 H-1）：
/// 旧位置正好在 `read_file` 的读取白名单里，于是"任意本机进程 → ToolCall(read_file)
/// → 取走密钥 → 注册为 UI 通道 → 自我确认 L3"是一条完整链路（已实测复现）。
/// `/run/aether` 同样是白名单根，所以真正的拦截靠 `tools::READ_DENY_SUBPATHS`
/// 里的 `ui.key`；迁址是第二道防线，同时让"密钥"与"日志"在语义上分开
/// （审计日志目录是给人看的，密钥不是）。
/// `/run` 是 ramfs：密钥每次开机重新生成，不再跨重启残留。
const UI_KEY_PATH: &str = "/run/aether/ui.key";

/// 两个监听器共享的连接处理状态。
struct Shared {
    cfg: Arc<Config>,
    gate: Arc<Gate>,
    approvals: Arc<Approvals>,
    ui_key: Arc<String>,
    active: Arc<std::sync::atomic::AtomicUsize>,
}

impl Shared {
    /// 在独立线程里处理一条已建立的连接（含连接数上限与活跃计数）。
    ///
    /// 泛型化是为了让 TCP 与 Unix socket 走**同一段**请求处理逻辑 —— 两处各写一份
    /// 必然会在某次修改后走偏（这个项目已经吃过"逐入口手写权限检查"的亏）。
    fn spawn_conn<R, W>(&self, reader: R, writer: W)
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        if self.active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CONNECTIONS {
            eprintln!("[aetherd] 连接数已达上限（{MAX_CONNECTIONS}），拒绝新连接");
            return;
        }
        let cfg = self.cfg.clone();
        let gate = self.gate.clone();
        let approvals = self.approvals.clone();
        let ui_key = self.ui_key.clone();
        let active = self.active.clone();
        active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::thread::spawn(move || {
            let _guard = ActiveGuard(&active);
            if let Err(e) = handle_conn(reader, writer, &cfg, &gate, &approvals, ui_key.as_str()) {
                eprintln!("[aetherd] 连接处理结束: {e}");
            }
        });
    }
}

/// 建 Unix socket 监听（0600）并在后台线程里接受连接。
///
/// 失败**不算致命**：TCP 通道仍在，只是客户端默认不会用它注册。所以这里只告警，
/// 由 `serve` 决定怎么报。
#[cfg(unix)]
fn spawn_unix_listener(shared: Arc<Shared>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    if let Some(parent) = PathBuf::from(UNIX_SOCKET_PATH).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 上次异常退出留下的 socket 文件必须先删，否则 bind 报 EADDRINUSE
    let _ = std::fs::remove_file(UNIX_SOCKET_PATH);
    let listener = UnixListener::bind(UNIX_SOCKET_PATH)?;
    // 0600：只有 root 能连 —— "对端身份"就是由这一行保证的
    std::fs::set_permissions(UNIX_SOCKET_PATH, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("[aetherd] IPC 服务（Unix socket，0600）: {UNIX_SOCKET_PATH}");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            stream.set_read_timeout(Some(IDLE_TIMEOUT)).ok();
            match stream.try_clone() {
                Ok(reader) => shared.spawn_conn(reader, stream),
                Err(e) => eprintln!("[aetherd] Unix 连接克隆失败: {e}"),
            }
        }
    });
    Ok(())
}

pub fn serve(cfg: Config) -> anyhow::Result<()> {
    // 只听回环；宿主调试需要 hostfwd 直连时，由调试脚本显式注入 AETHER_BIND=0.0.0.0，
    // 出厂镜像（platform/overlay/init）不设置该变量
    let bind = std::env::var("AETHER_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let listener = TcpListener::bind((bind.as_str(), aether_ipc::DEFAULT_PORT))?;
    eprintln!(
        "[aetherd] IPC 服务已启动: {bind}:{} · 本地: {} · 云端: {}",
        aether_ipc::DEFAULT_PORT,
        cfg.local.base_url,
        // 显示**实际生效**的端点（而不是"配没配"）—— 否则"配置到底有没有生效"
        // 只能靠猜，0.6 的向导改完配置后无法自证
        cfg.cloud.as_ref().map(|c| c.base_url.as_str()).unwrap_or("(未配置)")
    );
    let cfg = Arc::new(cfg);
    let shared = Arc::new(Shared {
        cfg,
        gate: Arc::new(Gate::new(PathBuf::from("/var/log/aether/aether-audit.log"))),
        approvals: Arc::new(Approvals::new()),
        ui_key: Arc::new(resolve_ui_key()),
        active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });

    // Unix socket 优先通道（0600）。建不起来只告警：TCP 还在，服务不该因此停摆。
    #[cfg(unix)]
    if let Err(e) = spawn_unix_listener(shared.clone()) {
        eprintln!("[aetherd] 警告：Unix socket 建立失败（{e}）—— 只剩 TCP 通道，客户端会拒绝经它注册");
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        stream.set_read_timeout(Some(IDLE_TIMEOUT)).ok();
        match stream.try_clone() {
            Ok(reader) => shared.spawn_conn(reader, stream),
            Err(e) => eprintln!("[aetherd] 连接克隆失败: {e}"),
        }
    }
    Ok(())
}

/// 解析 UI 通道密钥（P1-8）。
///
/// 优先级：`AETHER_UI_KEY` 环境变量 > `/run/aether/ui.key` > 生成并落盘。
/// 生成时在 Unix 下收紧为 0600。
///
/// 诚实边界：这挡不住**能读到该文件**的本机进程（例如 root 起的终端）。
/// 但它把"谁能兑现确认令牌"从"任何能连 7311 的进程"收窄到"能读到密钥的进程"，
/// 并让"确认方"成为服务端可以断言的事实。
///
/// 2026-10-02 审计（H-1）修正了一个被推翻的前提：原实现假设"读 0600 的 root 文件
/// 需要特权"，但本进程自己的 `read_file` 工具（L0、免确认、对未注册连接开放）
/// 正好能代读它 —— 于是攻击面没有收窄。现在 `tools::READ_DENY_SUBPATHS` 明确
/// 拒绝 `ui.key`，密钥本身也移出了日志目录。
fn resolve_ui_key() -> String {
    if let Ok(k) = std::env::var("AETHER_UI_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            eprintln!("[aetherd] UI 通道密钥来自 AETHER_UI_KEY");
            return k;
        }
    }
    let path = PathBuf::from(UI_KEY_PATH);
    if let Ok(k) = std::fs::read_to_string(&path) {
        let k = k.trim().to_string();
        if !k.is_empty() {
            eprintln!("[aetherd] UI 通道密钥来自 {}", path.display());
            return k;
        }
    }
    let key = crate::perm::random_token();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &key) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            eprintln!("[aetherd] 已生成 UI 通道密钥 → {}（0600）", path.display());
        }
        Err(e) => eprintln!("[aetherd] UI 密钥落盘失败（{e}）：本次会话使用内存密钥"),
    }
    key
}

/// 线程退出时递减活跃连接计数。
struct ActiveGuard<'a>(&'a Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 处理一条连接（TCP 与 Unix socket 共用）。
///
/// `reader`/`writer` 是同一连接的两个句柄：读端要 `take()` 限长，写端要逐条 flush，
/// 用一对句柄而不是一个 `TcpStream`，是为了两种传输走同一段逻辑。
/// 读超时由调用方在底层 stream 上设置（两种 stream 的设置方法一致，但不是同一类型）。
fn handle_conn<R: Read, W: Write>(
    reader: R,
    writer: W,
    cfg: &Config,
    gate: &Gate,
    approvals: &Approvals,
    ui_key: &str,
) -> anyhow::Result<()> {
    // 注意：`take()` 作用在整个 reader 生命周期上，是**连接级累计**上限而非单行上限
    // （见代码审查 P2-17）。此处保留该行为以约束 Approvals 的内存增长。
    let mut reader = BufReader::new(reader).take(MAX_LINE_BYTES);
    let mut writer = writer;
    let mut line = String::new();
    // 本连接是否已注册为 UI 通道：只有已注册通道能兑现确认令牌（P1-8）
    let mut is_ui = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // 对端关闭
        }
        for resp in handle_request(&line, cfg, gate, approvals, ui_key, &mut is_ui) {
            writer.write_all(encode(&resp).as_bytes())?;
            writer.flush()?;
        }
    }
}

/// 写一条审计，失败时打 stderr。
///
/// `docs/ai-permissions.md` 机制 2 承诺"审计写入失败不阻断执行，但**不允许静默
/// 零留痕**"。用 `let _ = gate.audit(...)` 恰恰就是静默 —— 磁盘满之后审计会停止，
/// 而系统继续正常运行，没有任何人会发现。这里把它显式化。
fn audit_or_warn(gate: &Gate, tool: &str, level: Level, verdict: &str) {
    if let Err(e) = gate.audit(tool, level, "", verdict) {
        eprintln!("[aetherd] 审计日志写入失败（{tool}/{verdict}）: {e}");
    }
}

fn handle_request(
    line: &str,
    cfg: &Config,
    gate: &Gate,
    approvals: &Approvals,
    ui_key: &str,
    is_ui: &mut bool,
) -> Vec<Response> {
    let Ok(req) = aether_ipc::decode::<Request>(line) else {
        return vec![Response::Error { code: 1, message: "无法解析的请求".into() }];
    };
    match req {
        Request::Ping => vec![Response::Pong],
        Request::RegisterUi { key } => {
            if key == ui_key {
                *is_ui = true;
                eprintln!("[aetherd] UI 通道已注册");
                vec![Response::UiRegistered]
            } else {
                eprintln!("[aetherd] UI 通道注册失败：密钥不匹配");
                vec![Response::Error { code: 403, message: "UI 通道注册失败：密钥不匹配".into() }]
            }
        }
        Request::ConfirmCancel { token } => {
            // 与剪贴板 / 配置重载同一门槛：撤销令牌也是"改系统状态"。
            //
            // 未注册连接**本来**拿不到令牌（L2+ 请求会被 403 且不下发），所以这不是
            // 可利用的漏洞；补它是为了**一致性** —— 万一令牌从别的路径泄漏（日志、
            // 转发），也不该让未注册连接能撤销用户的确认。
            if !*is_ui {
                audit_or_warn(gate, "confirm_cancel", Level::L2, verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝撤销令牌：本连接未注册为 UI 通道");
                return vec![Response::Error {
                    code: 403,
                    message: "确认撤销必须由已注册的 UI 通道发起".into(),
                }];
            }
            // 让"用户拒绝了"成为服务端的事实：撤销令牌 + 记入拒绝冷却 + 落审计。
            // 此前合成器的"拒绝"只是本地 UI 状态，服务端既不知道也没留痕（P1-9）。
            match approvals.revoke(&token) {
                Some((tool, args)) => {
                    gate.mark_denied(&tool, &args);
                    if let Err(e) = gate.audit(&tool, tool_level(&tool), &args.to_string(), verdict::DENIED_BY_USER) {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 用户拒绝了 {tool}：令牌已撤销，{} 秒内不再重复询问", DENY_COOLDOWN_SECS);
                    vec![Response::ConfirmCancelled { token }]
                }
                None => vec![Response::Error {
                    code: 404,
                    message: "确认令牌不存在或已失效（可能已过期、已使用或已撤销）".into(),
                }],
            }
        }
        Request::Chat { session_id, text } => handle_chat(&session_id, &text, cfg, gate, approvals, *is_ui),
        Request::ToolCall { tool, arguments, approval, .. } => {
            // 闸门的第一道：**资源敏感的工具**（剪贴板读写）必须由已注册的 UI 通道发起。
            //
            // 为什么加在这里（2026-10-02 审计 H-2）：P1-1 的修复把门槛加在
            // `ClipboardGet/Set` 两个 `Request` 变体上，而同一份数据经
            // `ToolCall{tool:"clipboard_read"}` 照样能拿到 —— 实测同一连接
            // 走协议端点 403、走工具路径成功。所以门槛必须按**资源**判，
            // 不能按**入口变体**判；判定表只有 `tools::TRUSTED_CHANNEL_TOOLS` 一处。
            if tools::requires_trusted_channel(&tool) && !*is_ui {
                audit_or_warn(gate, &tool, tool_level(&tool), verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝 {tool}：该工具读写用户环境数据，必须由已注册的 UI 通道发起");
                return vec![Response::Error {
                    code: 403,
                    message: format!("「{tool}」读写用户环境数据，必须由已注册的 UI 通道发起"),
                }];
            }
            // 是否"已授权"只能由服务端签发的令牌证明；且**只有已注册的 UI 通道**
            // 能兑现它——令牌本身不再等价于放行权（P1-8）。
            let approved = match &approval {
                Some(token) => {
                    if !*is_ui {
                        eprintln!("[aetherd] 拒绝兑现确认令牌：本连接未注册为 UI 通道（工具 {tool}）");
                        return vec![Response::Error {
                            code: 403,
                            message: "确认令牌兑现失败：本连接未注册为 UI 通道".into(),
                        }];
                    }
                    match approvals.redeem(token, &tool, &arguments) {
                        Ok(()) => true,
                        Err(reason) => {
                            return vec![Response::Error {
                                code: 403,
                                message: format!("确认令牌校验失败：{reason}"),
                            }]
                        }
                    }
                }
                None => false,
            };
            let mut ctx = tools::ToolCtx::default();
            let out = match tools::execute(gate, &mut ctx, &tool, &arguments, approved) {
                Ok(tools::ExecOutcome::Done(output)) => vec![Response::ToolResult { tool, ok: true, output }],
                Ok(tools::ExecOutcome::NeedsConfirmation { tool, level, arguments, consequence, echo_required }) => {
                    // 只有 UI 通道才配拿到确认令牌：未注册连接触发的 L2+ 直接拒绝，
                    // 不把令牌发给不可信的请求方。
                    if !*is_ui {
                        eprintln!("[aetherd] 拒绝下发确认令牌：本连接未注册为 UI 通道（工具 {tool}）");
                        return vec![Response::Error {
                            code: 403,
                            message: format!("L2+ 操作（{tool}）必须由已注册的 UI 通道发起"),
                        }];
                    }
                    let token = approvals.issue(&tool, &arguments);
                    // `Level` 的 Display 已经是 "L3 危险"，这里只取数字，避免拼成 "LL3 危险"
                    // （第四轮审查 P3-1）。与 handle_chat 里的同一行保持同一种写法。
                    eprintln!("[aetherd] 工具 {tool} 需 L{} 确认，已下发确认请求", level as u8);
                    vec![Response::NeedsConfirmation {
                        tool,
                        level: level as u8,
                        arguments,
                        consequence: consequence.to_string(),
                        echo_required,
                        token,
                    }]
                }
                Err(e) => vec![Response::ToolResult { tool, ok: false, output: format!("[工具错误] {e}") }],
            };
            let mut out = out;
            if out.iter().any(|r| matches!(r, Response::ToolResult { ok: true, .. })) {
                for a in ctx.desktop_actions {
                    out.push(Response::Action { name: a.name, arguments: a.arguments });
                }
            }
            out
        }
        // 剪贴板是"读用户数据 + 写用户环境"的端点：它常被用来复制密码，
        // 而写入等于替用户决定他下次粘贴出什么。因此与 L2+ 令牌**同一门槛**：
        // 只有已注册的 UI 通道能碰。
        //
        // 第四轮审查（P1-1）前这里是零门槛的 —— 未注册连接可直接读写，只补了一行
        // audit。根因是 `handle_request` 的权限检查**逐分支手写**，新增 `Request`
        // 变体就多一处漏检的入口。本文件的 `request_variants_are_gated` 测试
        // 就是为了防止重演。
        Request::ClipboardSet { text } => {
            if !*is_ui {
                audit_or_warn(gate, "clipboard_set", Level::L1, verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝剪贴板写入：本连接未注册为 UI 通道");
                return vec![Response::Error {
                    code: 403,
                    message: "剪贴板访问必须由已注册的 UI 通道发起".into(),
                }];
            }
            audit_or_warn(gate, "clipboard_set", Level::L1, verdict::ALLOWED);
            match crate::clipboard::set(&text) {
                Ok(bytes) => vec![Response::ClipboardWritten { bytes }],
                Err(e) => vec![Response::Error { code: 413, message: e }],
            }
        }
        Request::ClipboardGet => {
            if !*is_ui {
                audit_or_warn(gate, "clipboard_get", Level::L1, verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝剪贴板读取：本连接未注册为 UI 通道");
                return vec![Response::Error {
                    code: 403,
                    message: "剪贴板访问必须由已注册的 UI 通道发起".into(),
                }];
            }
            audit_or_warn(gate, "clipboard_get", Level::L1, verdict::ALLOWED);
            vec![Response::ClipboardText { text: crate::clipboard::get() }]
        }
        Request::ReloadConfig => {
            // 改配置 = 改 AI 的能力边界，与剪贴板同一门槛：只有已注册 UI 通道能发起
            if !*is_ui {
                audit_or_warn(gate, "reload_config", Level::L2, verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝配置重载：本连接未注册为 UI 通道");
                return vec![Response::Error {
                    code: 403,
                    message: "配置重载必须由已注册的 UI 通道发起".into(),
                }];
            }
            audit_or_warn(gate, "reload_config", Level::L2, verdict::ALLOWED);
            eprintln!("[aetherd] 配置重载请求已受理：退出以便 aether-init 用新配置重启");
            // 延迟退出：先把响应发出去，再终止进程。
            // 不做热替换是有意的 —— 重启后的状态必然与配置文件一致。
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                std::process::exit(0);
            });
            vec![Response::ConfigReloading]
        }
        Request::SysInfo { .. } => vec![Response::SysInfo(sys_report())],
        Request::ServiceControl { unit, action } => vec![Response::ServiceAck {
            unit,
            ok: false,
            message: format!("服务控制 {action:?} 将在 M3 aether-init 就绪后生效"),
        }],
    }
}

/// 拒绝冷却时长（与 perm::DENY_TTL 保持一致，仅用于日志文案）。
const DENY_COOLDOWN_SECS: u64 = 300;

/// 从工具注册表查等级（审计"用户拒绝"时需要写等级列）。
fn tool_level(name: &str) -> Level {
    tools::registry()
        .into_iter()
        .find(|t| t.name == name)
        .map(|t| t.level)
        .unwrap_or(Level::L2)
}

/// Chat 请求：快速意图 → 命中则回复+桌面行为；未命中 → LLM agent。
/// 注意：Action / NeedsConfirmation 必须在 done 前发送——客户端（compositor）
/// 收到 done 即停止读取。
fn handle_chat(
    session_id: &str,
    text: &str,
    cfg: &Config,
    gate: &Gate,
    approvals: &Approvals,
    is_ui: bool,
) -> Vec<Response> {
    if let Some((reply, action)) = intent::try_handle(text) {
        let mut out = Vec::new();
        if let Some(a) = action {
            out.push(Response::Action { name: a.name, arguments: a.arguments });
        }
        out.push(Response::ChatChunk {
            session_id: session_id.into(),
            delta: reply,
            done: true,
            // 快速意图是确定性本地通道，无需 LLM
            channel: Some("local".into()),
        });
        return out;
    }

    // 未命中快速意图：交给 LLM agent（可能较慢，连接线程阻塞在此处即可）
    // `is_ui` 一路传下去：agent 可能调起"需要可信通道"的工具（剪贴板），
    // 未注册连接发起的对话不得通过 LLM 绕过这道闸门（2026-10-02 审计 H-2）。
    match crate::agent_run(cfg, gate, text, is_ui) {
        Ok(outcome) => {
            let mut out = Vec::new();
            for a in outcome.actions {
                out.push(Response::Action { name: a.name, arguments: a.arguments });
            }
            // L2+ 操作暂停在确认上：签发一次性令牌交给 UI，模型看不到它。
            // 未注册的通道拿不到令牌——否则它可以把令牌转给别处兑现（P1-8）。
            if let Some(p) = outcome.pending {
                if !is_ui {
                    eprintln!(
                        "[aetherd] 拒绝下发确认令牌：本连接未注册为 UI 通道（工具 {}）",
                        p.tool
                    );
                    out.push(Response::Error {
                        code: 403,
                        message: format!(
                            "「{}」需要用户确认，但本连接未注册为 UI 通道，无法下发确认令牌",
                            p.tool
                        ),
                    });
                } else {
                    let token = approvals.issue(&p.tool, &p.arguments);
                    eprintln!("[aetherd] 工具 {} 需 L{} 确认，已下发确认请求", p.tool, p.level);
                    out.push(Response::NeedsConfirmation {
                        tool: p.tool,
                        level: p.level,
                        arguments: p.arguments,
                        consequence: p.consequence,
                        echo_required: p.echo_required,
                        token,
                    });
                }
            }
            out.push(Response::ChatChunk {
                session_id: session_id.into(),
                delta: outcome.answer,
                done: true,
                // 本轮实际推理通道（"local"/"cloud"），顶栏三态据此渲染
                channel: outcome.channel,
            });
            out
        }
        Err(e) => {
            // 区分"没配模型"与"配了但连不上"：前者要引导用户配置，后者要检查服务。
            // 这是 0.6 的 UI 侧落点 —— 用户看不到配置状态，只能靠这条提示。
            let unconfigured = cfg.cloud.is_none() && !crate::llm::local_available(&cfg.local.base_url);
            let hint = if unconfigured {
                "（未检测到可用模型：在终端运行 `aetherd config --help` 配置本地 Ollama 或云端 API Key）"
            } else {
                ""
            };
            vec![Response::Error {
                code: 503,
                message: format!("AI 通道不可用: {e}{hint}"),
            }]
        }
    }
}

fn sys_report() -> SysReport {
    // 真实数据：/proc 内存 + aether-init 服务列表（collect_sys_report 内部按缺省降级）
    crate::collect_sys_report()
}

#[cfg(test)]
mod ipc_gating_tests {
    use super::*;
    use crate::perm::{Approvals, Gate};
    use crate::test_config;

    /// 剪贴板是**进程级全局状态**，而测试默认并行 —— 不串行化会互相覆盖。
    /// 这是全局状态 + 并行测试的标准处理方式。
    static CLIP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        CLIP_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn handle_line(line: &str, is_ui: &mut bool) -> Vec<Response> {
        let cfg = test_config();
        let gate = Gate::new(std::env::temp_dir().join("aether_clipboard_audit_test.log"));
        let approvals = Approvals::new();
        handle_request(line, &cfg, &gate, &approvals, "test-key", is_ui)
    }

    #[test]
    fn clipboard_set_then_get_roundtrip() {
        let _g = lock();
        let mut is_ui = true; // 剪贴板要求已注册 UI 通道
        let mut out = handle_line(
            &aether_ipc::encode(&Request::ClipboardSet { text: "abc 剪贴板".into() }),
            &mut is_ui,
        );
        assert!(
            matches!(out.remove(0), Response::ClipboardWritten { .. }),
            "写入应得到确认"
        );
        let out = handle_line(&aether_ipc::encode(&Request::ClipboardGet), &mut is_ui);
        match &out[0] {
            Response::ClipboardText { text } => assert_eq!(text, "abc 剪贴板"),
            other => panic!("意外的应答：{other:?}"),
        }
    }

    #[test]
    fn clipboard_oversized_is_rejected_without_clobbering() {
        let _g = lock();
        let mut is_ui = true;
        crate::clipboard::set("keep").unwrap();
        let big = "x".repeat(crate::clipboard::MAX_BYTES + 1);
        let out = handle_line(
            &aether_ipc::encode(&Request::ClipboardSet { text: big }),
            &mut is_ui,
        );
        assert!(
            matches!(out[0], Response::Error { code: 413, .. }),
            "超长应整体拒绝"
        );
        assert_eq!(crate::clipboard::get(), "keep", "拒绝后原内容不受影响");
    }

    /// P1-1 回归：剪贴板是"读用户数据 + 写用户环境"的端点，未注册连接一律 403。
    #[test]
    fn clipboard_requires_registered_ui() {
        let _g = lock();
        let mut is_ui = false;
        let out = handle_line(&aether_ipc::encode(&Request::ClipboardGet), &mut is_ui);
        assert!(
            matches!(out[0], Response::Error { code: 403, .. }),
            "未注册连接读剪贴板必须被拒，实得 {out:?}"
        );
        let out = handle_line(
            &aether_ipc::encode(&Request::ClipboardSet { text: "x".into() }),
            &mut is_ui,
        );
        assert!(
            matches!(out[0], Response::Error { code: 403, .. }),
            "未注册连接写剪贴板必须被拒，实得 {out:?}"
        );
    }

    /// 2.2b：把"每个能读用户数据或改系统状态的 `Request` 变体都必须被拦"
    /// 固化成门禁。
    ///
    /// 存在的理由：`handle_request` 的权限检查是**逐分支手写**的，新增变体容易
    /// 漏掉门槛 —— P1-1（剪贴板零门槛）就是这么来的。**新增变体时若忘了加门槛，
    /// 这里会红。**
    #[test]
    fn request_variants_are_gated() {
        let _g = lock();
        let mut is_ui = false;
        let cases: Vec<(&str, Request)> = vec![
            ("clipboard_get", Request::ClipboardGet),
            ("clipboard_set", Request::ClipboardSet { text: "x".into() }),
            ("reload_config", Request::ReloadConfig),
            ("confirm_cancel", Request::ConfirmCancel { token: "deadbeef".into() }),
            (
                "tool_call(L3)",
                Request::ToolCall {
                    session_id: "gate-test".into(),
                    tool: "install_disk".into(),
                    arguments: serde_json::json!({"disk": "/dev/vda", "confirm": "/dev/vda"}),
                    approval: None,
                },
            ),
            (
                // 写操作也是 L2：未注册连接同样拿不到令牌
                "tool_call(L2 写)",
                Request::ToolCall {
                    session_id: "gate-test".into(),
                    tool: "file_delete".into(),
                    arguments: serde_json::json!({"path": "C:/Temp/x"}),
                    approval: None,
                },
            ),
        ];
        for (name, req) in cases {
            let out = handle_line(&aether_ipc::encode(&req), &mut is_ui);
            let denied = out.iter().any(|r| matches!(r, Response::Error { code: 403, .. }));
            assert!(denied, "{name} 在未注册连接下必须被拒（403），实得 {out:?}");
        }
    }

    /// 反向断言：不该被拦的变体要保持可用 —— 否则上面的门禁可以靠"一律拒绝"作弊。
    #[test]
    fn unprivileged_variants_still_work() {
        let _g = lock();
        let mut is_ui = false;
        let out = handle_line(&aether_ipc::encode(&Request::Ping), &mut is_ui);
        assert!(matches!(out[0], Response::Pong), "ping 不应要求注册，实得 {out:?}");
        let out = handle_line(
            &aether_ipc::encode(&Request::SysInfo { scope: aether_ipc::SysInfoScope::Memory }),
            &mut is_ui,
        );
        assert!(
            matches!(out[0], Response::SysInfo(_)),
            "只读的 sys_info 不应要求注册，实得 {out:?}"
        );
    }

    /// H-2 回归：**工具路径**同样受可信通道约束。
    ///
    /// 这条是 2026-10-02 审计的实测结论固化：同一份剪贴板数据，走协议端点 403，
    /// 走 `ToolCall` 此前却成功 —— 闸门按入口变体手写，就必然漏掉等价入口。
    /// 上面 `request_variants_are_gated` 只覆盖 `Request` 变体，**覆盖不到这个漏口**，
    /// 所以必须单独钉一条。
    #[test]
    fn tool_call_clipboard_requires_registered_ui() {
        let _g = lock();
        // 未注册连接：读、写都被拒
        for (name, tool, args) in [
            ("clipboard_read", "clipboard_read", serde_json::json!({})),
            ("clipboard_write", "clipboard_write", serde_json::json!({"text": "x"})),
        ] {
            let mut is_ui = false;
            let out = handle_line(
                &aether_ipc::encode(&Request::ToolCall {
                    session_id: "gate-test".into(),
                    tool: tool.into(),
                    arguments: args,
                    approval: None,
                }),
                &mut is_ui,
            );
            assert!(
                out.iter().any(|r| matches!(r, Response::Error { code: 403, .. })),
                "{name} 在未注册连接下必须 403，实得 {out:?}"
            );
        }

        // 反向断言：已注册通道仍可用（防"一律拒绝"式作弊）
        let mut is_ui = true;
        let out = handle_line(
            &aether_ipc::encode(&Request::ToolCall {
                session_id: "gate-test".into(),
                tool: "clipboard_write".into(),
                arguments: serde_json::json!({"text": "已注册通道可写"}),
                approval: None,
            }),
            &mut is_ui,
        );
        assert!(
            out.iter().any(|r| matches!(r, Response::ToolResult { ok: true, .. })),
            "已注册通道写剪贴板应成功，实得 {out:?}"
        );
    }
}
