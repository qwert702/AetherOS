//! 工具系统：AI 操作系统的手。每个工具声明权限等级，
//! 执行前必须过 perm::Gate 并写审计日志。
//!
//! 安全设计：所有进程执行均为"罐头探针"——命令与参数全部是编译期
//! 常量，AI 只能在探针集合里选择，不存在用户输入进入命令行的路径。

use crate::intent::DesktopAction;
use crate::perm::verdict;
use crate::perm::{Gate, Level, Verdict};
use anyhow::{bail, Context, Result};
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
            // 写操作 → L1（可逆写），与 clipboard_read 同级。
            // 第四轮审查（P2-1）前这里是 L0（只读），而 L0 的语义是"读" ——
            // 一个写操作标成只读，意味着它既无确认也无正确的审计分级；
            // 而且当时读（L1）比写（L0）级别高，方向是反的。
            level: Level::L1,
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

        // ---- 写操作（4.1）：L2 敏感写，必须过闸门 + 用户确认 ----
        Tool {
            name: "file_write",
            description: "写入（或覆盖）一个文本文件。路径必须在用户数据区（家目录、/tmp）内；aether 自身的配置与日志不可写。",
            level: Level::L2,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "目标文件绝对路径"},
                    "content": {"type": "string", "description": "文件内容（上限 256KB，超长整体拒绝）"}
                },
                "required": ["path", "content"]
            }),
            run: tool_file_write,
            consequence: "覆盖写入目标文件：原有内容不会进回收站，直接不可恢复（新建则无影响）",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "file_delete",
            description: "把文件或目录移入回收站（可恢复）。不做永久删除。",
            level: Level::L2,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "要删除的路径"}
                },
                "required": ["path"]
            }),
            run: tool_file_delete,
            consequence: "移入回收站，可从回收站恢复；但超过 7 天、或回收站条目/容量超限时会被自动清理",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "file_rename",
            description: "重命名或移动文件/目录。源与目标都必须在用户数据区内。",
            level: Level::L2,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "from": {"type": "string", "description": "源路径"},
                    "to": {"type": "string", "description": "目标路径"}
                },
                "required": ["from", "to"]
            }),
            run: tool_file_rename,
            consequence: "移动或改名；若目标已存在则拒绝（不覆盖）",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "trash_list",
            description: "列出回收站内容（条目名、原路径、大小）。删除后想找回东西时先用它。",
            level: Level::L0,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            run: tool_trash_list,
            consequence: "",
            echo_field: None,
            // 输出含用户文件路径。路径本身不含文件内容，所以不标 sensitive_output ——
            // 标了会让"看看回收站里有什么"这种纯本地操作也强制走本地模型
            sensitive_output: false,
        },
        Tool {
            name: "trash_restore",
            description: "把回收站里的某项恢复到原路径。entry 用 trash_list 给出的条目名。",
            level: Level::L2,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "entry": {"type": "string", "description": "条目名，形如 1759000000-1"}
                },
                "required": ["entry"]
            }),
            run: tool_trash_restore,
            consequence: "把文件搬回原路径；若原路径已被占用则拒绝（不覆盖现有文件）",
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
        Verdict::Allowed => verdict::ALLOWED,
        Verdict::NeedsConfirmation => verdict::NEEDS_CONFIRMATION,
        Verdict::Denied(_) => verdict::DENIED,
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
                if let Err(ae) = gate.audit(name, tool.level, &args_str, verdict::FAILED) {
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

/// **写操作的允许根：比读更窄。**
///
/// 为什么不复用读白名单：读 `/etc/aether` 只是"看配置"，而**写**它等于让 AI
/// 修改自己的权限规则；写 `/var/log/aether` 等于篡改审计记录（灭证）。
/// 这两条都是提权/灭证，不是"文件编辑" —— 必须挡在闸门之外。
///
/// 写操作**不能**在允许根之外新建文件，所以这里不需要"拒绝子路径"那一层来
/// 排除系统区；但凭证类路径（`.ssh` 等）本来就在允许根**之内**，仍需单独拒绝，
/// 见 `resolve_writable`。
#[cfg(target_os = "linux")]
const WRITE_ALLOWED_ROOTS: &[&str] = &[
    "/home", // 用户数据
    "/tmp",  // 临时文件
];

/// 开发机（Windows）上的对应白名单。
#[cfg(not(target_os = "linux"))]
const WRITE_ALLOWED_ROOTS: &[&str] = &["C:/Users", "C:/Temp"];

/// 单次写入的字节上限。超长整体拒绝（不做截断 —— 截断会静默产生半个文件）。
const MAX_WRITE_BYTES: usize = 256 * 1024;

/// 去掉 Windows canonicalize 产生的 `\\?\` 扩展长度前缀，
/// 否则白名单前缀永远匹配不上。
pub(crate) fn strip_verbatim_prefix(p: std::path::PathBuf) -> std::path::PathBuf {
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

/// 校验并规范化待写路径；不可写则返回带原因的 Err。
///
/// 与 `resolve_readable` 的两点差异：
/// 1. 允许根更窄（见 `WRITE_ALLOWED_ROOTS`）；
/// 2. 目标**可以尚不存在**（新建文件、重命名到新名字）—— 此时 `canonicalize`
///    会直接失败，所以改为"规范化父目录 + 拼上文件名"。
///
/// 仍然先 canonicalize：`..` 穿越与符号链接绕行因此同样被堵死。
fn resolve_writable(path: &str) -> Result<std::path::PathBuf> {
    let p = std::path::Path::new(path);
    let canon = if p.exists() {
        std::fs::canonicalize(p).map_err(|e| anyhow::anyhow!("无法访问 {path}: {e}"))?
    } else {
        let parent = p
            .parent()
            .filter(|s| !s.as_os_str().is_empty())
            .ok_or_else(|| anyhow::anyhow!("路径缺少父目录: {path}"))?;
        let file = p
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("路径缺少文件名: {path}"))?;
        let cparent = std::fs::canonicalize(parent)
            .map_err(|e| anyhow::anyhow!("父目录不可访问（{}）: {e}", parent.display()))?;
        cparent.join(file)
    };
    let canon = strip_verbatim_prefix(canon);

    if !WRITE_ALLOWED_ROOTS.iter().any(|root| canon.starts_with(root)) {
        bail!(
            "路径不在允许写入的范围内（{path}）：写操作仅限用户数据区（家目录、/tmp）。\
             aether 自身的配置与日志不可写 —— 那等于让 AI 改自己的权限规则与审计记录"
        );
    }
    if is_credential_path(&canon) {
        bail!("拒绝写入凭证类路径（{path}）");
    }
    Ok(canon)
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

// ---------------------------------------------------------------------------
// 写操作（4.1）：全部 L2 —— 必须过闸门 + 用户确认卡片
//
// 三条设计约束（来自 `docs/PRODUCTION-PLAN-2026-09-28.md` §7）：
// 1. 所有入口走 `Gate::judge`（这三个是 `ToolCall`，自动过闸门；**不要**另加
//    `Request` 变体，那会绕过闸门 —— P1-1 的教训）；
// 2. 删除先进回收站，且回收站有容量上限与过期清理；
// 3. 审计先补齐（4.3b 已完成），否则 L2 写操作没有追溯手段。
// ---------------------------------------------------------------------------

/// 写入（或覆盖）文本文件（L2）。
fn tool_file_write(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        bail!("缺少 path 参数");
    };
    let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
        bail!("缺少 content 参数");
    };
    if content.len() > MAX_WRITE_BYTES {
        bail!(
            "内容 {} 字节，超过单次写入上限 {MAX_WRITE_BYTES}（拒绝整段，不做截断）",
            content.len()
        );
    }
    let canon = resolve_writable(path)?;
    if canon.is_dir() {
        bail!("目标是目录，不能写入: {}", canon.display());
    }
    let existed = canon.exists();
    std::fs::write(&canon, content).with_context(|| format!("写入失败: {}", canon.display()))?;
    Ok(format!(
        "{} {}（{} 字节）",
        if existed { "已覆盖" } else { "已创建" },
        canon.display(),
        content.len()
    ))
}

