//! aetherd serve：常驻 IPC 服务（TCP + aether-ipc NDJSON 协议）。
//!
//! 每连接一线程；Chat 请求先走离线快速意图，未命中再进 LLM agent；
//! 一个 Chat 请求可产生多条响应（ChatChunk + Action）。

use crate::perm::verdict;
use crate::{intent, perm::{Approvals, Gate, Level}, tools, Config};
use aether_ipc::{encode, Request, Response, SysReport};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 单条请求行的长度上限（NDJSON 帧协议，正常请求远小于此值）。
const MAX_LINE_BYTES: u64 = 1024 * 1024;
/// 同时服务的连接数上限：超出后立即拒绝，防资源耗尽。
const MAX_CONNECTIONS: usize = 32;
/// 单连接空闲读超时：半开连接不永久占用线程。
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// UI 通道密钥的落盘位置（与审计日志同目录，随持久化分区跨重启保留）。
const UI_KEY_PATH: &str = "/var/log/aether/ui.key";

pub fn serve(cfg: Config) -> anyhow::Result<()> {
    // 只听回环；宿主调试需要 hostfwd 直连时，由调试脚本显式注入 AETHER_BIND=0.0.0.0，
    // 出厂镜像（platform/overlay/init）不设置该变量
    let bind = std::env::var("AETHER_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let listener = TcpListener::bind((bind.as_str(), aether_ipc::DEFAULT_PORT))?;
    eprintln!(
        "[aetherd] IPC 服务已启动: {bind}:{} · 云端: {}",
        aether_ipc::DEFAULT_PORT,
        cfg.cloud.is_some()
    );
    let cfg = Arc::new(cfg);
    let gate = Arc::new(Gate::new(PathBuf::from("/var/log/aether/aether-audit.log")));
    // 一次性确认令牌表：跨连接共享（确认请求与重发可能来自不同连接）
    let approvals = Arc::new(Approvals::new());
    // UI 通道密钥（P1-8）：确认令牌只有"已注册的 UI 通道"能兑现
    let ui_key = Arc::new(resolve_ui_key());
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CONNECTIONS {
            eprintln!("[aetherd] 连接数已达上限（{MAX_CONNECTIONS}），拒绝新连接");
            continue;
        }
        let cfg = cfg.clone();
        let gate = gate.clone();
        let approvals = approvals.clone();
        let ui_key = ui_key.clone();
        let active = active.clone();
        active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::thread::spawn(move || {
            let _guard = ActiveGuard(&active);
            if let Err(e) = handle_conn(stream, &cfg, &gate, &approvals, ui_key.as_str()) {
                eprintln!("[aetherd] 连接处理结束: {e}");
            }
        });
    }
    Ok(())
}

/// 解析 UI 通道密钥（P1-8）。
///
/// 优先级：`AETHER_UI_KEY` 环境变量 > `<审计目录>/ui.key` > 生成并落盘。
/// 生成时在 Unix 下收紧为 0600。
///
/// 诚实边界：这挡不住**能读到该文件**的本机进程。但它把"谁能兑现确认令牌"
/// 从"任何能连 7311 的进程"收窄到"能读到密钥的进程"，并让"确认方"成为
/// 服务端可以断言的事实——这正是原实现完全缺失的那一环。
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

fn handle_conn(
    stream: TcpStream,
    cfg: &Config,
    gate: &Gate,
    approvals: &Approvals,
    ui_key: &str,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT)).ok();
    // 注意：`take()` 作用在整个 reader 生命周期上，是**连接级累计**上限而非单行上限
    // （见代码审查 P2-17）。此处保留该行为以约束 Approvals 的内存增长。
    let mut reader = BufReader::new(stream.try_clone()?).take(MAX_LINE_BYTES);
    let mut writer = stream;
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
    match crate::agent_run(cfg, gate, text) {
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
        Err(e) => vec![Response::Error {
            code: 503,
            message: format!("AI 通道不可用: {e}"),
        }],
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
            (
                "tool_call(L3)",
                Request::ToolCall {
                    session_id: "gate-test".into(),
                    tool: "install_disk".into(),
                    arguments: serde_json::json!({"disk": "/dev/vda", "confirm": "/dev/vda"}),
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
}
