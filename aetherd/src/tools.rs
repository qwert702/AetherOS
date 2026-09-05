//! 工具系统：AI 操作系统的手。每个工具声明权限等级，
//! 执行前必须过 perm::Gate 并写审计日志。
//!
//! 安全设计：所有进程执行均为"罐头探针"——命令与参数全部是编译期
//! 常量，AI 只能在探针集合里选择，不存在用户输入进入命令行的路径。

use crate::perm::{Gate, Level, Verdict};
use anyhow::{bail, Result};
use serde_json::Value;

/// 工具定义。
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub level: Level,
    /// JSON Schema 风格的参数说明（喂给 LLM 的 tools 字段）
    pub parameters: Value,
    /// 执行体
    pub run: fn(&Value) -> Result<String>,
}

pub fn registry() -> Vec<Tool> {
    vec![
        Tool {
            name: "sys_info",
            description: "查询系统状态：内存、磁盘、服务列表（由 aether 系统服务提供）",
            level: Level::L0,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["cpu", "memory", "disk", "services"], "description": "查询范围"}
                },
                "required": []
            }),
            run: tool_sys_info,
        },
        Tool {
            name: "read_file",
            description: "读取一个文本文件的内容（只读）",
            level: Level::L0,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "文件绝对路径"}},
                "required": ["path"]
            }),
            run: tool_read_file,
        },
        Tool {
            name: "sys_probe",
            description: "运行一个预置的系统探针，返回只读信息（uname/disk/processes/whoami）",
            level: Level::L0,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "probe": {"type": "string", "enum": ["uname", "disk", "processes", "whoami"], "description": "探针名"}
                },
                "required": []
            }),
            run: tool_sys_probe,
        },
    ]
}

/// 执行工具：闸门裁决 → 审计 → 运行。需确认的操作未批准时返回
/// NEEDS_CONFIRMATION 语义错误，由上层翻译成 UI 确认卡片。
pub fn execute(gate: &Gate, name: &str, args: &Value, approved: bool) -> Result<String> {
    let Some(tool) = registry().into_iter().find(|t| t.name == name) else {
        bail!("未知工具: {name}");
    };
    let args_str = args.to_string();
    let verdict = gate.judge(tool.level, approved);
    let verdict_str = match &verdict {
        Verdict::Allowed => "allowed",
        Verdict::NeedsConfirmation => "needs_confirmation",
        Verdict::Denied(_) => "denied",
    };
    let _ = gate.audit(name, tool.level, &args_str, verdict_str);
    match verdict {
        Verdict::Allowed => (tool.run)(args),
        Verdict::NeedsConfirmation => {
            bail!("NEEDS_CONFIRMATION: 工具 {name} 需要 L{} 级用户确认", tool.level as u8)
        }
        Verdict::Denied(reason) => bail!("DENIED: {reason}"),
    }
}

fn tool_sys_info(args: &Value) -> Result<String> {
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("memory");
    match scope {
        "memory" => {
            #[cfg(target_os = "linux")]
            {
                let s = std::fs::read_to_string("/proc/meminfo")?;
                Ok(s.lines().take(3).collect::<Vec<_>>().join("\n"))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = scope;
                Ok("内存: 预览环境暂无 /proc，接入系统服务后填充（M3 aether-init）".into())
            }
        }
        "services" => Ok("运行中服务: aetherd, aether-compositor（M3 后由 aether-init 提供）".into()),
        other => Ok(format!("范围 {other} 的真实数据将在 M3/M5 接入系统服务后提供")),
    }
}

fn tool_read_file(args: &Value) -> Result<String> {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        bail!("缺少 path 参数");
    };
    let content = std::fs::read_to_string(path)?;
    let truncated: String = content.chars().take(4000).collect();
    Ok(truncated)
}

/// 罐头探针：命令与参数全部为编译期常量。
fn tool_sys_probe(args: &Value) -> Result<String> {
    let which = args.get("probe").and_then(|v| v.as_str()).unwrap_or("uname");
    let output = match which {
        "uname" => std::process::Command::new("uname").arg("-a").output(),
        "disk" => std::process::Command::new("df").arg("-h").output(),
        "processes" => std::process::Command::new("ps").arg("aux").output(),
        "whoami" => std::process::Command::new("whoami").output(),
        other => bail!("未知探针: {other}"),
    }
    .map_err(|e| anyhow::anyhow!("探针 {which} 运行失败: {e}"))?;
    let mut out = String::from_utf8_lossy(&output.stdout).to_string();
    let err = String::from_utf8_lossy(&output.stderr);
    if !err.is_empty() {
        out.push_str(&err);
    }
    Ok(out.chars().take(4000).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn gate() -> Gate {
        Gate::for_test()
    }

    #[test]
    fn unknown_tool_rejected() {
        assert!(execute(&gate(), "format_disk", &serde_json::json!({}), true).is_err());
    }

    #[test]
    fn l0_runs_without_approval() {
        let r = execute(&gate(), "sys_info", &serde_json::json!({"scope":"services"}), false);
        assert!(r.is_ok());
    }

    #[test]
    fn probe_runs_without_approval() {
        let r = execute(&gate(), "sys_probe", &serde_json::json!({"probe":"uname"}), false);
        assert!(r.is_ok(), "probe failed: {r:?}");
    }

    #[test]
    fn unknown_probe_rejected() {
        let r = execute(&gate(), "sys_probe", &serde_json::json!({"probe":"format_everything"}), true);
        let err = r.unwrap_err().to_string();
        assert!(err.contains("未知探针"));
    }
}
