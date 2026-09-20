//! aetherd — AetherOS AI 中枢。
//!
//! 形态：
//! - `aetherd chat "指令"` —— 单轮 agent：路由 → LLM → 工具调用循环 → 回答
//! - `aetherd serve`      —— 常驻 IPC 服务（127.0.0.1:7311，NDJSON），
//!   供 Shell 指令条调用；离线快速意图先行，LLM 兜底
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

/// 因需用户确认而暂停的工具调用（L2+）。
pub(crate) struct PendingConfirm {
    pub tool: String,
    pub level: u8,
    pub arguments: serde_json::Value,
    pub consequence: String,
    pub echo_required: Option<String>,
}

/// 一次 agent 运行的结果。
pub(crate) struct AgentOutcome {
    pub answer: String,
    pub actions: Vec<crate::intent::DesktopAction>,
    /// 有值表示本次因 L2+ 操作暂停，UI 确认后由 `ToolCall + approval` 重发执行
    pub pending: Option<PendingConfirm>,
    /// 本轮回答实际使用的推理通道（"local"/"cloud"），随 ChatChunk 回传客户端
    pub channel: Option<String>,
}

/// Agent 主循环：最多 MAX_ROUNDS 轮工具调用。
/// 返回最终回答 + LLM 产生的桌面行为队列（经 IPC Action 下发合成器）。
pub(crate) fn agent_run(cfg: &Config, gate: &Gate, user_text: &str) -> Result<AgentOutcome> {
    const MAX_ROUNDS: usize = 4;
    let local_ok = llm::local_available(&cfg.local.base_url);
    let mut ctx = tools::ToolCtx::default();
    // 记录每轮实际路由的通道：最终回答的 channel 随 ChatChunk 回传客户端
    let mut last_channel: Option<router::Channel> = None;
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
        last_channel = Some(channel);
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

        let resp = llm::complete(endpoint, &messages, Some(&tools_json()))
            .inspect_err(|e| eprintln!("[aetherd] ERROR LLM 请求失败（{}/{}）: {e}", endpoint.base_url, endpoint.model))?;

        // 无工具调用 → 最终回答
        let Some(raw_calls) = resp.tool_calls.clone() else {
            return Ok(AgentOutcome {
                answer: resp.content,
                actions: std::mem::take(&mut ctx.desktop_actions),
                pending: None,
                channel: last_channel.map(|c| c.label().to_string()),
            });
        };
        let calls = raw_calls.as_array().cloned().unwrap_or_default();

        // 空 tool_calls 数组：模型本轮没调用工具，content 即回答（与"轮次耗尽"区分开）
        if calls.is_empty() {
            let answer = if resp.content.trim().is_empty() { "（模型未返回有效回答）".into() } else { resp.content };
            return Ok(AgentOutcome {
                answer,
                actions: std::mem::take(&mut ctx.desktop_actions),
                pending: None,
                channel: last_channel.map(|c| c.label().to_string()),
            });
        }

        // 有工具调用：逐个执行，并为每个 tool_call 补一条结果消息。
        // OpenAI 兼容协议要求每个 tool_call.id 都有对应 tool 消息，
        // 缺失则下一轮请求直接 400。
        messages.push(resp);
        for call in &calls {
            let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = call
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let raw_args = call.pointer("/function/arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let args: serde_json::Value = serde_json::from_str(raw_args).unwrap_or(serde_json::json!({}));

            eprintln!("[aetherd] 工具调用: {name} {args}");
            // L2+ 工具需用户确认：中断本轮 agent，把结构化确认请求上抛给 UI。
            // 继续把"需确认"当普通工具结果喂回模型，只会让它编造一个"已执行"的回答。
            let result = match tools::execute(gate, &mut ctx, &name, &args, false) {
                Ok(tools::ExecOutcome::Done(out)) => out,
                Ok(tools::ExecOutcome::NeedsConfirmation { tool, level, arguments, consequence, echo_required }) => {
                    eprintln!("[aetherd] 工具 {tool} 需 L{} 确认，暂停并请求 UI 确认", level as u8);
                    return Ok(AgentOutcome {
                        answer: format!("「{tool}」需要你确认后才能执行。"),
                        actions: std::mem::take(&mut ctx.desktop_actions),
                        pending: Some(PendingConfirm {
                            tool,
                            level: level as u8,
                            arguments,
                            consequence: consequence.to_string(),
                            echo_required,
                        }),
                        channel: last_channel.map(|c| c.label().to_string()),
                    });
                }
                Err(e) => format!("[工具错误] {e}"),
            };
            messages.push(llm::Message {
                role: "tool".into(),
                content: result,
                tool_calls: None,
                tool_call_id: if id.is_empty() { None } else { Some(id) },
                name: Some(name),
            });
        }
    }
    Ok(AgentOutcome {
        answer: "（已达本轮工具调用上限，请拆分任务）".into(),
        actions: std::mem::take(&mut ctx.desktop_actions),
        pending: None,
        channel: last_channel.map(|c| c.label().to_string()),
    })
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
            let gate = Gate::new(PathBuf::from("/var/log/aether/aether-audit.log"));
            let out = agent_run(&cfg, &gate, &text).context("agent 运行失败")?;
            if let Some(p) = out.pending {
                // CLI 无 UI 通道：说明需确认及原因，退出码 3 供脚本区分
                eprintln!(
                    "需要用户确认：{} 需 L{} 级确认（{}）",
                    p.tool, p.level, p.consequence
                );
                std::process::exit(3);
            }
            println!("{}", out.answer);
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

/// 从 /proc/meminfo 文本解析 (已用MB, 总MB)。解析失败返回 None。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_mem_mb(meminfo: &str) -> Option<(u64, u64)> {
    let mut total = None;
    let mut avail = None;
    for line in meminfo.lines() {
        let mut it = line.split_whitespace();
        let key = it.next().unwrap_or("");
        let val: Option<u64> = it.next().and_then(|v| v.parse().ok());
        // /proc/meminfo 数值单位 kB
        match key {
            "MemTotal:" => total = val.map(|kv| kv / 1024),
            "MemAvailable:" => avail = val.map(|kv| kv / 1024),
            _ => {}
        }
    }
    match (total, avail) {
        (Some(t), Some(a)) if t >= a => Some((t - a, t)),
        _ => None,
    }
}

