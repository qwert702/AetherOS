//! 工具系统：AI 操作系统的手。每个工具声明权限等级，
//! 执行前必须过 perm::Gate 并写审计日志。
//!
//! 安全设计：所有进程执行均为"罐头探针"——命令与参数全部是编译期
//! 常量，AI 只能在探针集合里选择，不存在用户输入进入命令行的路径。

use crate::intent::DesktopAction;
use crate::perm::{Gate, Level, Verdict};
use anyhow::{bail, Result};
use serde_json::Value;

/// 工具执行上下文：桌面类工具产生的行为指令在此累积，
/// 由 agent 循环结束后经 IPC Action 通道下发给 Shell/合成器执行。
#[derive(Default)]
pub struct ToolCtx {
    pub desktop_actions: Vec<DesktopAction>,
}

/// 工具定义。
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub level: Level,
    /// JSON Schema 风格的参数说明（喂给 LLM 的 tools 字段）
    pub parameters: Value,
    /// 执行体
    pub run: fn(&Value, &mut ToolCtx) -> Result<String>,
    /// 一句话后果说明（L2+ 确认卡片展示给用户；L0/L1 留空）
    pub consequence: &'static str,
    /// L3 回显确认：需原样输入的参数名（如 "disk"）。上抛确认请求时会被
    /// 解析成该参数的**实际值**（如 "/dev/vda"）——用户要回显的是目标本身。
    pub echo_field: Option<&'static str>,
    /// 输出是否含"用户数据"（文件内容）。
    ///
    /// 为 true 时，其结果一旦进入对话上下文，本轮推理**强制走本地通道**，
    /// 不再上云 —— 这是 P0-4b 的落地点：读到的文件内容不该离开本机。
    pub sensitive_output: bool,
}

pub fn registry() -> Vec<Tool> {
    vec![
        Tool {
            name: "clipboard_read",
            description: "读取系统剪贴板的当前内容（跨进程）。剪贴板里常出现密码/令牌，读取会被记录到审计日志，且读到的内容会强制后续推理留在本地。",
            level: Level::L1,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            run: tool_clipboard_read,
            consequence: "",
            echo_field: None,
            // 剪贴板 = 用户可能刚复制的密码：读到即视为敏感上下文（P0-4b 同一逻辑）
            sensitive_output: true,
        },
        Tool {
            name: "clipboard_write",
            description: "把一段文本写入系统剪贴板（跨进程），供用户粘贴到别处。",
            level: Level::L0,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "要放入剪贴板的文本（上限 64KB）"}
                },
                "required": ["text"]
            }),
            run: tool_clipboard_write,
            consequence: "",
            echo_field: None,
            sensitive_output: false,
        },

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
            consequence: "",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "read_file",
            description: "读取一个文本文件的内容（只读）。仅可读用户数据区（家目录、/tmp）与 aether 自身配置/日志；系统区（/etc 除 aether 外、/proc、/sys、/dev、/boot）一律拒绝。",
            level: Level::L0,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "文件绝对路径"}},
                "required": ["path"]
            }),
            run: tool_read_file,
            consequence: "",
            echo_field: None,
            // 文件内容属于用户数据：读到即强制本地通道，不上云
            sensitive_output: true,
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
            consequence: "",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "desktop",
            description: "操作桌面：切换窗口布局（layout_set）、打开应用窗口（open_app）、关闭当前活动窗口（close_active）",
            level: Level::L1,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["layout_set", "open_app", "close_active"],
                        "description": "桌面操作类型"
                    },
                    "layout": {
                        "type": "string",
                        "enum": ["two_col", "three_col", "monocle", "float"],
                        "description": "action=layout_set 时的目标布局"
                    },
                    "app": {
                        "type": "string",
                        "enum": ["文件", "终端", "浏览器", "音乐", "设置"],
                        "description": "action=open_app 时的应用名"
                    }
                },
                "required": ["action"]
            }),
            run: tool_desktop,
            consequence: "",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "install_disk",
            description: "把当前系统整盘安装到指定磁盘（isohybrid 镜像 dd 覆盖写，安装后从该盘引导）。整盘覆盖，目标盘数据将丢失！L3 危险操作：需用户确认，且 confirm 参数必须与 disk 完全一致。",
            level: Level::L3,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "disk": {"type": "string", "description": "目标块设备（如 /dev/vda、/dev/sda），整盘覆盖"},
                    "confirm": {"type": "string", "description": "回显确认：必须与 disk 完全相同，否则拒绝执行"}
                },
                "required": ["disk", "confirm"]
            }),
            run: tool_install_disk,
            consequence: "整盘覆盖写入：目标磁盘上的分区表与所有数据将被永久删除，不可恢复。",
            echo_field: Some("disk"),
            sensitive_output: false,
        },
    ]
}

