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
        Tool {
            name: "app_list",
            description: "列出这台机器上已安装的应用。要装应用、排障、或者确认某个程序在不在，先看这个。",
            level: Level::L0,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            run: tool_app_list,
            consequence: "",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "app_install",
            description: "把一个应用包（含 app.json 的目录）装进系统，装完在终端里直接敲名字就能运行。**装之前会做依赖预检**：静态链接、缺哪些共享库、或者根本不是本机能执行的格式，都会在装之前讲清楚。",
            level: Level::L2,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "应用包目录（里面要有 app.json）。通常先把它放在家目录或 /tmp"}
                },
                "required": ["path"]
            }),
            run: tool_app_install,
            consequence: "该目录会被复制进 /var/apps/<id>/，并出现在终端的 PATH 里。装进来的程序以你的权限运行。",
            echo_field: None,
            sensitive_output: false,
        },
        Tool {
            name: "app_remove",
            description: "卸载一个已安装的应用。不是直接删 —— 会移入回收站，删错了还能捞回来。",
            level: Level::L2,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "应用 id（先用 app_list 查）"}
                },
                "required": ["id"]
            }),
            run: tool_app_remove,
            consequence: "该应用从系统里移除（移入回收站），终端里的命令随之失效。",
            echo_field: None,
            sensitive_output: false,
        },
    ]
}

/// 必须由**已注册的 UI 通道**发起的工具名单（单一事实来源）。
///
/// 为什么需要它：第四轮审查（P1-1）把剪贴板的门槛加在 `Request::ClipboardGet/Set` 上，
/// 但同一份数据经 `Request::ToolCall{tool:"clipboard_read"}` 也能拿到 —— 闸门按
/// "入口变体"手写，就必然漏掉等价入口。2026-10-02 安全审计（H-2）已实测复现：
/// 同一条未注册连接走协议端点返回 403、走工具路径却成功读回剪贴板明文。
///
/// 判定依据是**资源敏感度**而不是工具等级：这些工具读写的是"用户环境里的数据"，
/// L1 的自动放行只对"已确认身份的调用方"成立。
///
/// `read_file` 是 2026-10-02 对抗审查补进来的：它与 `clipboard_read` 同为
/// `sensitive_output`，但等级是 **L0**（免确认）—— 于是"未注册连接以 root 身份
/// 代读 /home 下任意文件（含别的用户家目录）"是一条现成的原语。合成器（唯一正牌
/// 交互方）始终先注册，所以对正常使用没有影响；`aetherd chat` 这类无 UI 的命令行
/// 用法会失去读文件能力，这是有意的收紧。
const TRUSTED_CHANNEL_TOOLS: &[&str] = &["clipboard_read", "clipboard_write", "read_file"];

/// 按名字查询该工具是否需要可信通道（`agent_run` 与 `server::ToolCall` 共用同一条判定）。
pub fn requires_trusted_channel(name: &str) -> bool {
    TRUSTED_CHANNEL_TOOLS.contains(&name)
}

/// 该工具的输出是否含用户数据（文件内容）。
///
/// agent 循环据此置位"敏感上下文"：其结果一旦进入 messages，
/// 后续轮次强制走本地通道（P0-4b）。
pub fn is_sensitive_output(name: &str) -> bool {
    registry().iter().any(|t| t.name == name && t.sensitive_output)
}

