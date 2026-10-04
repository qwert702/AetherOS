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

/// 退出码：拒绝监听非回环地址（`AETHER_BIND` 指向网络但没有二次确认）。
///
/// 用命名常量而不是裸数字（代码审查指出）：启动脚本/服务定义要靠退出码区分
/// "配置被安全策略拒绝"与"真的启动失败"，裸 `2`/`3` 让人只能去读源码。
const EXIT_REMOTE_IPC_DENIED: i32 = 2;
/// 退出码：Unix socket 建不起来且没有显式允许"仅 TCP"（见 `serve()` 里的说明）。
#[cfg(unix)]
const EXIT_UNIX_SOCKET_UNAVAILABLE: i32 = 3;

/// aetherd 的 Unix socket 路径（定义在协议 crate 里，两侧共用）。
///
/// **默认通道**：socket 由文件权限兜底（`/run` 属 root，socket 0600），因此
/// "对端是不是 aetherd"由内核保证 —— 而 TCP 上客户端**无法验证对端**，谁抢到
/// 7311 谁就能收到 `RegisterUi` 里的 UI 密钥（2026-10-02 审计 M-17）。
/// TCP 仍然保留（开发机预览与 hostfwd 调试），但客户端默认不再用它注册。
#[cfg(unix)]
const UNIX_SOCKET_PATH: &str = aether_ipc::UNIX_SOCKET_PATH;

/// UI 通道密钥的落盘位置。
///
/// **为什么从 `/var/log/aether/` 迁到 `/run/aether/`**（2026-10-02 审计 H-1）：
/// 旧位置正好在 `read_file` 的读取白名单里，于是"任意本机进程 → ToolCall(read_file)
/// → 取走密钥 → 注册为 UI 通道 → 自我确认 L3"是一条完整链路（已实测复现）。
/// `/run/aether` 同样是白名单根，所以真正的拦截靠 `tools::READ_DENY_SUBPATHS`
/// 里的 `ui.key`；迁址是第二道防线，同时让"密钥"与"日志"在语义上分开
/// （审计日志目录是给人看的，密钥不是）。
/// `/run` 是 ramfs：密钥每次开机重新生成，不再跨重启残留。
/// 常量本身定义在协议 crate 里（与合成器共用同一条路径）。
const UI_KEY_PATH: &str = aether_ipc::UI_KEY_PATH;

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
            // M-17 残余（2026-10-02 审计）：**对端身份校验**，与 aether-init 同一判据。
            // 0600 权限位已经挡住陌生用户，但"权限位正确"与"真的只接受预期身份"
            // 是两件事 —— 一旦权限被改宽（打包/挂载/误操作），这里仍会拒绝。
            // 读不到凭据 → 拒绝（fail-closed）。
            if let Err(reason) = aether_ipc::check_peer(&stream) {
                eprintln!("[aetherd] 拒绝 Unix 连接：{reason}");
                continue;
            }
            stream.set_read_timeout(Some(IDLE_TIMEOUT)).ok();
            // 写超时同样必须有（对抗审查发现）：只设读超时的话，一个"连上但不读"的
            // 对端会让 `write_all` **永久阻塞**在该 socket 上 —— 32 条这样的连接就能
            // 占满全部连接槽，合法客户端（合成器）再也连不进来。写超时到点后
            // write_all 返回错误、连接被关闭、线程释放。
            stream.set_write_timeout(Some(IDLE_TIMEOUT)).ok();
            match stream.try_clone() {
                Ok(reader) => shared.spawn_conn(reader, stream),
                Err(e) => eprintln!("[aetherd] Unix 连接克隆失败: {e}"),
            }
        }
    });
    Ok(())
}

/// 解析监听地址。**默认只听回环**；非回环需要显式二次确认（2026-10-02 审计 I-6）。
///
/// 为什么不能只靠"脚本里别设 0.0.0.0"：IPC 里**没有任何用户级认证** ——
/// 谁连上来谁就能调 `read_file`（以 root 读 /home 下任意文件）、发 `ToolCall`、
/// 甚至注册 UI 通道。一个环境变量就能把这个接口交给整个局域网，
/// 而"脚本没设"不是技术保证，只是纪律。
fn resolve_bind() -> String {
    let bind = std::env::var("AETHER_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    // 回环判定用**解析后的 IP**，而不是三个字面量（对抗审查发现）：
    // `127.0.0.2`、`127.1`、`[::1]` 都是合法回环写法，字面量匹配会把它们误判成
    // "非回环"而拒绝启动（可用性误伤）。主机名解析不了时按非回环处理（保守）。
    let loopback = bind
        .trim_matches(|c| c == '[' || c == ']')
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false);
    if !loopback {
        if std::env::var("AETHER_ALLOW_REMOTE_IPC").as_deref() != Ok("1") {
            eprintln!(
                "[aetherd] 拒绝监听 {bind}：IPC 没有用户级认证，非回环监听等于把\
                 「以 root 读写文件」暴露给网络。"
            );
            eprintln!(
                "         确实需要（例如隔离网络里调试）请**显式**设 AETHER_ALLOW_REMOTE_IPC=1"
            );
            std::process::exit(EXIT_REMOTE_IPC_DENIED);
        }
        eprintln!(
            "[aetherd] ⚠ 警告：正在监听 {bind}（非回环）—— IPC 无认证，请确认网络可信"
        );
    }
    bind
}

pub fn serve(cfg: Config) -> anyhow::Result<()> {
    let bind = resolve_bind();
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

    // 把自动放行阈值打出来（审计 I-9 的配套）：闸门松紧是安全相关的状态，
    // 应该能从启动日志里看出来，而不是只存在于代码里。
    eprintln!(
        "[aetherd] 权限闸门：自动放行低于 {:?}，其余需已注册 UI 通道确认",
        shared.gate.auto_approve_below()
    );

    // Unix socket 优先通道（0600）。
    //
    // 建不起来**不再静默降级**（代码审查 2.4）：合成器只连 socket、**不回退 TCP**，
    // 所以"只剩 TCP"意味着桌面失去 L2+ 确认、剪贴板、服务控制 —— 而用户只会看到
    // 一行日志。要么显式声明"我只要 TCP"（开发机调试），要么直接失败退出让问题可见。
    #[cfg(unix)]
    if let Err(e) = spawn_unix_listener(shared.clone()) {
        if std::env::var("AETHER_ALLOW_TCP_ONLY").as_deref() == Ok("1") {
            eprintln!(
                "[aetherd] 警告：Unix socket 建立失败（{e}）—— 已按 AETHER_ALLOW_TCP_ONLY=1 \
                 仅提供 TCP 通道"
            );
        } else {
            eprintln!("[aetherd] 致命：Unix socket 建立失败（{e}）");
            eprintln!(
                "         合成器只连 socket、不回退 TCP，继续跑等于桌面失去确认/剪贴板/服务控制。"
            );
            eprintln!("         开发机调试可显式设 AETHER_ALLOW_TCP_ONLY=1");
            std::process::exit(EXIT_UNIX_SOCKET_UNAVAILABLE);
        }
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        stream.set_read_timeout(Some(IDLE_TIMEOUT)).ok();
        // 写超时：理由同 spawn_unix_listener（防"连上不读"占满连接槽）
        stream.set_write_timeout(Some(IDLE_TIMEOUT)).ok();
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
            // 只在**权限确实收紧到 0600 之后**才这样打印（代码审查发现）：
            // 此前 `let _` 吞掉 chmod 错误后无条件打印"（0600）"，日志便断言了一个
            // 未经验证的权限事实 —— 而 H-1 的整个论证正建立在"密钥是 0600"上。
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                match std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
                    Ok(()) => eprintln!("[aetherd] 已生成 UI 通道密钥 → {}（0600）", path.display()),
                    Err(e) => eprintln!(
                        "[aetherd] 警告：已生成 UI 通道密钥 → {}，但收紧权限失败（{e}）—— \
                         该文件可能被同机其它用户读取，请手工 `chmod 600`", path.display()
                    ),
                }
            }
            #[cfg(not(unix))]
            eprintln!("[aetherd] 已生成 UI 通道密钥 → {}", path.display());
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