/// 删除文件/目录（L2）—— **移入回收站**，不做永久删除。
fn tool_file_delete(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        bail!("缺少 path 参数");
    };
    let canon = resolve_writable(path)?;
    if !canon.exists() {
        bail!("路径不存在: {}", canon.display());
    }
    let root = crate::trash::default_root();
    // 先按策略清理：回收站自己没上限的话，它只是把"磁盘满"挪了个位置
    let purged = crate::trash::purge(&root, &crate::trash::Policy::default());
    let name = crate::trash::trash(&root, &canon)?;
    let mut msg = format!("已移入回收站: {}（条目 {name}，可恢复）", canon.display());
    if purged > 0 {
        msg.push_str(&format!("；顺带清理了 {purged} 个过期条目"));
    }
    Ok(msg)
}

/// 重命名 / 移动（L2）。源与目标都必须在可写白名单内。
fn tool_file_rename(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(from) = args.get("from").and_then(|v| v.as_str()) else {
        bail!("缺少 from 参数");
    };
    let Some(to) = args.get("to").and_then(|v| v.as_str()) else {
        bail!("缺少 to 参数");
    };
    let src = resolve_writable(from)?;
    let dst = resolve_writable(to)?;
    if !src.exists() {
        bail!("源不存在: {}", src.display());
    }
    // 不覆盖已存在的目标：静默盖掉一个文件比"操作失败"糟得多
    if dst.exists() {
        bail!("目标已存在，拒绝覆盖: {}", dst.display());
    }
    std::fs::rename(&src, &dst)
        .with_context(|| format!("重命名失败: {} → {}", src.display(), dst.display()))?;
    Ok(format!("已重命名: {} → {}", src.display(), dst.display()))
}