/// 该工具的输出是否含用户数据（文件内容）。
///
/// agent 循环据此置位"敏感上下文"：其结果一旦进入 messages，
/// 后续轮次强制走本地通道（P0-4b）。
pub fn is_sensitive_output(name: &str) -> bool {
    registry().iter().any(|t| t.name == name && t.sensitive_output)
}

/// 工具执行结果。
#[derive(Debug)]
pub enum ExecOutcome {
    /// 已执行，附带输出文本（审计告警会前置在文本里）
    Done(String),
    /// L2+ 未获用户确认：由上层翻译成确认卡片（IPC `NeedsConfirmation`）。
    /// 不携带令牌——令牌由服务层签发，AI 路径拿不到，因此无法自我授权。
    NeedsConfirmation {
        tool: String,
        level: Level,
        arguments: Value,
        consequence: &'static str,
        /// L3 回显目标（参数实际值，运行时才知道，故为 String）
        echo_required: Option<String>,
    },
}

/// 执行工具：闸门裁决 → 审计 → 运行。
pub fn execute(gate: &Gate, ctx: &mut ToolCtx, name: &str, args: &Value, approved: bool) -> Result<ExecOutcome> {
    let Some(tool) = registry().into_iter().find(|t| t.name == name) else {
        bail!("未知工具: {name}");
    };
    let args_str = args.to_string();
    let verdict = gate.judge(name, tool.level, args, approved);
    let verdict_str = match &verdict {
        Verdict::Allowed => "allowed",
        Verdict::NeedsConfirmation => "needs_confirmation",
        Verdict::Denied(_) => "denied",
    };
    // 审计失败不阻断执行，但必须显式可见：错误随输出返回调用方，并落 stderr。
    let audit_warn = gate.audit(name, tool.level, &args_str, verdict_str).err().map(|e| {
        eprintln!("[aetherd] 审计日志写入失败: {e}");
        format!("⚠ 审计日志写入失败（{e}），本次操作未留痕\n")
    });
    match verdict {
        Verdict::Allowed => match (tool.run)(args, ctx) {
            Ok(out) => Ok(ExecOutcome::Done(match audit_warn {
                Some(w) => format!("{w}{out}"),
                None => out,
            })),
            Err(e) => {
                // 执行失败也留痕：否则"闸门放行"与"真的读到了"在审计里无法区分
                // （例如 read_file 通过闸门但被路径白名单拒绝，只记 allowed 会误导）
                if let Err(ae) = gate.audit(name, tool.level, &args_str, "failed") {
                    eprintln!("[aetherd] 审计日志写入失败: {ae}");
                }
                Err(e)
            }
        },
        Verdict::NeedsConfirmation => Ok(ExecOutcome::NeedsConfirmation {
            tool: name.to_string(),
            level: tool.level,
            arguments: args.clone(),
            consequence: tool.consequence,
            // 回显目标取参数实际值：用户要确认的是"/dev/vda"，不是字段名"disk"
            echo_required: tool
                .echo_field
                .and_then(|field| args.get(field))
                .and_then(|v| v.as_str())
                .map(String::from),
        }),
        Verdict::Denied(reason) => bail!("DENIED: {reason}"),
    }
}