/// 从 /proc/uptime 文本解析运行秒数。解析失败返回 None。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_uptime_secs(uptime: &str) -> Option<u64> {
    uptime
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|f| f as u64)
}

/// 向 aether-init（Unix socket /run/aether-init.sock）查询服务状态列表。
#[cfg(target_os = "linux")]
fn query_init_services() -> anyhow::Result<Vec<aether_ipc::ServiceStatus>> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    let mut stream = UnixStream::connect(aether_init_socket())?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    stream.write_all(
        aether_ipc::encode(&aether_ipc::Request::SysInfo {
            scope: aether_ipc::SysInfoScope::Services,
        })
        .as_bytes(),
    )?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    match aether_ipc::decode::<aether_ipc::Response>(&line)? {
        aether_ipc::Response::SysInfo(r) => Ok(r.services),
        aether_ipc::Response::Error { message, .. } => Err(anyhow::anyhow!(message)),
        _ => Err(anyhow::anyhow!("aether-init 返回了意外响应")),
    }
}

/// aether-init 服务控制通道路径（与 aether-init/ipc.rs 保持一致）。
#[cfg(target_os = "linux")]
fn aether_init_socket() -> &'static str {
    "/run/aether-init.sock"
}

/// 汇总真实系统状态：/proc 内存/运行时长 + aether-init 服务列表。
/// 拿不到的部分保持 None/空（调用方按缺省展示）。非 Linux（单测）返回空报告。
pub(crate) fn collect_sys_report() -> aether_ipc::SysReport {
    #[cfg(target_os = "linux")]
    {
        let mem = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| parse_mem_mb(&s));
        let uptime = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| parse_uptime_secs(&s));
        let services = query_init_services().unwrap_or_default();
        aether_ipc::SysReport {
            cpu_percent: None,
            mem_used_mb: mem.map(|(u, _)| u),
            mem_total_mb: mem.map(|(_, t)| t),
            uptime_secs: uptime,
            services,
        }
    }
    #[cfg(not(target_os = "linux"))]
    aether_ipc::SysReport::default()
}

/// 快速意图用：当前系统状态一句话摘要（真实数据，离线可用）。
pub fn sys_brief() -> String {
    let r = collect_sys_report();
    if r.services.is_empty() && r.mem_total_mb.is_none() {
        return "系统状态暂时取不到（aether-init 未响应）。".into();
    }
    let mem = match (r.mem_used_mb, r.mem_total_mb) {
        (Some(u), Some(t)) => format!("内存 {u}/{t}MB"),
        _ => "内存未知".into(),
    };
    let up = r
        .uptime_secs
        .map(|s| format!("，已运行 {} 分钟", s / 60))
        .unwrap_or_default();
    let ok = r.services.iter().filter(|s| s.state == "Running").count();
    let detail = r
        .services
        .iter()
        .map(|s| {
            let mark = if s.state == "Running" { "✓" } else { "✗" };
            format!("{}{mark}", s.unit)
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{mem}{up}；{ok}/{} 服务运行中（{detail}）。", r.services.len())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_parse() {
        let s = "MemTotal:        524288 kB\nMemFree:          102400 kB\nMemAvailable:      40960 kB\n";
        assert_eq!(parse_mem_mb(s), Some((472, 512)));
    }

    #[test]
    fn meminfo_garbage_is_none() {
        assert_eq!(parse_mem_mb("hello world"), None);
        assert_eq!(parse_mem_mb(""), None);
    }

    #[test]
    fn uptime_parse() {
        assert_eq!(parse_uptime_secs("123.45 678.90"), Some(123));
        assert_eq!(parse_uptime_secs("garbage"), None);
    }

    #[test]
    fn sys_brief_degrades_gracefully() {
        // 非 Linux/拿不到数据时必须返回降级文案而非 panic
        let s = sys_brief();
        assert!(!s.is_empty());
    }
}
