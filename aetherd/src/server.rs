//! aetherd serve：常驻 IPC 服务（TCP + aether-ipc NDJSON 协议）。
//!
//! 每连接一线程；Chat 请求先走离线快速意图，未命中再进 LLM agent；
//! 一个 Chat 请求可产生多条响应（ChatChunk + Action）。

use crate::{intent, perm::Gate, tools, Config};
use aether_ipc::{encode, Request, Response, SysReport};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

pub fn serve(cfg: Config) -> anyhow::Result<()> {
    // 默认只听回环；guest 内经 /init 设 AETHER_BIND=0.0.0.0 以支持宿主 hostfwd 调试
    let bind = std::env::var("AETHER_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let listener = TcpListener::bind((bind.as_str(), aether_ipc::DEFAULT_PORT))?;
    eprintln!(
        "[aetherd] IPC 服务已启动: {bind}:{} · 云端: {}",
        aether_ipc::DEFAULT_PORT,
        cfg.cloud.is_some()
    );
    let cfg = Arc::new(cfg);
    let gate = Arc::new(Gate::new(PathBuf::from("aether-audit.log")));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let cfg = cfg.clone();
        let gate = gate.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_conn(stream, &cfg, &gate) {
                eprintln!("[aetherd] 连接处理结束: {e}");
            }
        });
    }
    Ok(())
}

fn handle_conn(stream: TcpStream, cfg: &Config, gate: &Gate) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
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
            let mut ctx = tools::ToolCtx::default();
            let output = tools::execute(gate, &mut ctx, &tool, &arguments, false)
                .unwrap_or_else(|e| format!("[工具错误] {e}"));
            let mut out = vec![Response::ToolResult { tool, ok: true, output }];
            for a in ctx.desktop_actions {
                out.push(Response::Action { name: a.name, arguments: a.arguments });
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