fn tool_clipboard_read(_args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let text = crate::clipboard::get();
    if text.is_empty() {
        return Ok("（剪贴板为空）".into());
    }
    Ok(text)
}

fn tool_clipboard_write(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
        bail!("缺少参数 text");
    };
    let bytes = crate::clipboard::set(text).map_err(|e| anyhow::anyhow!(e))?;
    Ok(format!("已写入剪贴板（{bytes} 字节）"))
}

fn tool_sys_info(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
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

/// `read_file` 允许读取的根（P0-4a）。
///
/// 用**白名单**而不是黑名单：黑名单永远漏（符号链接、大小写、`..` 回绕、
/// `/proc/self/cwd` 之类的间接路径），而白名单配合 `canonicalize` 能保证
/// "拒绝的就是拒绝的"。代价是 AI 不再"无所不知"——它只能读用户数据区与
/// aether 自身的配置/日志，系统区（/etc 非 aether 部分、/proc、/sys、/dev、
/// /boot、/root）一律不可读。
#[cfg(target_os = "linux")]
const READ_ALLOWED_ROOTS: &[&str] = &[
    "/home",           // 用户数据
    "/etc/aether",     // aether 自身配置
    "/var/log/aether", // 审计日志
    "/run/aether",     // 运行时状态
    "/tmp",            // 临时文件
];

/// 开发机（Windows）上的对应白名单：用户数据区与临时目录。
#[cfg(not(target_os = "linux"))]
const READ_ALLOWED_ROOTS: &[&str] = &["C:/Users", "C:/Temp"];

/// 即使在白名单根之内，也拒绝的凭证类子路径（家目录下的私钥/令牌）。
const READ_DENY_SUBPATHS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    ".netrc",
    ".git-credentials",
];

/// 去掉 Windows canonicalize 产生的 `\\?\` 扩展长度前缀，
/// 否则白名单前缀永远匹配不上。
fn strip_verbatim_prefix(p: std::path::PathBuf) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return std::path::PathBuf::from(rest);
        }
    }
    p
}

/// 校验并规范化待读路径；不可读则返回带原因的 Err。
fn resolve_readable(path: &str) -> Result<std::path::PathBuf> {
    // canonicalize 同时解析 `..` 与符号链接——这两条绕行路径因此被堵死
    let canon = std::fs::canonicalize(path)
        .map_err(|e| anyhow::anyhow!("无法访问 {path}: {e}"))?;
    let canon = strip_verbatim_prefix(canon);

    // 1. 必须落在白名单根之内（Path::starts_with 按组件匹配，/homeevil ≠ /home）
    if !READ_ALLOWED_ROOTS.iter().any(|root| canon.starts_with(root)) {
        bail!(
            "路径不在允许读取的范围内（{path}）：read_file 仅可读用户数据区（家目录、/tmp）\
             与 aether 自身配置/日志；系统区与其它用户目录不可读"
        );
    }
    // 2. 白名单根之内的凭证类子路径仍然拒绝
    if is_credential_path(&canon) {
        bail!("拒绝读取凭证类路径（{path}）");
    }
    Ok(canon)
}

/// 规范化路径是否命中凭证类子路径（私钥、云凭证、令牌文件）。
fn is_credential_path(canon: &std::path::Path) -> bool {
    let lower = canon.to_string_lossy().replace('\\', "/").to_lowercase();
    READ_DENY_SUBPATHS.iter().any(|d| lower.contains(d))
}

fn tool_read_file(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        bail!("缺少 path 参数");
    };
    let canon = resolve_readable(path)?;
    let content = std::fs::read_to_string(&canon)?;
    let truncated: String = content.chars().take(4000).collect();
    Ok(truncated)
}

