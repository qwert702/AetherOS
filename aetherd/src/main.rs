//! aetherd — AetherOS AI 中枢。
//!
//! 形态：
//! - `aetherd chat "指令"` —— 单轮 agent：路由 → LLM → 工具调用循环 → 回答
//! - `aetherd serve`      —— 常驻 IPC 服务（127.0.0.1:7311，NDJSON），
//!   供 Shell 指令条调用；离线快速意图先行，LLM 兜底
//! - 所有工具执行经过权限闸门（docs/ai-permissions.md）+ 审计日志

mod apps;
mod intent;
mod llm;
mod clipboard;
mod modelcfg;
mod perm;
mod router;
mod server;
mod tools;
mod trash;

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

/// 组装运行时配置。**优先级：环境变量 > 配置文件 > 内置默认**（0.6）。
///
/// 环境变量优先是因为它更适合"临时覆盖"（调试、CI），配置文件适合
/// "装好就不再动"；两者都在时，显式的环境变量意图更强。
fn config_from_env() -> Config {
    let file = modelcfg::ModelConfig::load();
    // 环境变量 > 文件字段 > 默认值
    let pick = |env_key: &str, from_file: Option<&String>, default: &str| -> String {
        std::env::var(env_key)
            .ok()
            .or_else(|| from_file.cloned())
            .unwrap_or_else(|| default.to_string())
    };

    let local = llm::Endpoint {
        base_url: pick("AETHER_LOCAL_URL", file.local_url.as_ref(), "http://127.0.0.1:11434/v1"),
        api_key: "ollama".into(),
        model: pick("AETHER_LOCAL_MODEL", file.local_model.as_ref(), "qwen2.5:7b"),
    };
    // 云端端点：**有 Key 或 有 Base URL** 就算配了。
    //
    // 为什么要允许"只有地址"：自建网关（客户端 → 自己的服务器 → 各家 AI）的
    // 认证方式可能与上游不同，甚至内网免认证 —— 强制要 Key 会把这种部署挡在门外。
    // 空 Key 会发成 `Authorization: Bearer `，上游返回 401 时错误信息是清楚的，
    // 比"端点根本没启用、只说 AI 不可用"好排查得多。
    let cloud_key = std::env::var("AETHER_API_KEY").ok().or_else(|| file.api_key.clone());
    let cloud_base = std::env::var("AETHER_API_BASE").ok().or_else(|| file.cloud_base.clone());
    let cloud = if cloud_key.is_some() || cloud_base.is_some() {
        Some(llm::Endpoint {
            base_url: cloud_base.unwrap_or_else(|| "https://open.bigmodel.cn/api/paas/v4".into()),
            api_key: cloud_key.unwrap_or_default(),
            model: pick("AETHER_MODEL", file.cloud_model.as_ref(), "glm-4-flash"),
        })
    } else {
        None
    };
    let local_only = std::env::var("AETHER_LOCAL_ONLY")
        .map(|v| v == "1")
        .unwrap_or(file.local_only);
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
///
/// `trusted_channel`：本次对话是否来自**已注册的 UI 通道**。需要可信通道的工具
/// （剪贴板读写）在未注册连接上直接拒绝 —— 否则"让模型去调 clipboard_read"
/// 就成了绕过 `Request::ToolCall` 闸门的旁路（2026-10-02 审计 H-2）。
pub(crate) fn agent_run(
    cfg: &Config,
    gate: &Gate,
    user_text: &str,
    trusted_channel: bool,
) -> Result<AgentOutcome> {
    const MAX_ROUNDS: usize = 4;
    let local_ok = llm::local_available(&cfg.local.base_url);
    let mut ctx = tools::ToolCtx::default();
    // 记录每轮实际路由的通道：最终回答的 channel 随 ChatChunk 回传客户端
    let mut last_channel: Option<router::Channel> = None;
    // 上下文里是否已含敏感工具结果（如 read_file 读到的文件内容）。
    // 一旦置位，后续轮次强制本地——这是 P0-4b：读到的文件内容不得离开本机。
    let mut sensitive_context = false;
    let mut messages = vec![
        llm::Message { role: "system".into(), content: SYSTEM_PROMPT.into(), tool_calls: None, tool_call_id: None, name: None },
        llm::Message { role: "user".into(), content: user_text.into(), tool_calls: None, tool_call_id: None, name: None },
    ];

    for _round in 0..MAX_ROUNDS {
        // 每轮重新路由：任务特征随对话演进
        let task = router::Task {
            text: user_text,
            sensitive_context,
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
            // 需要可信通道的工具（剪贴板）：未注册连接发起的对话一律拒绝。
            // 仍要补一条 tool 消息 —— OpenAI 兼容协议要求每个 tool_call.id 都有对应结果，
            // 否则下一轮请求直接 400。
            if tools::requires_trusted_channel(&name) && !trusted_channel {
                eprintln!("[aetherd] 拒绝 {name}：本对话连接未注册为 UI 通道");
                messages.push(llm::Message {
                    role: "tool".into(),
                    content: format!(
                        "[工具错误] 「{name}」读写用户环境数据，需要已注册的 UI 通道；本次对话未注册，已拒绝"
                    ),
                    tool_calls: None,
                    tool_call_id: if id.is_empty() { None } else { Some(id) },
                    name: Some(name),
                });
                continue;
            }
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
            // 敏感工具（read_file）的输出即将进入 messages：从下一轮起强制本地，
            // 不再把读到的文件内容发往云端端点。
            if tools::is_sensitive_output(&name) {
                sensitive_context = true;
            }
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

/// 供 server 分发测试用的最小配置（不指向任何真实端点）。
#[cfg(test)]
pub(crate) fn test_config() -> Config {
    Config {
        local: llm::Endpoint {
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "test".into(),
            model: "test".into(),
        },
        cloud: None,
        local_only: true,
    }
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
            let out = agent_run(&cfg, &gate, &text, false).context("agent 运行失败")?;
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
            // 开机把已装应用挂进 PATH。
            //
            // 为什么放在这儿：`/usr` 在内存盘里，重启就还原，所以包装脚本必须每次开机重建
            // （见 apps.rs::link_all）；而 `/var/apps` 是 aether-init 挂上持久化分区之后才有的，
            // /init 里做不了这件事。aetherd 是应用目录的主人，由它来做最自然。
            #[cfg(target_os = "linux")]
            {
                let root = apps::default_root();
                match apps::link_all(&root, std::path::Path::new("/usr/local/bin")) {
                    Ok(names) if !names.is_empty() => eprintln!(
                        "[aetherd] 已挂载 {} 个应用到 /usr/local/bin：{}",
                        names.len(),
                        names.join(" ")
                    ),
                    Ok(_) => {}
                    // 挂载失败不该拦住服务启动：应用是附加能力，AI 中枢才是本体
                    Err(e) => eprintln!("[aetherd] 挂载已装应用失败（不影响服务启动）: {e}"),
                }
            }
            server::serve(config_from_env())?;
        }
        Some("config") => {
            run_config(&args[2..])?;
        }
        Some("app") => {
            run_app(&args[2..])?;
        }
        _ => {
            eprintln!("aetherd — AetherOS AI 中枢\n\n用法:\n  aetherd chat <指令>   单轮 agent 对话\n  aetherd serve         常驻 IPC 服务（127.0.0.1:{}）\n  aetherd config ...    模型配置（0.6，见 aetherd config --help）\n  aetherd app ...       已装应用的管理（见 aetherd app）\n\n环境变量（优先级高于配置文件）:\n  AETHER_API_KEY        云端 API Key（GLM 等 OpenAI 兼容端点）\n  AETHER_API_BASE       云端 Base URL（默认 GLM）\n  AETHER_MODEL          云端模型（默认 glm-4-flash）\n  AETHER_LOCAL_URL      本地 Ollama 地址（默认 127.0.0.1:11434/v1）\n  AETHER_LOCAL_ONLY     置 1 强制仅本地\n\n配置文件: {}", aether_ipc::DEFAULT_PORT, modelcfg::config_path().display());
        }
    }
    Ok(())
}

/// `aetherd app` —— 已装应用的列表 / 安装 / 卸载 / 挂进 PATH。
///
/// 为什么做成子命令而不是"只有图形界面"：镜像里没有包管理器，"装应用"必须有一个
/// 能被脚本调用、能在救援控制台里用的入口；Dock 和 AI 工具都只是它的外壳。
fn run_app(args: &[String]) -> Result<()> {
    let root = apps::default_root();
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let items = apps::list(&root);
            if items.is_empty() {
                println!("还没装任何应用。安装目录：{}", root.display());
                println!("安装：aetherd app install <包目录>");
                return Ok(());
            }
            println!("已装 {} 个应用（{}）：", items.len(), root.display());
            for a in items {
                println!(
                    "  {:<14} {:<18} {:<8} {:>7} KB  {}",
                    a.manifest.id,
                    a.manifest.name,
                    a.manifest.version,
                    a.bytes / 1024,
                    a.manifest.desc
                );
            }
            Ok(())
        }
        Some("install") => {
            let src = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("用法: aetherd app install <包目录>"))?;
            let (a, pre) = apps::install(&root, std::path::Path::new(src))?;
            println!(
                "已装「{}」（{}）→ {}",
                a.manifest.name,
                a.manifest.id,
                a.dir.display()
            );
            println!("大小 {} KB", a.bytes / 1024);
            println!("预检：{}", pre.detail);
            if pre.kind == apps::PreflightKind::Risky {
                println!("注意：预检不通过也装上了，但很可能跑不起来 —— 上面写了缺什么。");
            }
            println!("（新开一个 shell，或跑 `aetherd app link`，就能直接敲 `{}`）", a.manifest.id);
            Ok(())
        }
        Some("remove") => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("用法: aetherd app remove <id>"))?;
            let trashed = apps::remove(&root, id)?;
            println!("已卸载「{id}」，移入回收站：{}", trashed.display());
            println!("（终端里的命令下次开机消失；想立刻清掉就跑 `aetherd app link`）");
            Ok(())
        }
        Some("link") => {
            let bindir = args
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/usr/local/bin"));
            let names = apps::link_all(&root, &bindir)?;
            if names.is_empty() {
                println!("没有已装应用，{} 已清理干净", bindir.display());
            } else {
                println!("已挂载 {} 个应用到 {}：{}", names.len(), bindir.display(), names.join(" "));
            }
            Ok(())
        }
        Some(other) => {
            eprintln!(
                "aetherd app —— 应用管理（见 aetherd/src/apps.rs）\n\n用法:\n  \
                 aetherd app list                列出已装应用\n  \
                 aetherd app install <包目录>    安装（目录里要有 app.json）\n  \
                 aetherd app remove <id>         卸载（进回收站，不是直接删）\n  \
                 aetherd app link [目录]         把已装应用挂进 PATH（/init 开机调用）\n\n\
                 安装目录: {}（可用 AETHER_APPS_DIR 覆盖）\n\
                 包格式: 一个目录，含 app.json 与 entry 指向的可执行文件；可选 lib/ 放自带共享库。",
                apps::default_root().display()
            );
            anyhow::bail!("未知子命令: {other}")
        }
    }
}

