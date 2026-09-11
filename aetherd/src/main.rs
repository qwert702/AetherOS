//! aetherd — AetherOS AI 中枢。
//!
//! 形态：
//! - `aetherd chat "指令"` —— 单轮 agent：路由 → LLM → 工具调用循环 → 回答
//! - `aetherd serve`      —— 常驻 IPC 服务（127.0.0.1:7311，NDJSON），
//!     供 Shell 指令条调用；离线快速意图先行，LLM 兜底
//! - 所有工具执行经过权限闸门（docs/ai-permissions.md）+ 审计日志

mod intent;
mod llm;
mod perm;
mod router;
mod server;
mod tools;

use anyhow::{Context, Result};
use perm::Gate;
use std::path::PathBuf;

const SYSTEM_PROMPT: &str = "你是 Aether，AetherOS 操作系统的 AI 中枢。你可以调用工具查询和操作这台电脑：\
用 desktop 工具切换窗口布局、打开应用、关闭窗口；用 sys_probe/sys_info 了解系统状态。\
回答使用用户的语言，简洁、可执行。用户说'整理桌面'时调用 desktop 的 layout_set two_col。";

#[derive(Clone)]
struct Config {
    local: llm::Endpoint,
    cloud: Option<llm::Endpoint>,
    local_only: bool,
}

fn config_from_env() -> Config {
    let local = llm::Endpoint {
        base_url: std::env::var("AETHER_LOCAL_URL").unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into()),
        api_key: "ollama".into(),
        model: std::env::var("AETHER_LOCAL_MODEL").unwrap_or_else(|_| "qwen2.5:7b".into()),
    };
    let cloud = std::env::var("AETHER_API_KEY").ok().map(|key| llm::Endpoint {
        base_url: std::env::var("AETHER_API_BASE")
            .unwrap_or_else(|_| "https://open.bigmodel.cn/api/paas/v4".into()),
        api_key: key,
        model: std::env::var("AETHER_MODEL").unwrap_or_else(|_| "glm-4-flash".into()),
    });
    let local_only = std::env::var("AETHER_LOCAL_ONLY").map(|v| v == "1").unwrap_or(false);
    Config { local, cloud, local_only }
}

/// OpenAI tools 数组（由工具注册表生成）。
fn tools_json() -> serde_json::Value {
    let fns: Vec<_> = tools::registry()
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect();
    serde_json::Value::Array(fns)
}

/// Agent 主循环：最多 MAX_ROUNDS 轮工具调用。
/// 返回最终回答 + LLM 产生的桌面行为队列（经 IPC Action 下发合成器）。
pub(crate) fn agent_run(cfg: &Config, gate: &Gate, user_text: &str) -> Result<(String, Vec<crate::intent::DesktopAction>)> {
    const MAX_ROUNDS: usize = 4;
    let local_ok = llm::local_available(&cfg.local.base_url);
    let mut ctx = tools::ToolCtx::default();
    let mut messages = vec![
        llm::Message { role: "system".into(), content: SYSTEM_PROMPT.into(), tool_calls: None, tool_call_id: None, name: None },
        llm::Message { role: "user".into(), content: user_text.into(), tool_calls: None, tool_call_id: None, name: None },
    ];

    for _round in 0..MAX_ROUNDS {
        // 每轮重新路由：任务特征随对话演进
        let task = router::Task {
            text: user_text,
            history_len: messages.len(),
            local_only: cfg.local_only,
            local_available: local_ok,
            cloud_available: cfg.cloud.is_some(),
        };
        let channel = router::route(&task);
        let endpoint = match (channel, &cfg.cloud) {
            (router::Channel::Cloud, Some(cloud)) => cloud,
            _ => {
                if channel == router::Channel::Local && !local_ok {
                    eprintln!("[aetherd] 本地模型不可达（{}）", cfg.local.base_url);
                }
                &cfg.local
            }
        };
        eprintln!("[aetherd] 通道: {:?} · 模型: {}", channel, endpoint.model);

        let resp = llm::complete(endpoint, &messages, Some(&tools_json()))?;

        // 无工具调用 → 最终回答
        let Some(calls) = resp.tool_calls.clone() else {
            return Ok((resp.content, std::mem::take(&mut ctx.desktop_actions)));
        };

        // 有工具调用：逐个执行（目前处理第一个，多并行调用在后续版本支持）
        messages.push(resp);
        let Some(call) = calls.get(0) else { break };
        let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = call
            .pointer("/function/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let raw_args = call.pointer("/function/arguments").and_then(|v| v.as_str()).unwrap_or("{}");
        let args: serde_json::Value = serde_json::from_str(raw_args).unwrap_or(serde_json::json!({}));

        eprintln!("[aetherd] 工具调用: {name} {args}");
        // 敏感工具（L2+）需用户确认：当前无 UI 通道，返回确认语义错误让模型向用户解释
        let result = tools::execute(gate, &mut ctx, &name, &args, false).unwrap_or_else(|e| {
            let msg = e.to_string();
            if msg.contains("NEEDS_CONFIRMATION") {
                format!("[需用户确认后重试] {msg}")
            } else {
                format!("[工具错误] {msg}")
            }
        });
        messages.push(llm::Message {
            role: "tool".into(),
            content: result,
            tool_calls: None,
            tool_call_id: if id.is_empty() { None } else { Some(id) },
            name: Some(name),
        });
    }
    Ok(("（已达本轮工具调用上限，请拆分任务）".into(), Vec::new()))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("chat") => {
            let text = args[2..].join(" ");
            if text.is_empty() {
                eprintln!("用法: aetherd chat <指令>");
                std::process::exit(2);
            }
            let cfg = config_from_env();
            let gate = Gate::new(PathBuf::from("aether-audit.log"));
            let (answer, _actions) = agent_run(&cfg, &gate, &text).context("agent 运行失败")?;
            println!("{answer}");
        }
        Some("serve") => {
            server::serve(config_from_env())?;
        }
        _ => {
            eprintln!("aetherd — AetherOS AI 中枢\n\n用法:\n  aetherd chat <指令>   单轮 agent 对话\n  aetherd serve         常驻 IPC 服务（127.0.0.1:{}）\n\n环境变量:\n  AETHER_API_KEY        云端 API Key（GLM 等 OpenAI 兼容端点）\n  AETHER_API_BASE       云端 Base URL（默认 GLM）\n  AETHER_MODEL          云端模型（默认 glm-4-flash）\n  AETHER_LOCAL_URL      本地 Ollama 地址（默认 127.0.0.1:11434/v1）\n  AETHER_LOCAL_ONLY     置 1 强制仅本地", aether_ipc::DEFAULT_PORT);
        }
    }
    Ok(())
}

/// 快速意图用：当前系统状态一句话摘要（离线通道的"状态"答复）。
pub fn sys_brief() -> String {
    "运行中: aetherd（AI 中枢）、aether-compositor（桌面）。内存/磁盘的实时数据将在 M3 接入 aether-init 后提供。".into()
}

/// 快速意图用：HH:MM 时钟（预览期按北京时间简化处理）。
pub fn text_clock() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + 8 * 3600;
    format!("{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60)
}