/// 非密码学指纹（FNV-1a 64 位）：只用于在审计日志里**关联同一次内容**
/// （例如"这两条记录写的是同一份数据"），不参与任何安全判定，因此不需要抗碰撞。
fn fingerprint(s: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// 工具参数的**审计表示**：敏感字段脱敏，其余原样。
///
/// 为什么必须脱敏（2026-10-02 审计 M-1）：审计日志此前落的是 `args.to_string()`，
/// 于是 `clipboard_write` 的 `text`（剪贴板里常是刚复制的密码）与 `file_write` 的
/// `content`（可能是密钥文件全文）**逐字进了日志**。而审计日志长期保留、且
/// `/var/log/aether` 在读取白名单里 —— 等于给敏感数据开了第二个泄露面。
///
/// 保留长度 + 指纹，是为了不牺牲可追溯性：仍能回答"写的是不是同一份内容""多大"。
/// 注意 `NeedsConfirmation` 发给 UI 的 `arguments` **不脱敏** —— 用户要看到自己批准的是什么。
///
/// `pub` 是因为**所有**写审计的地方都要用它（执行路径、用户拒绝路径……）：
/// 只在其中一处脱敏等于没脱敏（自查时发现 `ConfirmCancel` 漏了）。
///
/// ## 判定方式：**默认脱敏**，而不是"点名脱敏"
///
/// 代码审查指出：原来是"字段名黑名单"（只脱敏 clipboard_write.text / file_write.content），
/// 于是**新增一个携带内容的字段就会漏** —— 而"漏"这件事在审计里看不出来。
/// 现在反过来：只有**明确认定为安全**的短字段（路径、id、枚举名等）原样保留，
/// 其余字符串值一律脱敏。判据是"字段名在白名单里 **且** 值不太长"，
/// 这样既不会把 `{"path":"/etc/x"}` 这种有用的追溯信息抹掉，
/// 也不会因为将来加了 `note`/`payload` 之类的字段而漏。
pub fn audit_args(tool: &str, args: &serde_json::Value) -> String {
    /// 可以原样记录的字段名（都是"标识/位置/枚举"类，不含用户内容）。
    ///
    /// 清单来自工具注册表实际用到的键（`action/app/confirm/disk/entry/from/id/
    /// layout/path/probe/scope/to` 等）—— **不含** `content`/`text`，
    /// 那两个就是这条规则要挡的东西。
    const SAFE_FIELDS: &[&str] = &[
        "path", "dir", "from", "to", "id", "name", "tool", "unit", "action", "scope",
        "disk", "confirm", "entry", "app", "session_id", "approval", "probe", "layout",
    ];
    /// 白名单字段也超过这个长度就脱敏（防止有人把内容塞进 `path` 这种字段）。
    const MAX_SAFE_LEN: usize = 256;

    let Some(obj) = args.as_object() else {
        return args.to_string();
    };
    let mut redacted = args.clone();
    let Some(obj_mut) = redacted.as_object_mut() else {
        return args.to_string();
    };
    for (k, v) in obj.iter() {
        let Some(s) = v.as_str() else { continue };
        let safe = SAFE_FIELDS.contains(&k.as_str()) && s.chars().count() <= MAX_SAFE_LEN;
        if safe {
            continue;
        }
        obj_mut.insert(
            k.clone(),
            serde_json::Value::String(format!(
                "<已脱敏：{} 字节，指纹 {:016x}>",
                s.len(),
                fingerprint(s)
            )),
        );
    }
    // 工具名单独带上：只看一行日志时，"哪个工具"比参数更重要
    let _ = tool;
    redacted.to_string()
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
    let verdict = gate.judge(name, tool.level, args, approved);
    let verdict_str = match &verdict {
        Verdict::Allowed => verdict::ALLOWED,
        Verdict::NeedsConfirmation => verdict::NEEDS_CONFIRMATION,
        Verdict::Denied(_) => verdict::DENIED,
    };
    // 审计失败不阻断执行，但必须显式可见：错误随输出返回调用方，并落 stderr。
    // 参数由 `audit_call` 内部脱敏（不在这里传字符串 —— 见该方法的说明）。
    let audit_warn = gate.audit_call(name, tool.level, args, verdict_str).err().map(|e| {
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
                if let Err(ae) = gate.audit_call(name, tool.level, args, verdict::FAILED) {
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

fn tool_app_list(_args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let root = crate::apps::default_root();
    let items = crate::apps::list(&root);
    if items.is_empty() {
        return Ok(format!("还没有装任何应用（目录 {}）", root.display()));
    }
    let mut s = format!("已装 {} 个应用：\n", items.len());
    for a in items {
        s.push_str(&format!(
            "- {}（{}）{} · {} KB · {}\n",
            a.manifest.id,
            a.manifest.name,
            a.manifest.version,
            a.bytes / 1024,
            a.manifest.desc
        ));
    }
    Ok(s)
}

fn tool_app_install(args: &Value, ctx: &mut ToolCtx) -> Result<String> {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        bail!("缺少参数 path");
    };
    // 源目录也要过读白名单：否则这个 L2 工具就成了"把系统任意文件搬进应用目录"的跳板
    let src = resolve_readable(path)?;
    let root = crate::apps::default_root();
    let (a, pre) = crate::apps::install(&root, &src)?;
    let mut s = format!(
        "已装「{}」（{}）→ {}\n大小 {} KB\n预检：{}",
        a.manifest.name,
        a.manifest.id,
        a.dir.display(),
        a.bytes / 1024,
        pre.detail
    );
    if pre.kind == crate::apps::PreflightKind::Risky {
        s.push_str("\n⚠ 预检不通过但已装上 —— 它很可能跑不起来，缺什么上面写了。");
    }
    let _ = ctx;
    Ok(s)
}

fn tool_app_remove(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
    let Some(id) = args.get("id").and_then(|v| v.as_str()) else {
        bail!("缺少参数 id");
    };
    let root = crate::apps::default_root();
    let trashed = crate::apps::remove(&root, id)?;
    Ok(format!("已卸载「{id}」，移入回收站：{}", trashed.display()))
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
    "/var/apps",       // 已装应用（见 apps.rs）
    "/tmp",            // 临时文件
];

/// 开发机（Windows）上的对应白名单：用户数据区与临时目录。
#[cfg(not(target_os = "linux"))]
const READ_ALLOWED_ROOTS: &[&str] = &["C:/Users", "C:/Temp"];

/// 即使在白名单根之内，也拒绝的凭证类子路径（家目录下的私钥/令牌）。
///
/// `ui.key` 是 2026-10-02 安全审计补上的（高危 H-1）：UI 通道密钥此前落在
/// `/var/log/aether/ui.key`，而 `/var/log/aether` **正好在读取白名单里** ——
/// 于是 `read_file`（L0、免确认、不要求已注册）就成了"任意本机进程取走 UI 密钥"
/// 的现成工具，拿到密钥即可注册为 UI 通道并自我确认 L3 操作。
/// 密钥文件本身是 0600，但"以 root 运行的 aetherd 代读"绕过了文件权限。
///
/// 双保险：这里拒绝（拦读取路径）+ `server::UI_KEY_PATH` 已迁到 `/run/aether/`。
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
    // UI 通道密钥：拿到它 = 拿到"自我确认 L2/L3"的能力
    "ui.key",
    // 云端 API Key 与其它 aether 私有凭据（model.json 亦为 0600，同样不该由 AI 代读）
    "model.json",
];

/// 写操作的允许根：比读更窄。
///
/// 为什么不复用读白名单：读 `/etc/aether` 只是"看配置"，而**写**它等于让 AI
/// 修改自己的权限规则；写 `/var/log/aether` 等于篡改审计记录（灭证）。
/// 这两条都是提权/灭证，不是"文件编辑" —— 必须挡在闸门之外。
///
/// `/var/apps` 是**只加的那一个子目录**：装应用要往那儿写，而 `/var/log/aether`
/// 仍然不可写。把整个 `/var` 放开就等于允许灭证，所以只能加到子目录粒度。
///
/// 写操作**不能**在允许根之外新建文件，所以这里不需要"拒绝子路径"那一层来
/// 排除系统区；但凭证类路径（`.ssh` 等）本来就在允许根**之内**，仍需单独拒绝，
/// 见 `resolve_writable`。
#[cfg(target_os = "linux")]
const WRITE_ALLOWED_ROOTS: &[&str] = &[
    "/home",     // 用户数据
    "/var/apps", // 已装应用（安装/卸载要写这里）
    "/tmp",      // 临时文件
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
    let canon = match std::fs::canonicalize(path) {
        Ok(c) => strip_verbatim_prefix(c),
        Err(e) => {
            // 目标不存在（或不可达）时，**先做一次词法层策略判定再报错**。
            //
            // 为什么：canonicalize 先失败的话，一条越权路径得到的是"无法访问：No such file"
            // —— 读的人会往"文件不存在"的方向查，而真正的原因是策略拒了它。
            // 实测（QEMU guest 内以 root 读 `/root/.bashrc`）就是这个表现。
            //
            // 只加在这条 Err 分支上，**不动已存在路径的行为**：词法判定会误伤
            // "词法上不在白名单、但经符号链接落到白名单里"的路径，所以不能无条件前置。
            check_read_policy(std::path::Path::new(path), path)?;
            return Err(anyhow::anyhow!("无法访问 {path}: {e}"));
        }
    };
    check_read_policy(&canon, path)?;
    Ok(canon)
}

/// **纯策略**：这个（已规范化的）路径允不允许读。不碰文件系统。
///
/// 单独拆出来的理由：策略判断**不该依赖目标是否存在**。原写法是先
/// `canonicalize` 再查白名单，于是"不存在的系统路径"报的是"无法访问"而不是
/// "不在允许读取的范围内" —— 策略结论被环境细节盖住了。拆开之后策略可以
/// 跨平台确定性测试（构建机上 `/sys/kernel/osrelease` 恰好不存在，正是这条
/// 把该测试暴露出来的）。
fn check_read_policy(canon: &std::path::Path, shown: &str) -> Result<()> {
    // 1. 必须落在白名单根之内（Path::starts_with 按组件匹配，/homeevil ≠ /home）
    if !READ_ALLOWED_ROOTS.iter().any(|root| canon.starts_with(root)) {
        bail!(
            "路径不在允许读取的范围内（{shown}）：read_file 仅可读用户数据区（家目录、/tmp）\
             与 aether 自身配置/日志；系统区与其它用户目录不可读"
        );
    }
    // 2. 白名单根之内的凭证类子路径仍然拒绝
    if is_credential_path(canon) {
        bail!("拒绝读取凭证类路径（{shown}）");
    }
    Ok(())
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
    check_write_policy(&canon, path)?;
    Ok(canon)
}

/// **纯策略**：这个（已规范化的）路径允不允许写。不碰文件系统（理由同
/// `check_read_policy`）。
fn check_write_policy(canon: &std::path::Path, shown: &str) -> Result<()> {
    if !WRITE_ALLOWED_ROOTS.iter().any(|root| canon.starts_with(root)) {
        bail!(
            "路径不在允许写入的范围内（{shown}）：写操作仅限用户数据区（家目录、/tmp）。\
             aether 自身的配置与日志不可写 —— 那等于让 AI 改自己的权限规则与审计记录"
        );
    }
    if is_credential_path(canon) {
        bail!("拒绝写入凭证类路径（{shown}）");
    }
    Ok(())
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

    /// 回收站是**进程外共享状态**（开发机 `%TEMP%/aether-trash`，Linux `/var/trash`），
    /// 而测试默认并行 —— 两个测试同时 `purge`/`trash` 会互相干扰（实测：偶发的"删除失败"）。
    /// 这是全局状态 + 并行测试的标准处理方式（与 `server.rs` 的剪贴板锁同款）。
    static TRASH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn trash_guard() -> std::sync::MutexGuard<'static, ()> {
        TRASH_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 把回收站根临时改到测试私有目录。
    ///
    /// 为什么必须有：Linux 下默认根是 `/var/trash`，**只有 root 建得出来** —— 而
    /// 构建机/开发用户都不是 root，于是"删除 → 回收站 → 恢复"这条链在 Linux 上
    /// 直接以 `Permission denied` 失败（2026-09-28 构建机实测，4 个失败里的 2 个）。
    /// 通过 `AETHER_TRASH_DIR` 重定向，测试才能在任意机器上跑真实路径。
    ///
    /// 调用方必须持有 `trash_guard()` —— 它改的是进程全局环境变量。
    /// 离开作用域时恢复（`Drop`），避免污染后续测试。
    struct TrashDir(#[allow(dead_code)] std::path::PathBuf);

    impl TrashDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("aether-trash-tool-{tag}"));
            let _ = std::fs::remove_dir_all(&p);
            std::env::set_var("AETHER_TRASH_DIR", &p);
            Self(p)
        }
    }

    impl Drop for TrashDir {
        fn drop(&mut self) {
            std::env::remove_var("AETHER_TRASH_DIR");
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn gate() -> Gate {
        Gate::for_test()
    }

    /// 门禁：新增工具必须显式做"是否需要可信通道"分类。
    ///
    /// 用清单而不是结构体字段，是为了不改 17 处工具字面量；代价是编译器不强制，
    /// 因此由这条测试兜住 —— 新增工具后不登记就会红（H-2 的根因正是"分类不完整"）。
    #[test]
    fn every_tool_is_classified_for_trusted_channel() {
        const NOT_TRUSTED: &[&str] = &[
            "file_write", "file_delete", "file_rename", "trash_list", "trash_restore",
            "sys_info", "sys_probe", "desktop", "install_disk",
            "app_list", "app_install", "app_remove",
        ];
        let names: Vec<&str> = registry().iter().map(|t| t.name).collect();
        for n in &names {
            assert!(
                requires_trusted_channel(n) || NOT_TRUSTED.contains(n),
                "工具 {n} 未分类：请加入 TRUSTED_CHANNEL_TOOLS 或 NOT_TRUSTED"
            );
        }
        assert_eq!(
            names.len(),
            TRUSTED_CHANNEL_TOOLS.len() + NOT_TRUSTED.len(),
            "分类表与注册表数量不一致（新增或删除了工具？）"
        );
        // 剪贴板两个端点必须被判定为需要可信通道
        assert!(requires_trusted_channel("clipboard_read"));
        assert!(requires_trusted_channel("clipboard_write"));
        // read_file 也一样（2026-10-02 对抗审查补入）：它与 clipboard_read 同为
        // sensitive_output，却是 L0 免确认 —— 不拦就是"未注册连接以 root 代读用户家目录"。
        assert!(requires_trusted_channel("read_file"));
        // 只读探针不应被误判（否则桌面状态查询会被无谓地挡住）
        assert!(!requires_trusted_channel("sys_info"));
        assert!(!requires_trusted_channel("sys_probe"));
    }

    /// 默认脱敏（代码审查建议的治本改法）：**没登记为安全的字符串字段一律脱敏**。
    ///
    /// 旧实现是"点名脱敏"（只认 clipboard_write.text / file_write.content），
    /// 于是将来新增一个携带内容的字段就会漏 —— 而"漏"在审计里看不出来。
    /// 这条测试用**一个当前不存在的字段名**来钉住新语义。
    #[test]
    fn audit_args_redacts_unknown_string_fields_by_default() {
        let args = serde_json::json!({
            "path": "/tmp/x",              // 白名单 → 保留（追溯需要）
            "future_field": "尚未存在的秘密内容", // 不在白名单 → 必须脱敏
            "nested": { "a": 1 },          // 非字符串 → 原样
        });
        let s = audit_args("read_file", &args);
        assert!(s.contains("/tmp/x"), "白名单字段应保留：{s}");
        assert!(!s.contains("尚未存在的秘密内容"), "未登记字段必须默认脱敏：{s}");
        assert!(s.contains("nested"), "非字符串字段应保留：{s}");

        // 白名单字段也要有长度上限：否则可以把内容塞进 `path` 绕过脱敏
        let long = serde_json::json!({ "path": "x".repeat(300) });
        let s2 = audit_args("read_file", &long);
        assert!(!s2.contains(&"x".repeat(300)), "超长的白名单字段也必须脱敏：{s2}");
    }

    /// M-1 回归：审计参数必须脱敏 —— 剪贴板明文与写入内容都不得逐字落盘。
    #[test]
    fn audit_args_redacts_clipboard_and_file_content() {
        const SECRET: &str = "用户刚复制的密码：Hunter2-AETHER";
        let clip = serde_json::json!({"text": SECRET});
        let s = audit_args("clipboard_write", &clip);
        assert!(!s.contains(SECRET), "剪贴板明文不得进审计：{s}");
        assert!(s.contains("已脱敏"), "应标明已脱敏：{s}");
        assert!(s.contains(&format!("{} 字节", SECRET.len())), "应保留长度：{s}");

        let file = serde_json::json!({"path": "/tmp/a.txt", "content": "-----BEGIN KEY-----"});
        let s = audit_args("file_write", &file);
        assert!(!s.contains("BEGIN KEY"), "写入内容不得进审计：{s}");
        assert!(s.contains("/tmp/a.txt"), "非敏感字段应保留：{s}");

        // 非敏感工具原样保留（否则审计会失去可追溯性）
        let probe = serde_json::json!({"probe": "uname"});
        assert_eq!(audit_args("sys_probe", &probe), probe.to_string());
        // 同一内容 → 同一指纹（可关联）；不同内容 → 不同指纹
        let a = audit_args("clipboard_write", &serde_json::json!({"text": "same"}));
        let b = audit_args("clipboard_write", &serde_json::json!({"text": "same"}));
        let c = audit_args("clipboard_write", &serde_json::json!({"text": "other"}));
        assert_eq!(a, b, "同内容应得到同指纹");
        assert_ne!(a, c, "不同内容应得到不同指纹");
    }

    /// H-1 回归：UI 密钥与云端 API Key 都不得被 AI 读取。
    ///
    /// 这条链的后果是"任意本机进程自取密钥 → 自注册 UI 通道 → 自我确认 L3"。
    /// 分两层：凭证类判定是纯函数（与平台无关），入口层则必须用**本平台白名单内**
    /// 的路径，否则报的是"不在允许范围"而不是"凭证类"，测不到真正想测的那一层。
    #[test]
    fn credential_paths_including_ui_key_are_rejected() {        for p in [
            "/var/log/aether/ui.key",
            "/run/aether/ui.key",
            "/etc/aether/model.json",
            "/home/u/.ssh/id_rsa",
        ] {
            assert!(is_credential_path(std::path::Path::new(p)), "{p} 应被判为凭证类");
        }

        #[cfg(target_os = "linux")]
        let cases = ["/var/log/aether/ui.key", "/run/aether/ui.key", "/etc/aether/model.json"];
        #[cfg(not(target_os = "linux"))]
        let cases = ["C:/Users/x/ui.key", "C:/Temp/model.json"];

        for p in cases {
            let policy = check_read_policy(std::path::Path::new(p), p)
                .expect_err(&format!("{p} 必须被策略拒绝"))
                .to_string();
            assert!(policy.contains("凭证类"), "{p} 策略实得: {policy}");

            let mut ctx = ToolCtx::default();
            let err = execute(&gate(), &mut ctx, "read_file", &serde_json::json!({"path": p}), false)
                .expect_err(&format!("{p} 入口必须被拒绝"))
                .to_string();
            assert!(err.contains("凭证类"), "{p} 入口实得: {err}");
        }
    }

    /// 拒绝清单必须与**真实的密钥路径**绑定（代码审查建议）。
    ///
    /// 否则把 `aether_ipc::UI_KEY_PATH` 改成别的文件名时，上面那条测试照绿 ——
    /// 它用的是硬编码字面量，测不到"路径改了但清单没跟上"，而 H-1 会原样复现。
    #[test]
    fn deny_list_is_bound_to_the_real_key_path() {
        let real = std::path::Path::new(aether_ipc::UI_KEY_PATH);
        assert!(
            is_credential_path(real),
            "真实密钥路径 {} 必须被判为凭证类",
            aether_ipc::UI_KEY_PATH
        );
        let name = real.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        assert!(
            !name.is_empty(),
            "UI_KEY_PATH 必须有文件名：{}",
            aether_ipc::UI_KEY_PATH
        );
        assert!(
            READ_DENY_SUBPATHS.iter().any(|d| name.contains(d) || d.contains(name)),
            "拒绝清单里没有任何条目能覆盖 {name}：{READ_DENY_SUBPATHS:?}\
             —— 改了密钥路径就必须同步改这里，否则 AI 又能读到密钥"
        );
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
    ///
    /// 分两层断言：**策略层**是纯函数、不依赖路径是否存在；**入口层**只在路径
    /// 真实存在时测，用来证明工具确实接到了同一个策略。
    /// 为什么不能只测入口层：构建机上 `/sys/kernel/osrelease` 恰好不存在，于是
    /// `canonicalize` 先失败、报的是"无法访问"而不是策略拒绝 —— 测试就会以
    /// 一个与策略无关的理由失败（2026-09-28 构建机实测）。
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
            let policy = check_read_policy(std::path::Path::new(p), p)
                .expect_err(&format!("{p} 应被策略拒绝"))
                .to_string();
            assert!(
                policy.contains("不在允许读取的范围内") || policy.contains("凭证类"),
                "{p} 策略实得: {policy}"
            );

            // 注：不再按"路径是否存在"跳过 —— 修掉 `resolve_readable` 的报错顺序之后，
            // 不存在的越权路径也会得到策略拒绝，所以全部 6 条都能走完整入口断言。
            let err = execute(&gate(), &mut ctx, "read_file", &serde_json::json!({"path": p}), false)
                .expect_err(&format!("{p} 应被拒绝"))
                .to_string();
            assert!(
                err.contains("不在允许读取的范围内") || err.contains("凭证类"),
                "{p} 入口实得: {err}（不该报「无法访问」—— 那说明策略判定被 canonicalize 的失败盖住了）"
            );
        }
    }

    /// 不存在的越权路径必须报"策略拒绝"，不能报"无法访问"。
    ///
    /// 这条在**开发机上就能跑**（路径不存在是前提，不是障碍），所以它是这个修复的
    /// 主要回归防线：`resolve_readable` 原先先 `canonicalize`，文件不存在就报
    /// "无法访问：No such file"，读的人会往"文件不存在"的方向查，而真因是策略拒了。
    #[test]
    fn nonexistent_out_of_whitelist_reports_policy_not_io() {
        let mut ctx = ToolCtx::default();
        for p in ["/etc/shadow", "/definitely/not/here/nope.txt", "C:/Windows/win.ini"] {
            let err = execute(&gate(), &mut ctx, "read_file", &serde_json::json!({"path": p}), false)
                .expect_err(&format!("{p} 应被拒绝"))
                .to_string();
            assert!(err.contains("不在允许读取的范围内"), "{p} 实得: {err}");
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
    ///
    /// 路径必须**按平台给**：写死 `C:/Windows/...` 在 Linux 上会先撞"父目录不可访问"，
    /// 报的错就不是白名单拒绝 —— 测试会以一个与策略无关的理由失败（构建机实测的
    /// 4 个失败里的第 4 个）。这里两边都选**真实存在**的系统路径，
    /// 保证 canonicalize 能成功、失败原因必然是策略。
    #[test]
    fn write_rejects_paths_outside_whitelist() {
        let mut ctx = ToolCtx::default();
        #[cfg(not(target_os = "linux"))]
        let (outside, inside) = ("C:/Windows/win.ini", "C:/Temp/aether-inside.txt");
        #[cfg(target_os = "linux")]
        let (outside, inside) = ("/etc/passwd", "/tmp/aether-inside.txt");

        for (tool, args) in [
            ("file_write", serde_json::json!({"path": outside, "content": "x"})),
            ("file_delete", serde_json::json!({"path": outside})),
            ("file_rename", serde_json::json!({"from": outside, "to": inside})),
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
        let _t = TrashDir::new("roundtrip");
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
        let _t = TrashDir::new("through");
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