fn print_config_help() {
    println!(
        "aetherd config —— 模型配置（0.6）\n\n\
         用法:\n  \
         aetherd config --show                                       查看配置（Key 脱敏）\n  \
         aetherd config --local-url URL [--local-model M]            配置本地模型（Ollama）\n  \
         aetherd config --api-key KEY [--cloud-base URL] [--cloud-model M]   配置云端\n  \
         aetherd config --local-only 1|0                             强制仅本地 / 取消\n  \
         aetherd config --clear                                      清空配置\n\n\
         优先级：环境变量 > 配置文件 > 内置默认。\n\
         配置文件：{}",
        modelcfg::config_path().display()
    );
}

/// `aetherd config` —— 模型配置的读写（0.6）。
///
/// 用**参数式**而不是交互式问答：可脚本化、可测试，且在无 TTY 的环境
/// （init 脚本、远程管道）里也能用。
fn run_config(args: &[String]) -> Result<()> {
    let mut cfg = modelcfg::ModelConfig::load();
    let mut changed = false;
    let mut show = args.is_empty();

    let mut i = 0;
    while i < args.len() {
        let val = |k: usize| args.get(k + 1).cloned();
        match args[i].as_str() {
            "--show" => show = true,
            "--clear" => {
                cfg = modelcfg::ModelConfig::default();
                changed = true;
            }
            "--local-url" => {
                cfg.local_url = val(i);
                changed = true;
                i += 1;
            }
            "--local-model" => {
                cfg.local_model = val(i);
                changed = true;
                i += 1;
            }
            "--cloud-base" => {
                cfg.cloud_base = val(i);
                changed = true;
                i += 1;
            }
            "--cloud-model" => {
                cfg.cloud_model = val(i);
                changed = true;
                i += 1;
            }
            "--api-key" => {
                cfg.api_key = val(i);
                changed = true;
                i += 1;
            }
            "--local-only" => {
                cfg.local_only = matches!(val(i).as_deref(), Some("1") | Some("true") | Some("yes"));
                changed = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_config_help();
                return Ok(());
            }
            other => {
                eprintln!("未知参数: {other}\n");
                print_config_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }

    if changed {
        let path = cfg.save()?;
        println!("配置已写入 {}", path.display());
        #[cfg(unix)]
        println!("（Unix 下已收紧为 0600 —— 文件含 API Key）");
    }
    if show || !changed {
        println!("{}", serde_json::to_string_pretty(&cfg.summary())?);
    }
    if changed {
        // 不做热替换：让"配置生效"这条路径只有一条 —— 重启后的状态必然与
        // 配置文件一致，不会出现"改了但只改了一半"。
        println!(
            "\n提示：重启 aetherd 后生效。\n\
             若它正由 aether-init 托管，可在合成器里触发重载，或直接 reboot。"
        );
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