/// 列出回收站内容（L0，只读）。
///
/// 没有它，`file_delete` 就是"只能进不能出" —— 回收站里的东西看得见恢复不了。
fn tool_trash_list(_args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let root = crate::trash::default_root();
    let entries = crate::trash::list(&root);
    if entries.is_empty() {
        return Ok("回收站为空".into());
    }
    let mut out = format!("回收站共 {} 项（最新在前）：\n", entries.len());
    for e in entries.iter().take(50) {
        let kind = if e.is_dir {
            "目录".to_string()
        } else {
            format!("{} 字节", e.bytes)
        };
        out.push_str(&format!("- {}（{kind}，条目 {}）\n", e.origin, e.name));
    }
    if entries.len() > 50 {
        out.push_str(&format!("…（另有 {} 项未列出）\n", entries.len() - 50));
    }
    out.push_str("\n恢复用 trash_restore，参数 entry 填上表的「条目」值");
    Ok(out)
}

/// 从回收站恢复（L2）。`entry` 是 `trash_list` 给出的条目名，不是路径。
fn tool_trash_restore(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(entry) = args.get("entry").and_then(|v| v.as_str()) else {
        bail!("缺少 entry 参数（先用 trash_list 查条目名）");
    };
    let root = crate::trash::default_root();
    let path = crate::trash::restore(&root, entry)?;
    Ok(format!("已恢复到 {}", path.display()))
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

    /// 回收站是**进程外共享状态**（`%TEMP%/aether-trash`），而测试默认并行 ——
    /// 两个测试同时 `purge`/`trash` 会互相干扰（实测：偶发的"删除失败"）。
    /// 这是全局状态 + 并行测试的标准处理方式（与 `server.rs` 的剪贴板锁同款）。
    static TRASH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn trash_guard() -> std::sync::MutexGuard<'static, ()> {
        TRASH_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

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

    // ---- 4.1 写操作 ----

    /// **写白名单必须是读白名单的子集** —— "写不能比读更宽"的机械保证。
    ///
    /// 比逐条列举路径更耐改：将来有人往写白名单里加目录，若它不在读白名单内，
    /// 这条会红。
    #[test]
    fn write_roots_are_subset_of_read_roots() {
        for w in WRITE_ALLOWED_ROOTS {
            assert!(
                READ_ALLOWED_ROOTS.iter().any(|r| r == w),
                "写白名单 {w} 不在读白名单内 —— 写操作不能比读更宽"
            );
        }
    }

    /// 目标平台（Linux）上 aether 自身的配置与日志**可读但不可写**。
    ///
    /// 写 `/etc/aether` = 让 AI 改自己的权限规则（提权）；
    /// 写 `/var/log/aether` = 篡改审计记录（灭证）。两者都必须挡在闸门之外。
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_write_excludes_aether_own_config_and_logs() {
        assert!(READ_ALLOWED_ROOTS.contains(&"/etc/aether"), "配置应可读");
        assert!(READ_ALLOWED_ROOTS.contains(&"/var/log/aether"), "日志应可读");
        assert!(!WRITE_ALLOWED_ROOTS.contains(&"/etc/aether"), "配置不可写");
        assert!(!WRITE_ALLOWED_ROOTS.contains(&"/var/log/aether"), "日志不可写");
    }

    /// 写工具都必须是 **L2**（敏感写 → 弹确认卡片）；只读的 `trash_list` 是 L0。
    #[test]
    fn write_tools_are_l2_and_not_sensitive_output() {
        let reg = registry();
        for name in ["file_write", "file_delete", "file_rename", "trash_restore"] {
            let t = reg
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} 应注册"));
            assert_eq!(t.level, Level::L2, "{name} 必须是 L2（敏感写，需确认）");
            assert!(!t.sensitive_output, "{name} 的输出不含用户数据，不应标敏感");
        }
        let l = reg.iter().find(|t| t.name == "trash_list").expect("trash_list 应注册");
        assert_eq!(l.level, Level::L0, "列回收站是只读操作");
    }

    /// L2 写操作不带 approval 时必须停在确认上 —— 这是"AI 不能自己动手"的落点。
    #[test]
    fn write_tools_need_confirmation_without_approval() {
        let mut ctx = ToolCtx::default();
        let cases = [
            ("file_write", serde_json::json!({"path": "C:/Temp/x.txt", "content": "hi"})),
            ("file_delete", serde_json::json!({"path": "C:/Temp/x.txt"})),
            ("file_rename", serde_json::json!({"from": "C:/Temp/a", "to": "C:/Temp/b"})),
        ];
        for (name, args) in cases {
            let out = execute(&gate(), &mut ctx, name, &args, false)
                .unwrap_or_else(|e| panic!("{name} 应停在确认而非报错: {e}"));
            assert!(
                matches!(out, ExecOutcome::NeedsConfirmation { .. }),
                "{name} 不带 approval 必须弹确认"
            );
        }
    }

    /// 写操作不能碰白名单之外的路径（用 `approved=true` 绕过闸门，直测路径校验）。
    #[test]
    fn write_rejects_paths_outside_whitelist() {
        let mut ctx = ToolCtx::default();
        for (tool, args) in [
            ("file_write", serde_json::json!({"path": "C:/Windows/win.ini", "content": "x"})),
            ("file_delete", serde_json::json!({"path": "C:/Windows/win.ini"})),
            ("file_rename", serde_json::json!({"from": "C:/Windows/win.ini", "to": "C:/Windows/x"})),
        ] {
            let r = execute(&gate(), &mut ctx, tool, &args, true);
            let err = r
                .expect_err(&format!("{tool} 对系统区应被拒绝"))
                .to_string();
            assert!(
                err.contains("不在允许写入的范围内"),
                "{tool} 实得: {err}"
            );
        }
    }

    /// 超长内容整体拒绝，不产生半个文件。
    #[test]
    fn file_write_rejects_oversized_wholesale() {
        let mut ctx = ToolCtx::default();
        let big = "x".repeat(MAX_WRITE_BYTES + 1);
        let r = execute(
            &gate(),
            &mut ctx,
            "file_write",
            &serde_json::json!({"path": "C:/Temp/big.txt", "content": big}),
            true,
        );
        let err = r.expect_err("超长应被拒绝").to_string();
        assert!(err.contains("超过单次写入上限"), "实得: {err}");
    }

    /// 删除 → 回收站 → 恢复的完整往返（走真实工具入口，不只测 trash 模块）。
    #[test]
    fn delete_then_restore_roundtrip_via_tool() {
        let _g = trash_guard();
        let dir = std::env::temp_dir().join("aether_write_tool_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let f = dir.join("victim.txt");
        std::fs::write(&f, b"content").unwrap();

        let mut ctx = ToolCtx::default();
        let out = execute(
            &gate(),
            &mut ctx,
            "file_delete",
            &serde_json::json!({"path": f.to_string_lossy()}),
            true,
        )
        .expect("删除应成功");
        let ExecOutcome::Done(msg) = out else {
            panic!("应返回 Done");
        };
        assert!(msg.contains("已移入回收站"), "实得: {msg}");
        assert!(!f.exists(), "原文件应已被移走");

        // 清理：回收站是**共享**的（%TEMP%/aether-trash），测试不该留下条目
        let root = crate::trash::default_root();
        for e in crate::trash::list(&root) {
            if e.origin.ends_with("victim.txt") {
                let _ = std::fs::remove_dir_all(root.join(&e.name));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 重命名不覆盖已存在的目标。
    #[test]
    fn rename_refuses_to_overwrite_existing() {
        let dir = std::env::temp_dir().join("aether_rename_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        let mut ctx = ToolCtx::default();
        let r = execute(
            &gate(),
            &mut ctx,
            "file_rename",
            &serde_json::json!({"from": a.to_string_lossy(), "to": b.to_string_lossy()}),
            true,
        );
        let err = r.expect_err("目标已存在应被拒绝").to_string();
        assert!(err.contains("拒绝覆盖"), "实得: {err}");
        assert_eq!(std::fs::read(&b).unwrap(), b"b", "目标内容不应被动过");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 删除 → 列表 → 恢复的完整往返（4.1 验收里的「误删可从回收站恢复」）。
    ///
    /// 这条测试存在的理由：`trash::restore` 有单测，但**没有入口**时它等于不存在 ——
    /// 回收站只能进不能出。真正要验的是"走工具链路能把它拿回来"。
    #[test]
    fn delete_then_restore_through_tools() {
        let _g = trash_guard();
        // 目录名必须与 `trash.rs` 里的 `tmp("roundtrip")`（→ aether_trash_roundtrip）
        // **不同** —— 重名时本测试开头的 remove_dir_all 会把对方的目录删掉，
        // 表现为"对方偶发失败"，很难一眼看出是自己造成的
        let dir = std::env::temp_dir().join("aether_trash_tool_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let f = dir.join("recover-me.txt");
        std::fs::write(&f, b"payload").unwrap();

        let mut ctx = ToolCtx::default();
        // 1. 删（进回收站）
        let out = execute(
            &gate(),
            &mut ctx,
            "file_delete",
            &serde_json::json!({"path": f.to_string_lossy()}),
            true,
        )
        .expect("删除应成功");
        assert!(matches!(out, ExecOutcome::Done(_)));
        assert!(!f.exists(), "原文件应已被移走");

        // 2. trash_list 应能列出它（用户/AI 靠这个知道回收站里有什么）
        let listed = execute(&gate(), &mut ctx, "trash_list", &serde_json::json!({}), true)
            .expect("列表应成功");
        let ExecOutcome::Done(text) = listed else {
            panic!("应返回 Done");
        };
        assert!(text.contains("recover-me.txt"), "列表应含原路径: {text}");
        assert!(text.contains("trash_restore"), "应提示如何恢复: {text}");

        // 3. 取条目名（走 trash 模块而不是解析列表文本 —— 后者太脆）
        let root = crate::trash::default_root();
        let entry = crate::trash::list(&root)
            .into_iter()
            .find(|e| e.origin.ends_with("recover-me.txt"))
            .map(|e| e.name)
            .expect("回收站里应有刚删的文件");

        // 4. 恢复
        let back = execute(
            &gate(),
            &mut ctx,
            "trash_restore",
            &serde_json::json!({"entry": entry}),
            true,
        )
        .expect("恢复应成功");
        assert!(matches!(back, ExecOutcome::Done(_)));
        assert!(f.exists(), "文件应已回到原路径");
        assert_eq!(std::fs::read(&f).unwrap(), b"payload", "内容应完好");

        // 清理
        for e in crate::trash::list(&root) {
            if e.origin.ends_with("recover-me.txt") {
                let _ = std::fs::remove_dir_all(root.join(&e.name));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `trash_restore` 的 entry 来自外部输入，非法值必须被拒而不是 panic。
    #[test]
    fn trash_restore_rejects_bad_entry() {
        let mut ctx = ToolCtx::default();
        for bad in ["../etc", "a/b", ""] {
            let r = execute(
                &gate(),
                &mut ctx,
                "trash_restore",
                &serde_json::json!({"entry": bad}),
                true,
            );
            assert!(r.is_err(), "{bad:?} 应被拒绝");
        }
        // 不存在的条目：报错而不是静默成功
        let r = execute(
            &gate(),
            &mut ctx,
            "trash_restore",
            &serde_json::json!({"entry": "9999999999-1"}),
            true,
        );
        assert!(r.is_err(), "不存在的条目应报错");
    }
}
