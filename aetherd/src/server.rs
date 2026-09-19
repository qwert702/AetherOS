//! aetherd serve：常驻 IPC 服务（TCP + aether-ipc NDJSON 协议）。
//!
//! 每连接一线程；Chat 请求先走离线快速意图，未命中再进 LLM agent；
//! 一个 Chat 请求可产生多条响应（ChatChunk + Action）。

use crate::{intent, perm::Gate, tools, Config};
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
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CONNECTIONS {
            eprintln!("[aetherd] 连接数已达上限（{MAX_CONNECTIONS}），拒绝新连接");
            continue;
        }
        let cfg = cfg.clone();
        let gate = gate.clone();
        let active = active.clone();
        active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::thread::spawn(move || {
            let _guard = ActiveGuard(&active);
            if let Err(e) = handle_conn(stream, &cfg, &gate) {
                eprintln!("[aetherd] 连接处理结束: {e}");
            }
        });
    }
    Ok(())
}

/// 线程退出时递减活跃连接计数。
struct ActiveGuard<'a>(&'a Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn handle_conn(stream: TcpStream, cfg: &Config, gate: &Gate) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT)).ok();
    // take() 限制单次 read_line 的读取量：超长行不会撑爆内存
    let mut reader = BufReader::new(stream.try_clone()?).take(MAX_LINE_BYTES);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // 对端关闭
        }
        for resp in handle_request(&line, cfg, gate) {
            writer.write_all(encode(&resp).as_bytes())?;
            writer.flush()?;
        }
    }
}

fn handle_request(line: &str, cfg: &Config, gate: &Gate) -> Vec<Response> {
    let Ok(req) = aether_ipc::decode::<Request>(line) else {
        return vec![Response::Error { code: 1, message: "无法解析的请求".into() }];
    };
    match req {
        Request::Ping => vec![Response::Pong],
        Request::Chat { session_id, text } => handle_chat(&session_id, &text, cfg, gate),
        Request::ToolCall { tool, arguments, .. } => {
            // ToolCall 是本地 UI（安装向导等）在用户已确认后发起的直接调用，
            // 协议语义即"已授权的工具调用"，故视为 approved；AI agent 路径
            // （agent_run）则恒以 approved=false 过闸，L2+ 需用户确认。
            let mut ctx = tools::ToolCtx::default();
            let (ok, output) = match tools::execute(gate, &mut ctx, &tool, &arguments, true) {
                Ok(output) => (true, output),
                Err(e) => (false, format!("[工具错误] {e}")),
            };
            let mut out = vec![Response::ToolResult { tool, ok, output }];
            if ok {
                for a in ctx.desktop_actions {
                    out.push(Response::Action { name: a.name, arguments: a.arguments });
                }
            }
            out
        }
        Request::SysInfo { .. } => vec![Response::SysInfo(sys_report())],
        Request::ServiceControl { unit, action } => vec![Response::ServiceAck {
            unit,
            ok: false,
            message: format!("服务控制 {action:?} 将在 M3 aether-init 就绪后生效"),
        }],
    }
}

/// Chat 请求：快速意图 → 命中则回复+桌面行为；未命中 → LLM agent。
/// 注意：Action 必须在 done 前发送——客户端（compositor）收到 done 即停止读取。
fn handle_chat(session_id: &str, text: &str, cfg: &Config, gate: &Gate) -> Vec<Response> {
    if let Some((reply, action)) = intent::try_handle(text) {
        let mut out = Vec::new();
        if let Some(a) = action {
            out.push(Response::Action { name: a.name, arguments: a.arguments });
        }
        out.push(Response::ChatChunk {
            session_id: session_id.into(),
            delta: reply,
            done: true,
        });
        return out;
    }

    // 未命中快速意图：交给 LLM agent（可能较慢，连接线程阻塞在此处即可）
    match crate::agent_run(cfg, gate, text) {
        Ok((answer, actions)) => {
            let mut out = Vec::new();
            for a in actions {
                out.push(Response::Action { name: a.name, arguments: a.arguments });
            }
            out.push(Response::ChatChunk {
                session_id: session_id.into(),
                delta: answer,
                done: true,
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