/// 读一行请求，**按行**限长（2026-10-02 审计 I-10）。
///
/// 原来用 `BufReader::take(MAX_LINE_BYTES)`：`take` 作用在读取器上，是**连接级累计**
/// 上限 —— 一条连接累计读满 1MB 后被静默关闭（正常的长连接、多轮对话会莫名断开），
/// 而单条超长行反而不受这个限制约束（它在累计到 1MB 之前就能把内存吃满）。
/// 改成按行判：超长行报错断开，正常连接不再有累计上限。
fn read_line_capped<R: BufRead + ?Sized>(reader: &mut R, max: u64) -> std::io::Result<String> {
    let mut line = String::new();
    let n = (&mut *reader).take(max).read_line(&mut line)?;
    if n as u64 >= max && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("request line exceeds the {max} byte cap"),
        ));
    }
    Ok(line)
}

/// 处理一条连接（TCP 与 Unix socket 共用）。
///
/// `reader`/`writer` 是同一连接的两个句柄：读端按行限长，写端逐条 flush，
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
    let mut reader = BufReader::new(reader);
    let mut writer = writer;
    let mut line: String;
    // 本连接是否已注册为 UI 通道：只有已注册通道能兑现确认令牌（P1-8）
    let mut is_ui = false;
    // 本连接界面的语言（§3.5）：随 `RegisterUi` 一起来，回执文案据此取词
    let mut ui_en = false;
    loop {
        line = read_line_capped(&mut reader, MAX_LINE_BYTES)?;
        if line.is_empty() {
            return Ok(()); // 对端关闭
        }
        for resp in handle_request(&line, cfg, gate, approvals, ui_key, &mut is_ui, &mut ui_en) {
            writer.write_all(encode(&resp).as_bytes())?;
            writer.flush()?;
        }
    }
}

/// 写一条审计，失败时打 stderr。
///
/// `docs/AI权限模型（AI-PERMISSIONS）-2026-09-05.md` 机制 2 承诺"审计写入失败不阻断执行，但**不允许静默
/// 零留痕**"。用 `let _ = gate.audit(...)` 恰恰就是静默 —— 磁盘满之后审计会停止，
/// 而系统继续正常运行，没有任何人会发现。这里把它显式化。

/// 上次对时的偏移（毫秒）。`i64::MIN` = **从未对过** —— 不假装是 0：
/// "偏移 0" 是一个有意义的结果（完全准），与"没对过"必须区分开。
static LAST_OFFSET_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);
/// 上次对时的 Unix 秒（0 = 从未对过）。
static LAST_SYNC_UNIX: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

fn audit_or_warn(gate: &Gate, tool: &str, level: Level, verdict: &str) {
    if let Err(e) = gate.audit_note(tool, level, verdict) {
        eprintln!("[aetherd] 审计日志写入失败（{tool}/{verdict}）: {e}");
    }
}

/// 口令策略失败的**文案**（§3.5）。
///
/// 与 `preview_error_text` 同一套：`accounts` 那一层只回答"哪里不合规"（码），
/// 措辞在这里按界面语言渲染。
fn password_problem_text(en: bool, p: crate::accounts::PasswordProblem) -> String {
    use crate::accounts::PasswordProblem as P;
    match (p, en) {
        (P::Empty, false) => "口令不能为空",
        (P::Empty, true) => "the password cannot be empty",
        (P::TooShort, false) => "口令太短：至少 8 位",
        (P::TooShort, true) => "the password is too short: at least 8 characters",
        (P::TooLong, false) => "口令太长：最多 128 位",
        (P::TooLong, true) => "the password is too long: at most 128 characters",
        (P::ContainsNewline, false) => "口令里不能有换行",
        (P::ContainsNewline, true) => "the password cannot contain a line break",
        (P::ContainsColon, false) => "口令里不能有冒号",
        (P::ContainsColon, true) => "the password cannot contain a colon",
    }
    .to_string()
}