/// 罐头探针执行体：命令与参数全部为编译期常量。
fn tool_sys_probe(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
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

const KNOWN_APPS: &[&str] = &["文件", "终端", "浏览器", "音乐", "设置"];
const KNOWN_LAYOUTS: &[&str] = &["two_col", "three_col", "monocle", "float"];

/// 安装目标盘白名单校验：仅接受 /dev/<纯字母数字> 形式（如 /dev/vda、/dev/sda），
/// 拒绝子目录、..、通配符等一切花哨形式。防呆的主体仍是 aether-install 的
/// 块设备/容量校验与显式 --yes；这里挡住路径注入面。
fn valid_disk_path(disk: &str) -> bool {
    disk.starts_with("/dev/")
        && disk.len() > "/dev/".len()
        && disk[5..].chars().all(|c| c.is_ascii_alphanumeric())
}

/// 整盘安装工具：调 aether-install（其内部为罐头 dd，目标值仅作参数）。
/// L3 双重确认的第二道：confirm 参数必须回显目标盘设备名。
fn tool_install_disk(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(disk) = args.get("disk").and_then(|v| v.as_str()) else {
        bail!("缺少 disk 参数");
    };
    if !valid_disk_path(disk) {
        bail!("磁盘参数必须是 /dev/<盘符> 形式（如 /dev/vda），拒绝: {disk}");
    }
    let confirm = args.get("confirm").and_then(|v| v.as_str()).unwrap_or("");
    if confirm != disk {
        bail!(
            "整盘擦除需回显确认：请将 confirm 参数设为与 disk 完全相同（{disk}）以确认目标盘数据将丢失"
        );
    }
    let out = std::process::Command::new("/usr/bin/aether-install")
        .arg("--disk")
        .arg(disk)
        .arg("--yes")
        .output()
        .map_err(|e| anyhow::anyhow!("安装器运行失败: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("安装失败（{}）: {}", out.status, s.chars().take(400).collect::<String>());
    }
    Ok(s.chars().take(800).collect())
}

/// 桌面操作工具：LLM 操作桌面的手。参数经白名单校验后，
/// 以 DesktopAction 形式入队，由 agent 循环下发合成器执行。
fn tool_desktop(args: &Value, ctx: &mut ToolCtx) -> Result<String> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "layout_set" => {
            let layout = args.get("layout").and_then(|v| v.as_str()).unwrap_or("");
            if !KNOWN_LAYOUTS.contains(&layout) {
                bail!("未知布局: {layout}");
            }
            ctx.desktop_actions.push(DesktopAction {
                name: "layout_set".into(),
                arguments: serde_json::json!({ "layout": layout }),
            });
            Ok(format!("已切换桌面布局为 {layout}"))
        }
        "open_app" => {
            let app = args.get("app").and_then(|v| v.as_str()).unwrap_or("");
            if !KNOWN_APPS.contains(&app) {
                bail!("未知应用: {app}");
            }
            ctx.desktop_actions.push(DesktopAction {
                name: "open_app".into(),
                arguments: serde_json::json!({ "app": app }),
            });
            Ok(format!("已打开应用「{app}」"))
        }
        "close_active" => {
            ctx.desktop_actions.push(DesktopAction {
                name: "close_active".into(),
                arguments: serde_json::json!({}),
            });
            Ok("已关闭当前活动窗口".into())
        }
        other => bail!("未知桌面操作: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> Gate {
        Gate::for_test()
    }

    #[test]
    fn unknown_tool_rejected() {
        let mut ctx = ToolCtx::default();
        assert!(execute(&gate(), &mut ctx, "format_disk", &serde_json::json!({}), true).is_err());
    }

    #[test]
    fn l0_runs_without_approval() {
        let mut ctx = ToolCtx::default();
        let r = execute(&gate(), &mut ctx, "sys_info", &serde_json::json!({"scope":"services"}), false);
        assert!(matches!(r, Ok(ExecOutcome::Done(_))));
    }

    #[test]
    fn probe_runs_without_approval() {
        let mut ctx = ToolCtx::default();
        let r = execute(&gate(), &mut ctx, "sys_probe", &serde_json::json!({"probe":"uname"}), false);
        assert!(r.is_ok(), "probe failed: {r:?}");
    }

    #[test]
    fn unknown_probe_rejected() {
        let mut ctx = ToolCtx::default();
        let r = execute(&gate(), &mut ctx, "sys_probe", &serde_json::json!({"probe":"format_everything"}), true);
        let err = r.unwrap_err().to_string();
        assert!(err.contains("未知探针"));
    }

    #[test]
    fn desktop_layout_queued() {
        let mut ctx = ToolCtx::default();
        let r = execute(&gate(), &mut ctx, "desktop", &serde_json::json!({"action":"layout_set","layout":"two_col"}), false);
        assert!(r.is_ok());
        assert_eq!(ctx.desktop_actions.len(), 1);
        assert_eq!(ctx.desktop_actions[0].name, "layout_set");
        assert_eq!(ctx.desktop_actions[0].arguments["layout"], "two_col");
    }

    #[test]
    fn desktop_app_whitelist() {
        let mut ctx = ToolCtx::default();
        let ok = execute(&gate(), &mut ctx, "desktop", &serde_json::json!({"action":"open_app","app":"终端"}), false);
        assert!(ok.is_ok());
        assert_eq!(ctx.desktop_actions.len(), 1);
        // 白名单外的应用直接拒绝，不产生任何行为
        let bad = execute(&gate(), &mut ctx, "desktop", &serde_json::json!({"action":"open_app","app":"勒索软件"}), false);
        assert!(bad.is_err());
        assert_eq!(ctx.desktop_actions.len(), 1);
    }

    #[test]
    fn install_disk_rejects_path_injection() {
        let mut ctx = ToolCtx::default();
        for bad in ["/etc/passwd", "/dev/sda/../../x", "/dev/sda;rm", "/dev/", ""] {
            let r = execute(
                &gate(),
                &mut ctx,
                "install_disk",
                &serde_json::json!({"disk": bad, "confirm": bad}),
                true,
            );
            assert!(r.is_err(), "{bad} 应被拒绝");
        }
        // 正常路径形式通过校验层（真实写入只发生在有块设备的系统内）
        let good = execute(
            &gate(),
            &mut ctx,
            "install_disk",
            &serde_json::json!({"disk":"/dev/vda", "confirm":"/dev/vda"}),
            true,
        );
        if let Err(e) = good {
            assert!(!e.to_string().contains("拒绝"), "路径校验不应拦截 /dev/vda: {e}");
        }
    }

    #[test]
    fn install_disk_requires_confirm_echo() {
        let mut ctx = ToolCtx::default();
        // confirm 缺失或与 disk 不一致都拒绝
        for confirm in [serde_json::json!(""), serde_json::json!("/dev/sda"), serde_json::Value::Null] {
            let r = execute(
                &gate(),
                &mut ctx,
                "install_disk",
                &serde_json::json!({"disk":"/dev/vda", "confirm": confirm}),
                true,
            );
            let err = r.unwrap_err().to_string();
            assert!(err.contains("回显确认"), "confirm={confirm} 应触发回显确认: {err}");
        }
    }

    #[test]
    fn install_disk_needs_confirmation_without_approval() {
        // L3 危险操作：无用户批准时不得进入工具体（连路径校验都不该执行），
        // 而是上抛结构化确认请求，供 UI 弹确认卡片
        let mut ctx = ToolCtx::default();
        let r = execute(
            &gate(),
            &mut ctx,
            "install_disk",
            &serde_json::json!({"disk":"/dev/vda", "confirm":"/dev/vda"}),
            false,
        );
        match r {
            Ok(ExecOutcome::NeedsConfirmation { tool, level, echo_required, consequence, .. }) => {
                assert_eq!(tool, "install_disk");
                assert_eq!(level as u8, 3);
                // L3 必须要求回显目标盘名（实际值而非字段名），且给出后果说明
                assert_eq!(echo_required.as_deref(), Some("/dev/vda"));
                assert!(!consequence.is_empty(), "确认卡片需要后果说明");
            }
            other => panic!("L3 未批准应返回确认请求，实得: {other:?}"),
        }
    }

    /// P0-4a：白名单外的路径必须被**策略**拒绝，而不是"碰巧不存在"。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn read_file_rejects_paths_outside_whitelist_on_host() {
        let mut ctx = ToolCtx::default();
        // 这些文件在开发机上真实存在，属于系统区，必须拒绝
        for p in ["C:/Windows/win.ini", "C:/Windows/System32/drivers/etc/hosts"] {
            let r = execute(&gate(), &mut ctx, "read_file", &serde_json::json!({"path": p}), false);
            let err = r.expect_err(&format!("{p} 应被拒绝")).to_string();
            assert!(err.contains("不在允许读取的范围内"), "{p} 实得: {err}");
        }
    }

    /// 目标平台（Linux）上 aetherd 以 root 运行，这些路径此前全部可读。
    #[cfg(target_os = "linux")]
    #[test]
    fn read_file_rejects_system_paths_on_target() {
        let mut ctx = ToolCtx::default();
        for p in [
            "/etc/shadow",
            "/etc/passwd",
            "/proc/self/environ",
            "/sys/kernel/osrelease",
            "/boot/vmlinuz",
            "/root/.bashrc",
        ] {
            let r = execute(&gate(), &mut ctx, "read_file", &serde_json::json!({"path": p}), false);
            let err = r.expect_err(&format!("{p} 应被拒绝")).to_string();
            assert!(
                err.contains("不在允许读取的范围内") || err.contains("凭证类"),
                "{p} 实得: {err}"
            );
        }
    }

    /// 白名单内的路径应被**放行到文件系统层**（报"无法访问"而非"不在允许范围"）。
    #[test]
    fn read_file_allows_whitelisted_root() {
        let mut ctx = ToolCtx::default();
        #[cfg(not(target_os = "linux"))]
        let p = "C:/Users/__aether_probe_absent__/nope.txt";
        #[cfg(target_os = "linux")]
        let p = "/home/__aether_probe_absent__/nope.txt";
        let err = execute(&gate(), &mut ctx, "read_file", &serde_json::json!({"path": p}), false)
            .expect_err("不存在的文件应报错")
            .to_string();
        assert!(
            !err.contains("不在允许读取的范围内"),
            "{p} 位于白名单内，不应被策略拒绝，实得: {err}"
        );
    }

    /// 凭证类子路径即使在白名单根之内也必须拒绝（纯策略函数单测，不依赖文件存在）。
    #[test]
    fn credential_subpaths_are_recognized() {
        for p in [
            "C:/Users/x/.ssh/id_rsa",
            "/home/x/.ssh/id_ed25519",
            "/home/x/.aws/credentials",
            "/home/x/.netrc",
            "C:/Users/x/.git-credentials",
        ] {
            assert!(
                is_credential_path(std::path::Path::new(p)),
                "{p} 应被识别为凭证路径"
            );
        }
        for p in ["/home/x/notes.txt", "C:/Users/x/Documents/report.md"] {
            assert!(
                !is_credential_path(std::path::Path::new(p)),
                "{p} 不应被识别为凭证路径"
            );
        }
    }

    /// P0-4b 的配套事实：read_file 被标记为敏感输出（其内容不得离开本机）。
    #[test]
    fn read_file_is_marked_sensitive_output() {
        let reg = registry();
        let rf = reg.iter().find(|t| t.name == "read_file").expect("read_file 应注册");
        assert!(rf.sensitive_output, "read_file 的输出必须标记为敏感（强制本地通道）");
        // 敏感集合：文件内容 + 剪贴板（用户可能刚复制的密码）——读到就强制本地通道。
        // 其余工具不应误标（否则云端通道会被无谓地禁用）。
        let sensitive = ["read_file", "clipboard_read"];
        for t in reg.iter().filter(|t| !sensitive.contains(&t.name)) {
            assert!(!t.sensitive_output, "{} 不应标记为敏感输出", t.name);
        }
        for name in sensitive {
            let t = reg.iter().find(|t| t.name == name).expect(name);
            assert!(t.sensitive_output, "{name} 必须标记为敏感输出");
        }
    }
}