fn handle_request(
    line: &str,
    cfg: &Config,
    gate: &Gate,
    approvals: &Approvals,
    ui_key: &str,
    is_ui: &mut bool,
    ui_en: &mut bool,
) -> Vec<Response> {
    let Ok(req) = aether_ipc::decode::<Request>(line) else {
        return vec![Response::Error { code: 1, message: "unparsable request".into() }];
    };
    match req {
        Request::Ping => vec![Response::Pong],
        Request::RegisterUi { key, en } => {
            if key == ui_key {
                *is_ui = true;
                *ui_en = en;
                eprintln!("[aetherd] UI 通道已注册");
                vec![Response::UiRegistered]
            } else {
                eprintln!("[aetherd] UI 通道注册失败：密钥不匹配");
                vec![Response::Error { code: 403, message: "UI channel registration failed: key mismatch".into() }]
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
                    message: "revoking a confirmation requires a registered UI channel".into(),
                }];
            }
            // 让"用户拒绝了"成为服务端的事实：撤销令牌 + 记入拒绝冷却 + 落审计。
            // 此前合成器的"拒绝"只是本地 UI 状态，服务端既不知道也没留痕（P1-9）。
            match approvals.revoke(&token) {
                Some((tool, args)) => {
                    gate.mark_denied(&tool, &args);
                    // 走 `audit_call`：参数在 Gate 内部统一脱敏。否则"用户拒绝了一个
                    // file_write"这条记录会把待写入的全文（可能是密钥）原样落盘 ——
                    // M-1 的修复当初只覆盖了 tools::execute 一处（自查补漏）。
                    if let Err(e) = gate.audit_call(&tool, tool_level(&tool), &args, verdict::DENIED_BY_USER) {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 用户拒绝了 {tool}：令牌已撤销，{} 秒内不再重复询问", DENY_COOLDOWN_SECS);
                    vec![Response::ConfirmCancelled { token }]
                }
                None => vec![Response::Error {
                    code: 404,
                    message: "confirmation token is unknown or no longer valid (expired, already used, or revoked)".into(),
                }],
            }
        }
        Request::Chat { session_id, text, en } => {
            handle_chat(&session_id, &text, en, cfg, gate, approvals, *is_ui)
        }
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
                    message: format!("「{tool}」 reads/writes user environment data and requires a registered UI channel"),
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
                            message: "cannot redeem the confirmation token: this connection is not a registered UI channel".into(),
                        }];
                    }
                    match approvals.redeem(token, &tool, &arguments) {
                        Ok(()) => true,
                        Err(reason) => {
                            return vec![Response::Error {
                                code: 403,
                                message: format!("confirmation token check failed: {reason}"),
                            }]
                        }
                    }
                }
                None => false,
            };
            let mut ctx = tools::ToolCtx::default();
            // §3.5：工具返回给用户的文案跟着**连接**的语言（如剪贴板超长）
            ctx.en = *ui_en;
            let out = match tools::execute(gate, &mut ctx, &tool, &arguments, approved) {
                Ok(tools::ExecOutcome::Done(output)) => vec![Response::ToolResult { tool, ok: true, output }],
                Ok(tools::ExecOutcome::NeedsConfirmation { tool, level, arguments, consequence, echo_required }) => {
                    // 只有 UI 通道才配拿到确认令牌：未注册连接触发的 L2+ 直接拒绝，
                    // 不把令牌发给不可信的请求方。
                    if !*is_ui {
                        eprintln!("[aetherd] 拒绝下发确认令牌：本连接未注册为 UI 通道（工具 {tool}）");
                        return vec![Response::Error {
                            code: 403,
                            message: format!("L2+ operation (「{tool}」) requires a registered UI channel"),
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
                Err(e) => vec![Response::ToolResult { tool, ok: false, output: format!("[tool error] {e}") }],
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
                    message: "clipboard access requires a registered UI channel".into(),
                }];
            }
            audit_or_warn(gate, "clipboard_set", Level::L1, verdict::ALLOWED);
            match crate::clipboard::set(&text, *ui_en) {
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
                    message: "clipboard access requires a registered UI channel".into(),
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
                    message: "config reload requires a registered UI channel".into(),
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
        // **电源动作不走这条入口**（2026-10-02 功能批次 J）。
        //
        // `Request::SystemPower` 在协议里存在，但它的合法使用方向是
        // **aetherd → aether-init**（aetherd 确认过用户意图之后转发给 PID 1）。
        // 客户端若直接把它发给 aetherd，说明它想绕过 L3 确认流程 —— 一律拒绝。
        //
        // 这不是"多一道没用的检查"：`system_power` 工具的确认令牌由闸门签发并绑定参数，
        // 而直接发这个 Request 完全不需要令牌。两条路都通的话，确认机制就形同虚设。
        Request::SystemPower { action } => {
            audit_or_warn(gate, "system_power", Level::L3, verdict::REJECTED_NO_UI);
            eprintln!(
                "[aetherd] 拒绝直接下发的电源请求（{}）：必须经 system_power 工具（L3 + 用户确认）",
                action.as_str()
            );
            vec![Response::Error {
                code: 403,
                message: format!(
                    "电源动作必须经 system_power 工具执行（L3，需用户确认）；\
                     不接受直接下发的 {}",
                    action.as_str()
                ),
            }]
        }
        Request::GetDisplays => {
            // 只读的诊断信息；但仍然要求 UI 通道 —— 与其它"读取系统状态"的入口一致。
            if !*is_ui {
                audit_or_warn(gate, "get_displays", Level::L0, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "reading display outputs requires a registered UI channel".into(),
                }];
            }
            let outputs: Vec<aether_ipc::DisplayWire> =
                crate::display::scan(std::path::Path::new("/sys/class/graphics"))
                    .into_iter()
                    .map(|o| aether_ipc::DisplayWire {
                        index: o.index,
                        name: o.name,
                        xres: o.xres,
                        yres: o.yres,
                        bpp: o.bpp,
                    })
                    .collect();
            vec![Response::DisplayList {
                driven_index: crate::display::DRIVEN_INDEX,
                outputs,
            }]
        }
        Request::ListRemovable => {
            // 可移动设备列表会暴露"这台机器上插了什么" —— 与快照/更新同一门槛。
            if !*is_ui {
                audit_or_warn(gate, "list_removable", Level::L0, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "listing removable devices requires a registered UI channel".into(),
                }];
            }
            let scanned = crate::removable::scan(std::path::Path::new("/sys/block"));
            let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
            let with = crate::removable::with_mounts(scanned, &mounts);
            let devices: Vec<aether_ipc::RemovableWire> = with
                .iter()
                .map(|r| aether_ipc::RemovableWire {
                    dev: r.dev.clone(),
                    bytes: r.bytes,
                    mounted_at: r.mounted_at.clone(),
                })
                .collect();
            vec![Response::RemovableList { devices }]
        }
        Request::MountRemovable { dev } => {
            if !*is_ui {
                audit_or_warn(gate, "mount_removable", Level::L2, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "mounting a removable device requires a registered UI channel".into(),
                }];
            }
            // **核心安全规则**：只允许挂载"本次扫描到的"设备（`mount()` 内部会再查一遍）
            let scanned = crate::removable::scan(std::path::Path::new("/sys/block"));
            let args = serde_json::json!({ "dev": &dev });
            match crate::removable::mount(&dev, &scanned) {
                Ok(mp) => {
                    if let Err(e) =
                        gate.audit_call("mount_removable", Level::L2, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 已只读挂载 {dev} → {mp}");
                    vec![Response::RemovableAck {
                        ok: true,
                        message: if *ui_en {
                    format!("mounted {dev} read-only at {mp}")
                } else {
                    format!("已只读挂载 {dev} → {mp}")
                },
                    }]
                }
                Err(e) => {
                    if let Err(e2) =
                        gate.audit_call("mount_removable", Level::L2, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 挂载失败：{dev} —— {e}");
                    vec![Response::RemovableAck { ok: false, message: e }]
                }
            }
        }
        Request::UnmountRemovable { dev } => {
            if !*is_ui {
                audit_or_warn(gate, "unmount_removable", Level::L2, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "unmounting a removable device requires a registered UI channel".into(),
                }];
            }
            let scanned = crate::removable::scan(std::path::Path::new("/sys/block"));
            let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
            let with = crate::removable::with_mounts(scanned, &mounts);
            let args = serde_json::json!({ "dev": &dev });
            match crate::removable::umount(&dev, &with) {
                Ok(msg) => {
                    if let Err(e) =
                        gate.audit_call("unmount_removable", Level::L2, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] {msg}");
                    vec![Response::RemovableAck { ok: true, message: msg }]
                }
                Err(e) => {
                    if let Err(e2) =
                        gate.audit_call("unmount_removable", Level::L2, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 卸载失败：{dev} —— {e}");
                    vec![Response::RemovableAck { ok: false, message: e }]
                }
            }
        }
        Request::ListUpdates => {
            // 更新包的清单会暴露"这台机器在跑什么版本、要装什么" —— 与快照同一门槛。
            if !*is_ui {
                audit_or_warn(gate, "list_updates", Level::L0, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "reading the update list requires a registered UI channel".into(),
                }];
            }
            let root = std::path::PathBuf::from(crate::update::UPDATE_ROOT);
            let staged_version = crate::update::staged(&root).map(|s| s.version);
            vec![Response::UpdateList {
                packages: crate::update::list_incoming(&root),
                current_version: crate::update::current_version(),
                staged_version,
            }]
        }
        Request::StageUpdate { name } => {
            if !*is_ui {
                audit_or_warn(gate, "stage_update", Level::L2, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "staging an update requires a registered UI channel".into(),
                }];
            }
            let root = std::path::PathBuf::from(crate::update::UPDATE_ROOT);
            // `name` 来自界面 → **只允许投递目录下的直接子目录**（见 update.rs 的说明）
            let args = serde_json::json!({ "name": &name });
            let res = crate::update::incoming_bundle(&root, &name).and_then(|bundle| {
                crate::update::stage(
                    &bundle,
                    &root,
                    &crate::update::current_version(),
                    crate::timesync::now_unix().0,
                )
            });
            match res {
                Ok(st) => {
                    if let Err(e) =
                        gate.audit_call("stage_update", Level::L2, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 更新已暂存：版本 {}（重启后应用）", st.version);
                    vec![Response::UpdateStaged {
                        ok: true,
                        message: format!(
                            "已暂存更新 {}（{} 个文件）· **重启后生效**",
                            st.version,
                            st.files.len()
                        ),
                        version: Some(st.version),
                    }]
                }
                Err(e) => {
                    if let Err(e2) =
                        gate.audit_call("stage_update", Level::L2, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 暂存更新失败：{name} —— {e}");
                    vec![Response::UpdateStaged { ok: false, message: e, version: None }]
                }
            }
        }
        Request::GetSnapshots => {
            // 快照清单会暴露"这台机器上装了什么/配了什么"（文件名本身就说明问题）——
            // 与读权限策略同一门槛：必须是已注册的 UI 通道。
            if !*is_ui {
                audit_or_warn(gate, "get_snapshots", Level::L0, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "reading the snapshot list requires a registered UI channel".into(),
                }];
            }
            let root = std::path::PathBuf::from(crate::snapshots::ROOT);
            let items: Vec<aether_ipc::SnapshotWire> = crate::snapshots::list(&root)
                .into_iter()
                .map(|m| aether_ipc::SnapshotWire {
                    id: m.id,
                    created_unix: m.created_unix,
                    file_count: m.entries.iter().filter(|e| e.existed).count(),
                    total: m.entries.len(),
                })
                .collect();
            eprintln!("[aetherd] 快照清单已读取（{} 个）", items.len());
            vec![Response::SnapshotList { items }]
        }
        Request::CreateSnapshot => {
            if !*is_ui {
                audit_or_warn(gate, "create_snapshot", Level::L1, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "creating a snapshot requires a registered UI channel".into(),
                }];
            }
            let root = std::path::PathBuf::from(crate::snapshots::ROOT);
            let now = crate::timesync::now_unix().0;
            match crate::snapshots::create(&root, now) {
                Ok(m) => {
                    let n = m.entries.iter().filter(|e| e.existed).count();
                    let args = serde_json::json!({ "id": m.id, "files": n });
                    if let Err(e) =
                        gate.audit_call("create_snapshot", Level::L1, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 配置快照已创建：{}（{n} 个文件）", m.id);
                    vec![Response::SnapshotAck {
                        ok: true,
                        message: if *ui_en {
                    format!("created config snapshot {} ({n} files)", m.id)
                } else {
                    format!("已创建配置快照 {}（{n} 个文件）", m.id)
                },
                        notes: Vec::new(),
                    }]
                }
                Err(e) => {
                    let args = serde_json::json!({ "error": e.to_string() });
                    if let Err(e2) =
                        gate.audit_call("create_snapshot", Level::L1, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 创建配置快照失败：{e}");
                    vec![Response::SnapshotAck {
                        ok: false,
                        message: if *ui_en {
                    format!("snapshot failed: {e}")
                } else {
                    format!("创建快照失败：{e}")
                },
                        notes: Vec::new(),
                    }]
                }
            }
        }
        Request::RestoreSnapshot { id } => {
            if !*is_ui {
                audit_or_warn(gate, "restore_snapshot", Level::L2, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "rolling back requires a registered UI channel".into(),
                }];
            }
            // **回滚改的是系统配置**：按 L2 记审计（不改闸门流程，因为它不是 AI 工具调用，
            // 但审计里必须留下"谁在什么时候回滚到哪个点"）。
            let root = std::path::PathBuf::from(crate::snapshots::ROOT);
            let args = serde_json::json!({ "id": &id });
            match crate::snapshots::restore(&root, &id) {
                Ok(notes) => {
                    if let Err(e) =
                        gate.audit_call("restore_snapshot", Level::L2, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 已回滚到快照 {id}（{} 个文件）", notes.len());
                    vec![Response::SnapshotAck {
                        ok: true,
                        message: if *ui_en {
                    format!("rolled back to config snapshot {id}")
                } else {
                    format!("已回滚到配置快照 {id}")
                },
                        notes,
                    }]
                }
                Err(e) => {
                    if let Err(e2) =
                        gate.audit_call("restore_snapshot", Level::L2, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 回滚失败：{id} —— {e}");
                    vec![Response::SnapshotAck {
                        ok: false,
                        message: if *ui_en {
                    format!("rollback failed: {e}")
                } else {
                    format!("回滚失败：{e}")
                },
                        notes: Vec::new(),
                    }]
                }
            }
        }
        Request::SyncTime => {
            if !*is_ui {
                audit_or_warn(gate, "sync_time", Level::L1, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "time sync requires a registered UI channel".into(),
                }];
            }
            match crate::timesync::query().and_then(|t| {
                let (now_s, now_ns) = crate::timesync::now_unix();
                // 偏移 = 服务器时间 - 本机时间
                let offset_ms = (t.unix_secs - now_s) * 1000
                    + (t.nanos as i64 - now_ns as i64) / 1_000_000;
                crate::timesync::apply_offset(offset_ms).map(|how| (offset_ms, how))
            }) {
                Ok((offset_ms, how)) => {
                    // 偏移量总是记下来（**它是有用的信息**：用户该看到服务器报了多少）
                    LAST_OFFSET_MS.store(offset_ms, std::sync::atomic::Ordering::Relaxed);
                    let what = match how {
        crate::timesync::Adjustment::Refused => {
            // **说清是"拒绝"不是"出错"**：用户需要知道钟没动，以及为什么。
            format!(
                "偏移 {offset_ms} 毫秒超出合理性上限（{} 小时），已拒绝调整系统时钟 —— \
                 如果确实相差这么多，请手动设置时间",
                crate::timesync::MAX_OFFSET_MS / 3_600_000
            )
        }
                        crate::timesync::Adjustment::None => {
                            "已在误差范围内，未调整".to_string()
                        }
                        crate::timesync::Adjustment::Slew => "已平滑调整（不跳变）".to_string(),
                        crate::timesync::Adjustment::Step => "已直接校正（偏移较大）".to_string(),
                    };
                    let args = serde_json::json!({ "offset_ms": offset_ms });
                    if let Err(e) =
                        gate.audit_call("sync_time", Level::L1, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    // **被拒绝 ≠ 同步过**：不更新"上次同步时间"，
                    // 否则界面会说"刚刚同步过"，而时钟其实一个字节都没动。
                    if how != crate::timesync::Adjustment::Refused {
                        LAST_SYNC_UNIX.store(
                            crate::timesync::now_unix().0,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    eprintln!("[aetherd] 对时：偏移 {offset_ms} ms（{what}）");
                    vec![Response::TimeSync {
                        // 拒绝要让界面按"没成功"呈现（理由在 message 里）
                        ok: how != crate::timesync::Adjustment::Refused,
                        message: if *ui_en {
                    format!("offset {offset_ms} ms · {what}")
                } else {
                    format!("偏移 {offset_ms} ms · {what}")
                },
                        offset_ms,
                    }]
                }
                Err(e) => {
                    let args = serde_json::json!({ "error": e });
                    if let Err(e2) =
                        gate.audit_call("sync_time", Level::L1, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 对时失败：{e}");
                    vec![Response::TimeSync {
                        ok: false,
                        message: e,
                        offset_ms: 0,
                    }]
                }
            }
        }
        Request::SetTimezone { name } => {
            if !*is_ui {
                audit_or_warn(gate, "set_timezone", Level::L1, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "writing the system time zone requires a registered UI channel".into(),
                }];
            }
            let args = serde_json::json!({ "name": &name });
            match crate::timesync::set_timezone(&name) {
                Ok(path) => {
                    if let Err(e) =
                        gate.audit_call("set_timezone", Level::L1, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 系统时区已写入：{name}（{}）", path.display());
                    vec![Response::SettingAck {
                        key: "timezone".into(),
                        ok: true,
                        message: if *ui_en {
                    format!("system time zone set to {name}")
                } else {
                    format!("系统时区已设为 {name}")
                },
                    }]
                }
                Err(e) => {
                    if let Err(e2) =
                        gate.audit_call("set_timezone", Level::L1, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 写系统时区失败：{name} —— {e}");
                    vec![Response::SettingAck {
                        key: "timezone".into(),
                        ok: false,
                        message: if *ui_en {
                    format!("failed: {e}")
                } else {
                    format!("失败：{e}")
                },
                    }]
                }
            }
        }
        Request::GetTimeStatus => {
            if !*is_ui {
                audit_or_warn(gate, "get_time_status", Level::L0, verdict::REJECTED_NO_UI);
                return vec![Response::Error {
                    code: 403,
                    message: "reading time status requires a registered UI channel".into(),
                }];
            }
            let timezone = std::fs::read_to_string(crate::timesync::TIMEZONE_FILE)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            vec![Response::TimeStatus {
                timezone,
                last_offset_ms: LAST_OFFSET_MS.load(std::sync::atomic::Ordering::Relaxed),
                last_sync_unix: LAST_SYNC_UNIX.load(std::sync::atomic::Ordering::Relaxed),
            }]
        }
        Request::GetPolicy => {
            // 策略决定"什么能自动执行"，读它就知道系统的安全边界长什么样（侦察信息）——
            // 与"写系统设置"同一门槛：必须是已注册的 UI 通道。
            if !*is_ui {
                audit_or_warn(gate, "get_policy", Level::L0, verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝读取权限策略：本连接未注册为 UI 通道");
                return vec![Response::Error {
                    code: 403,
                    message: "reading the permission policy requires a registered UI channel".into(),
                }];
            }
            let entries = crate::tools::policy_entries(&gate.policy_snapshot());
            eprintln!("[aetherd] 权限策略已读取（{} 个工具）", entries.len());
            vec![Response::PolicyList { entries }]
        }
        Request::SetToolPolicy { tool, policy } => {
            if !*is_ui {
                audit_or_warn(gate, "set_tool_policy", Level::L1, verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝写权限策略：本连接未注册为 UI 通道");
                return vec![Response::Error {
                    code: 403,
                    message: "writing the permission policy requires a registered UI channel".into(),
                }];
            }
            // **未知工具一律拒绝**：允许写不存在的名字会留下死配置 ——
            // 用户以为禁掉了某个东西，其实没有，而且他没有任何办法看出来。
            if !crate::tools::is_known_tool(&tool) {
                let args = serde_json::json!({ "tool": &tool, "policy": policy });
                if let Err(e) =
                    gate.audit_call("set_tool_policy", Level::L1, &args, verdict::FAILED)
                {
                    eprintln!("[aetherd] 审计日志写入失败: {e}");
                }
                eprintln!("[aetherd] 拒绝写权限策略：未知工具「{tool}」");
                return vec![Response::PolicyAck {
                    tool,
                    ok: false,
                    message: "unknown tool: the policy is not written (it would leave an invisible dead entry)".into(),
                }];
            }
            let local = crate::tools::policy_of(policy);
            // 审计里也记这个名字，而审计页会把 args 显示出来 → 跟着连接语言
            let args = serde_json::json!({ "tool": &tool, "policy": local.label(*ui_en) });
            let path = std::path::Path::new(crate::policy::POLICY_PATH);
            match gate.set_tool_policy(&tool, local, path) {
                Ok(()) => {
                    if let Err(e) =
                        gate.audit_call("set_tool_policy", Level::L1, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!(
                        "[aetherd] 权限策略已更新：{tool} → {}（已写入 {}）",
                        local.label(*ui_en),
                        path.display()
                    );
                    // 先拼消息再移动 `tool`（结构体字段的求值顺序会让 `tool` 先被移走）
                    let message = if *ui_en {
                    format!("{} set to 「{}」", tool, local.label(*ui_en))
                } else {
                    format!("{} 已设为「{}」", tool, local.label(*ui_en))
                };
                    vec![Response::PolicyAck {
                        tool,
                        ok: true,
                        message,
                    }]
                }
                Err(e) => {
                    // **写盘失败必须如实报**：显示"已保存"而其实没写下去，
                    // 是最难查的一类问题（重启后设置没了，用户只能怀疑自己）。
                    if let Err(e2) =
                        gate.audit_call("set_tool_policy", Level::L1, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 权限策略写入失败：{tool} —— {e}");
                    vec![Response::PolicyAck {
                        tool,
                        ok: false,
                        message: if *ui_en {
                    format!("write failed (setting not applied): {e}")
                } else {
                    format!("写入失败（设置未生效）：{e}")
                },
                    }]
                }
            }
        }
        Request::SetSetting { key, value } => {
            // 写系统设置与"服务控制"同一门槛：**必须**由已注册的 UI 通道发起。
            //
            // 为什么不开给 AI 工具：AI 的动作要经确认卡片（L3），而"改 DNS"的正确入口
            // 是用户在设置页里明确输入。**一条路径比两条更容易审** ——
            // 两条入口意味着两处鉴权、两处审计、两处可能忘记同步的校验。
            if !*is_ui {
                audit_or_warn(gate, "set_setting", Level::L2, verdict::REJECTED_NO_UI);
                eprintln!(
                    "[aetherd] 拒绝写设置 {}：本连接未注册为 UI 通道",
                    key.as_str()
                );
                return vec![Response::Error {
                    code: 403,
                    message: "writing system settings requires a registered UI channel".into(),
                }];
            }
            // 审计参数走 audit_call（Gate 内部统一脱敏）。DNS 地址不是秘密，
            // 但仍走同一条路径，避免"新增一处直接落盘"的口子。
            let args = serde_json::json!({ "key": key.as_str(), "value": value });
            match crate::netset::apply(key, &value, *ui_en) {
                Ok(message) => {
                    if let Err(e) =
                        gate.audit_call("set_setting", Level::L2, &args, verdict::ALLOWED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 设置已写入：{} —— {message}", key.as_str());
                    vec![Response::SettingAck {
                        key: key.as_str().to_string(),
                        ok: true,
                        message,
                    }]
                }
                Err(e) => {
                    // 失败（含校验不通过）也留痕：审计要能回答"有没有人试图改设置、被什么挡下"
                    if let Err(e2) =
                        gate.audit_call("set_setting", Level::L2, &args, verdict::FAILED)
                    {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 写设置被拒：{} —— {e}", key.as_str());
                    vec![Response::SettingAck {
                        key: key.as_str().to_string(),
                        ok: false,
                        message: e,
                    }]
                }
            }
        }
        // §3.7 最小切片：检测外设（**只读**）。
        // 与 `GetDisplays` / `ListRemovable` 同一性质：只报告"看到了什么"，
        // 不做任何动作 —— 蓝牙配对与打印作业都**不在本版范围**。
        Request::ListPeripherals => {
            if !*is_ui {
                return vec![Response::Error {
                    code: 403,
                    message: "listing peripherals requires a registered UI channel".into(),
                }];
            }
            let (adapters, printing) = crate::peripherals::list();
            let mut list: Vec<aether_ipc::PeripheralWire> = adapters
                .into_iter()
                .map(|a| aether_ipc::PeripheralWire {
                    kind: "bluetooth".into(),
                    name: a.name,
                    address: a.address,
                    queues: Vec::new(),
                    cups: false,
                })
                .collect();
            // 打印**总是报一行**：`cups: false` 就是"这台机器没有打印子系统"这个事实本身，
            // 让界面去推断"没有这一行 = 没检测到"更容易出错。
            let (cups, queues) = match printing {
                crate::peripherals::Printing::NoSubsystem => (false, Vec::new()),
                crate::peripherals::Printing::NoQueues => (true, Vec::new()),
                crate::peripherals::Printing::Queues(q) => (true, q),
            };
            list.push(aether_ipc::PeripheralWire {
                kind: "printing".into(),
                name: "CUPS".into(),
                address: None,
                queues,
                cups,
            });
            vec![Response::PeripheralList { list }]
        }
        // §2.3：列出人类账户（只读）。门槛与其它 UI 通道请求一致。
        Request::ListUsers => {
            if !*is_ui {
                return vec![Response::Error {
                    code: 403,
                    message: "listing accounts requires a registered UI channel".into(),
                }];
            }
            match crate::accounts::list_users() {
                Ok(list) => vec![Response::UserList { list }],
                Err(e) => vec![Response::Error { code: 1, message: e }],
            }
        }
        // §2.3：改口令。
        //
        // **旧口令的校验在合成器侧**（它有 `sha512crypt`，锁屏在用）：
        // 也就是"先证明你是这个人，再让这里写新口令"。
        // 这里只做两件事：本地策略校验、把新口令经 `chpasswd` 的 stdin 写下去。
        Request::SetPassword { user, new_password, en } => {
            if !*is_ui {
                return vec![Response::Error {
                    code: 403,
                    message: "changing a password requires a registered UI channel".into(),
                }];
            }
            // 策略先过（纯函数）：不合规就**不碰系统**，直接回执
            if let Err(problem) = crate::accounts::check_new_password(&new_password) {
                return vec![Response::PasswordAck {
                    ok: false,
                    message: password_problem_text(en, problem),
                }];
            }
            // 审计只记**事实**（用户 + 动作）。这里根本没有口令字段；
            // 而且 `tools::audit_args` 是白名单制、默认脱敏，是第二道保险。
            let args = serde_json::json!({ "user": user, "action": "set_password" });
            match crate::accounts::set_password(&user, &new_password) {
                Ok(()) => {
                    let _ = gate.audit_call("set_password", Level::L2, &args, verdict::ALLOWED);
                    // 日志里也**只有用户名**，没有口令
                    eprintln!("[aetherd] 口令已更新：{user}");
                    vec![Response::PasswordAck {
                        ok: true,
                        message: if en {
                            format!("password updated for {user}")
                        } else {
                            format!("已更新 {user} 的口令")
                        },
                    }]
                }
                Err(e) => {
                    let _ = gate.audit_call("set_password", Level::L2, &args, verdict::FAILED);
                    eprintln!("[aetherd] 口令更新失败：{user} —— {e}");
                    vec![Response::PasswordAck { ok: false, message: e }]
                }
            }
        }
        Request::ServiceControl { unit, action, en } => {
            // 服务控制是"改系统状态"，与剪贴板/配置重载同一门槛：**必须**由已注册的
            // UI 通道发起。此前这里是个桩（直接回 ok:false + "M3 就绪后生效"），
            // 于是合成器的「重启服务」永远失败且原因是一句陈旧占位文案。
            if !*is_ui {
                audit_or_warn(gate, "service_control", Level::L2, verdict::REJECTED_NO_UI);
                eprintln!("[aetherd] 拒绝服务控制：本连接未注册为 UI 通道");
                return vec![Response::Error {
                    code: 403,
                    message: if en {
                        "Service control must come from a registered UI channel".into()
                    } else {
                        "service control requires a registered UI channel".into()
                    },
                }];
            }
            // 审计参数走 audit_call（Gate 内部统一脱敏）：服务名与动作都是安全字段，
            // 但仍走同一条路径，避免"新增一处直接落盘"的口子。
            let args = serde_json::json!({ "unit": unit, "action": format!("{action:?}") });
            match crate::control_init_service(&unit, action, en) {
                Ok(message) => {
                    if let Err(e) = gate.audit_call("service_control", Level::L2, &args, verdict::ALLOWED) {
                        eprintln!("[aetherd] 审计日志写入失败: {e}");
                    }
                    eprintln!("[aetherd] 服务控制成功：{unit} {action:?} —— {message}");
                    vec![Response::ServiceAck { unit, ok: true, message }]
                }
                Err(e) => {
                    // 失败也留痕：审计要能回答"有没有人试图重启服务、结果如何"
                    if let Err(e2) = gate.audit_call("service_control", Level::L2, &args, verdict::FAILED) {
                        eprintln!("[aetherd] 审计日志写入失败: {e2}");
                    }
                    eprintln!("[aetherd] 服务控制失败：{unit} {action:?} —— {e}");
                    vec![Response::ServiceAck { unit, ok: false, message: format!("{e}") }]
                }
            }
        }
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
    en: bool,
    cfg: &Config,
    gate: &Gate,
    approvals: &Approvals,
    is_ui: bool,
) -> Vec<Response> {
    if let Some((reply, action)) = intent::try_handle(text, en) {
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
    // en 也一路传下去：回答里由 aetherd 自己写的那几句要跟着界面语言
    match crate::agent_run(cfg, gate, text, is_ui, en) {
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
                            "「{}」 needs user confirmation, but this connection is not a registered UI channel; no confirmation token can be issued",
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
                if en {
                "(no model available: run `aetherd config --help` in a terminal to set up local Ollama or a cloud API key)"
            } else {
                "（未检测到可用模型：在终端运行 `aetherd config --help` 配置本地 Ollama 或云端 API Key）"
            }
            } else {
                ""
            };
            vec![Response::Error {
                code: 503,
                message: if en {
                    format!("AI channel unavailable: {e}{hint}")
                } else {
                    format!("AI 通道不可用: {e}{hint}")
                },
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
        handle_request(line, &cfg, &gate, &approvals, "test-key", is_ui, &mut false)
    }

    /// 批次 J 回归：**直接下发的电源请求必须被拒**（哪怕连接已注册为 UI 通道）。
    ///
    /// `Request::SystemPower` 的合法方向是 aetherd → aether-init（确认之后转发）。
    /// 若客户端也能直接发给 aetherd 并生效，L3 的确认令牌就形同虚设 ——
    /// 这条测试钉住"只有一条路可走"。
    #[test]
    fn direct_power_request_is_rejected_even_from_ui_channel() {
        let mut is_ui = true;
        let out = handle_line(
            &aether_ipc::encode(&Request::SystemPower {
                action: aether_ipc::PowerAction::PowerOff,
            }),
            &mut is_ui,
        );
        assert!(
            matches!(&out[0], Response::Error { code: 403, .. }),
            "直接下发电源请求必须 403（否则绕过确认卡片）：{out:?}"
        );
        let Response::Error { message, .. } = &out[0] else { unreachable!() };
        assert!(
            message.contains("system_power"),
            "拒绝时要告诉调用方正确的路径：{message}"
        );
    }

    /// §2.3：口令不合规时**不碰系统**（策略在纯函数里先过），直接回执；文案跟着语言。
    ///
    /// 这条不需要 root 也不需要目标系统 —— 它验证的是"坏输入根本走不到 `chpasswd`"。
    #[test]
    fn set_password_rejects_bad_password_before_touching_the_system() {
        let mut is_ui = true;
        let out = handle_line(
            &aether_ipc::encode(&Request::SetPassword {
                user: "alice".into(),
                new_password: "short".into(), // 5 位 < 8
                en: false,
            }),
            &mut is_ui,
        );
        match &out[0] {
            Response::PasswordAck { ok, message } => {
                assert!(!ok, "不合规口令不该成功");
                assert!(message.contains("太短"), "中文界面应说明太短：{message}");
            }
            other => panic!("应是 PasswordAck，实得 {other:?}"),
        }

        // 同一个请求、`en: true` → 回执应当是英文（§3.5：语言是连接级状态）
        let mut is_ui = true;
        let out = handle_line(
            &aether_ipc::encode(&Request::SetPassword {
                user: "alice".into(),
                new_password: "short".into(),
                en: true,
            }),
            &mut is_ui,
        );
        match &out[0] {
            Response::PasswordAck { ok, message } => {
                assert!(!ok);
                assert!(message.contains("too short"), "英文界面应说明 too short：{message}");
                assert!(
                    !message.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                    "英文回执里不该有汉字：{message}"
                );
            }
            other => panic!("应是 PasswordAck，实得 {other:?}"),
        }
    }

    /// 批次 I 回归：服务控制**必须**由已注册的 UI 通道发起。
    ///
    /// 此前这里是个不校验身份的桩（谁连上都能调，且永远回 ok:false）——
    /// 现在真的会去改系统服务，所以身份门槛是安全属性，不是形式。
    #[test]
    fn service_control_requires_registered_ui_channel() {
        let mut not_ui = false;
        let out = handle_line(
            &aether_ipc::encode(&Request::ServiceControl {
                unit: "network".into(),
                action: aether_ipc::ServiceAction::Restart,
                en: false,
            }),
            &mut not_ui,
        );
        assert!(
            matches!(&out[0], Response::Error { code: 403, .. }),
            "未注册连接必须 403，实际：{out:?}"
        );
        assert!(!not_ui, "拒绝请求不应把连接升级为 UI 通道");
    }

    /// 批次 I 回归：服务控制**不再假装成功**。
    ///
    /// 关键断言是"那句陈旧占位文案不能再出现" —— 桩的实现会回
    /// `ok:false, "…将在 M3 aether-init 就绪后生效"`，而 aether-init 早就存在。
    /// 非 Linux（本机单测）走"没有 init 通道"的诚实失败；Linux CI 上若连不上
    /// init 也是失败路径。两种情况都要求：要么成功，要么给出**真实原因**。
    #[test]
    fn service_control_reports_real_result_not_placeholder() {
        let mut is_ui = true;
        let out = handle_line(
            &aether_ipc::encode(&Request::ServiceControl {
                unit: "network".into(),
                action: aether_ipc::ServiceAction::Status,
                en: false,
            }),
            &mut is_ui,
        );
        let Response::ServiceAck { ok, message, unit } = &out[0] else {
            panic!("期望 ServiceAck，实际：{out:?}");
        };
        assert_eq!(unit, "network", "ack 必须带回单元名");
        assert!(
            !message.contains("M3"),
            "不得再回那句陈旧占位文案（说明又变回桩了）：{message}"
        );
        if !*ok {
            assert!(!message.trim().is_empty(), "失败必须给出原因，不能是空话");
        }
    }

    /// I-10 回归：按行限长必须**精确** —— 恰好上限、上限+1、带不带换行四种组合。
    ///
    /// 这条此前零测试（对抗审查指出"限长是无测试的断言"）。边界错一个字节，
    /// 要么把合法长行拒掉，要么让超长行漏过。
    #[test]
    fn read_line_capped_boundaries() {
        const MAX: u64 = 16;
        let read = |s: &str| {
            let mut r = std::io::BufReader::new(std::io::Cursor::new(s.as_bytes().to_vec()));
            read_line_capped(&mut r, MAX)
        };

        // 恰好 MAX 字节且带换行 → 通过（不能误判为超长）
        let exact = "a".repeat(MAX as usize - 1) + "\n";
        assert_eq!(exact.len() as u64, MAX);
        assert!(read(&exact).is_ok(), "恰好 MAX 且带换行应通过");

        // MAX-1 字节带换行 → 通过
        let shorter = "a".repeat(MAX as usize - 2) + "\n";
        assert!(read(&shorter).is_ok());

        // MAX 字节但**没有换行**（被截断）→ 必须报错
        let truncated = "a".repeat(MAX as usize);
        let e = read(&truncated).expect_err("无换行的满额行必须报错");
        // §3.5：这条错误属协议/日志层，改成了中性英文。
        // 断言改成"说明上限**且带上限数字**"—— 数字才是行为事实，措辞不是。
        let msg = e.to_string();
        assert!(msg.contains("cap") && msg.contains("16"), "错误信息应说明上限与数字：{msg}");

        // MAX+1 字节带换行 → 换行落在上限之后 → 必须报错
        let over = "a".repeat(MAX as usize) + "\n";
        assert!(read(&over).is_err(), "超过 MAX 的行必须被拒");

        // 空输入（对端关闭）→ 空串，不报错
        assert_eq!(read("").unwrap(), "");
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
        crate::clipboard::set("keep", false).unwrap();
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
    /// M-17 残余：aetherd 用的是共享判据（`aether-ipc`），这里只做一次**接线检查** ——
    /// 判据本身的性质（只放行 root、未知一律拒绝）由 `aether-ipc` 的用例覆盖，
    /// 免得同一性质在两处各测一遍、日后分叉。
    #[test]
    fn ipc_peer_check_is_wired_to_the_shared_predicate() {
        // 用**纯核心**（跨平台）：`peer_uid_allowed` 是 Linux 便捷版，Windows 上没有
        assert!(aether_ipc::peer_uid_allowed_with(Some(0), 1000), "root 无条件放行");
        assert!(!aether_ipc::peer_uid_allowed_with(None, 0), "未知身份必须拒绝");
    }

    /// **整体审计第 3 遍的持久化**（附录 V）：访问控制表变成机器检查的不变量。
    ///
    /// 背景：附录 T 的"26/26 处理器合规"是一次性人工核对（脚本 + 人眼）。
    /// 这条测试把它固定下来 —— 以后**新加一个会改系统状态的处理器却忘了加 UI 门**，
    /// 会在这里当场变红，而不是等到下一次审计。
    ///
    /// **与 `request_variants_are_gated` 是互补的，不是重复**：
    /// 那条是**行为**测试（真的调 `handle_line` 断言 403），比这里更硬 ——
    /// 但它的用例是**手写清单**（`cases`），新增变体若没加进清单就不会红；
    /// 这条**自动枚举源码**里每个 `Request::X`，所以"新增变体却忘了归类/加门"
    /// 一定会在它这里暴露；此外它还检查**是否落审计**（那条不查）。
    ///
    /// 规则：
    /// 1. 每个 `Request::X` 分支必须在下面两张表之一里**显式出现**
    ///    （`READ_ONLY` 或被视为"改状态"）—— 不允许有"没归类"的分支，
    ///    因为新分支默认会被当成"改状态"来检查；
    /// 2. "改状态"的分支必须有 UI 通道判断（`is_ui`）
    ///    或"明确拒绝 + 落审计"（`audit_or_warn`）；
    /// 3. 只读分支也要求 UI 门（与本项目"读系统状态也要注册过的通道"约定一致），
    ///    但不要求审计。
    ///
    /// 豁免名单为什么这么短：`Ping`（连通性探针）与 `SysInfo`（自己机器的
    /// 概览，既不改状态也不含敏感信息）是客户端**未注册**时也能用的入口；
    /// `RegisterUi` 是"变成 UI"这一步本身。
    #[test]
    fn every_request_handler_is_gated_and_classified() {
        let src = include_str!("server.rs");
        // 只扫生产区域：本文件末尾就是测试模块，从这里截断
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };

        /// 未注册通道也能用的入口（**显式**列出，不靠名字前缀猜）
        const NO_GATE_NEEDED: &[&str] = &["Ping", "SysInfo", "RegisterUi"];
        /// 只读入口：要求 UI 门，但不要求审计
        const READ_ONLY: &[&str] = &[
            "Chat",
            "ClipboardGet",
            "GetDisplays",
            "GetSnapshots",
            "GetTimeStatus",
            "GetPolicy",
            "ListRemovable",
            "ListUpdates",
            // §2.3：列出人类账户 —— 只读（读 /etc/passwd 里 uid 落在 [1000,65534) 的项），
            // 不含口令哈希，也不改任何状态，所以不落审计。
            "ListUsers",
            // §3.7：检测外设 —— 只读（读 /sys/class/bluetooth 与 CUPS 的路径），
            // 不配对、不打印、不改任何状态。
            "ListPeripherals",
        ];

        // 收集 `Request::X` 分支及其正文
        let mut arms: Vec<(String, String)> = Vec::new();
        let lines: Vec<&str> = prod.lines().collect();
        let mut current: Option<(String, usize)> = None;
        for (i, l) in lines.iter().enumerate() {
            let trimmed = l.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Request::") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                if !name.is_empty() {
                    if let Some((n, start)) = current.take() {
                        arms.push((n, lines[start..i].join("\n")));
                    }
                    current = Some((name, i));
                }
            }
        }
        if let Some((n, start)) = current.take() {
            arms.push((n, lines[start..].join("\n")));
        }
        assert!(arms.len() >= 20, "只找到 {} 个处理器，解析可能失效", arms.len());

        let mut problems: Vec<String> = Vec::new();
        for (name, body) in &arms {
            let gated = body.contains("is_ui") || body.contains("audit_or_warn");
            let audited = body.contains("audit_call(") || body.contains("audit_or_warn(");
            let no_gate_ok = NO_GATE_NEEDED.contains(&name.as_str());
            let read_only = READ_ONLY.contains(&name.as_str());
            if !gated && !no_gate_ok {
                problems.push(format!(
                    "Request::{name} 会改系统状态却没有 UI 通道门（is_ui / audit_or_warn）"
                ));
            }
            if !read_only && !audited && !no_gate_ok {
                problems.push(format!(
                    "Request::{name} 改状态却没落审计（audit_call / audit_or_warn）—— \
                     若确属低影响且高频，请把它加进 READ_ONLY 并写明理由"
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "访问控制表出现缺口：\n{}",
            problems.join("\n")
        );
        // 反证：豁免名单必须真的存在，否则这条测试可能是"空转"
        for n in NO_GATE_NEEDED {
            assert!(
                arms.iter().any(|(a, _)| a == n),
                "豁免名单里的 {n} 已不存在 —— 名单该更新，否则它掩盖的是真缺口"
            );
        }
    }

}
