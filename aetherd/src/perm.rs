//! 权限模型与审计日志 —— docs/ai-permissions.md 的实现。
//!
//! 原则：AI 无所不知（可读），但不会不问就动手（受控可写）。

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    /// 只读：查状态、读文件
    L0 = 0,
    /// 可逆写：改壁纸、开关服务
    L1 = 1,
    /// 敏感写：装卸软件、改设置、删用户文件（需确认）
    L2 = 2,
    /// 危险：格式化、改内核参数、批量删除（双重确认）
    L3 = 3,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Level::L0 => "L0 只读",
            Level::L1 => "L1 可逆写",
            Level::L2 => "L2 敏感写",
            Level::L3 => "L3 危险",
        };
        f.write_str(s)
    }
}

/// 工具执行请求经过的权限闸门。
pub struct Gate {
    /// 审计日志路径（追加写）
    audit_path: PathBuf,
    /// 测试/无人值守模式下，允许的最高自动通过等级
    pub auto_approve_below: Level,
    /// 被用户**明确拒绝**过的操作指纹 → 拒绝时刻。
    ///
    /// 存在意义：让"用户拒绝了"成为服务端的一个持久事实。在此之前，
    /// 合成器的"拒绝"只是本地 UI 状态（不发 IPC），服务端既不撤销令牌，
    /// 也不知道发生过拒绝，`Verdict::Denied` 因此永远不可达。
    denied: Mutex<HashMap<String, Instant>>,
}

/// 用户拒绝后，同一操作在此时长内不再重复询问（直接 Denied）。
const DENY_TTL: Duration = Duration::from_secs(300);

/// 审计日志大小上限（与服务日志的 `logtee::MAX_LOG_BYTES` 取同一值，便于记忆）。
const MAX_AUDIT_BYTES: u64 = 8 * 1024 * 1024;

/// 审计裁决列取值。
///
/// 前 5 个与 `docs/ai-permissions.md` 的表格一致；`REJECTED_NO_UI` 是第四轮审查
/// 新增的第 6 个 —— "未注册通道访问剪贴板"这类**入侵尝试**必须能与
/// "用户拒绝过"（`denied`）区分开，否则审计无法回答"有没有人在敲门"。
pub mod verdict {
    pub const ALLOWED: &str = "allowed";
    pub const NEEDS_CONFIRMATION: &str = "needs_confirmation";
    pub const DENIED_BY_USER: &str = "denied_by_user";
    pub const DENIED: &str = "denied";
    pub const FAILED: &str = "failed";
    /// 未注册为 UI 通道的连接发起了受限请求（剪贴板 / L2+ 令牌兑现）。
    pub const REJECTED_NO_UI: &str = "rejected_no_ui";
}

/// 闸门裁决结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 直接执行
    Allowed,
    /// 需要用户确认（UI 弹卡片；确认后带 approval token 重试）
    NeedsConfirmation,
    /// 拒绝执行（用户已明确拒绝过同一操作）
    Denied(String),
}

/// (tool, arguments) 的规范化指纹。`serde_json::Value::Object` 是 BTreeMap，
/// 键序稳定，因此同样的参数总是得到同样的指纹。
fn deny_key(tool: &str, args: &serde_json::Value) -> String {
    format!("{tool}\u{1}{args}")
}

impl Gate {
    /// 审计日志固定在 /var/log/aether/（持久化分区挂载点），跨重启保留。
    ///
    /// 权限：审计日志含工具参数（路径、探针名等），虽已对剪贴板/文件内容脱敏
    /// （见 `tools::audit_args`），仍不该让同机其它用户读到 —— 目录 0700、文件 0600。
    /// 2026-10-02 审计 M-2：此前两者都靠 umask（推定 0644），与 `ui.key`/`model.json`
    /// 的显式 0600 不一致。
    pub fn new(audit_path: PathBuf) -> Self {
        if let Some(parent) = audit_path.parent() {
            let _ = std::fs::create_dir_all(parent);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        Self {
            audit_path,
            auto_approve_below: Level::L2,
            denied: Mutex::new(HashMap::new()),
        }
    }

    /// 测试专用：不落盘审计日志。
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            audit_path: PathBuf::from(""),
            auto_approve_below: Level::L2,
            denied: Mutex::new(HashMap::new()),
        }
    }

    /// 记下"用户拒绝了这个操作"，并清理过期项。
    pub fn mark_denied(&self, tool: &str, args: &serde_json::Value) {
        let mut map = self.denied.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, t| t.elapsed() < DENY_TTL);
        map.insert(deny_key(tool, args), Instant::now());
    }

    /// 该操作是否处于"已被用户拒绝"的冷却期内。
    pub fn is_denied(&self, tool: &str, args: &serde_json::Value) -> bool {
        let mut map = self.denied.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, t| t.elapsed() < DENY_TTL);
        map.contains_key(&deny_key(tool, args))
    }

    /// 裁决一次工具调用。
    ///
    /// 顺序要紧：**先看"用户是否已经拒绝过"**，再走等级闸门。否则被拒绝的
    /// 操作会立刻重新弹卡片，用户永远摆脱不掉同一个请求。
    pub fn judge(&self, tool: &str, level: Level, args: &serde_json::Value, approved: bool) -> Verdict {
        if !approved && self.is_denied(tool, args) {
            return Verdict::Denied(format!(
                "该操作已被用户拒绝（{tool}），{} 秒内不再重复询问",
                DENY_TTL.as_secs()
            ));
        }
        if level >= self.auto_approve_below && !approved {
            return Verdict::NeedsConfirmation;
        }
        Verdict::Allowed
    }

    /// 追加一条审计记录。审计失败不阻断执行，但会显式报错给调用方记录。
    ///
    /// `docs/ai-permissions.md` 的承诺是"审计写入失败不阻断执行，但**不允许静默
    /// 零留痕**" —— 所以调用方**不应**忽略这里的返回值：至少要在失败时打一条
    /// stderr，否则"审计静默停止"会变成一个查不出原因的现象。
    pub fn audit(&self, tool: &str, level: Level, args: &str, verdict: &str) -> std::io::Result<()> {
        self.rotate_if_needed();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        // 只在**创建**时生效；已存在的旧文件（历史上按 umask 建的可能更宽）下面再收一次。
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&self.audit_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        writeln!(f, "{ts}\t{level}\t{tool}\t{args}\t{verdict}")
    }

    /// 审计日志超过上限就轮转（旧档只留一份）。
    ///
    /// 为什么需要：审计日志是**追加写且无上限**的，而 aetherd 是常驻守护进程
    /// （`restart` 默认 true）。长期运行必然撑满持久化分区，届时 `audit()` 会
    /// 开始失败 —— 若调用方忽略返回值，安全机制就静默蒸发了。
    /// 第四轮审查（P2-2）发现：服务日志有 8MB 轮转，**安全关键的审计日志反而没有**。
    ///
    /// 与 `aether-init/src/logtee.rs::rotate_in` 同构。没有提取成共享工具是因为
    /// 两者分属不同 crate，为此新建一个 crate 不划算 —— **若将来出现第三处，就该提取**。
    ///
    /// 轮转失败不阻断本次写入（下一次写入会再试）；`for_test()` 的空路径直接跳过。
    fn rotate_if_needed(&self) {
        if self.audit_path.as_os_str().is_empty() {
            return;
        }
        let Ok(meta) = std::fs::metadata(&self.audit_path) else {
            return; // 文件还不存在：本次写入会创建它
        };
        if meta.len() <= MAX_AUDIT_BYTES {
            return;
        }
        let old = PathBuf::from(format!("{}.1", self.audit_path.display()));
        let _ = std::fs::remove_file(&old); // 旧档只留一份
        let _ = std::fs::rename(&self.audit_path, &old);
    }
}

/// 一次性确认令牌表。
///
/// 存在意义是让"用户确认过"成为 AI 无法伪造的事实：令牌只经 IPC 发给
/// UI 客户端，绝不进入 LLM 上下文，因此模型无法自我授权。校验时绑定
/// (tool, arguments)，用后即废，并有 5 分钟时效。
#[derive(Default)]
pub struct Approvals {
    inner: Mutex<HashMap<String, Pending>>,
}

struct Pending {
    tool: String,
    args: serde_json::Value,
    issued: Instant,
}

/// 令牌有效期：超过即失效（用户离开后回来点确认不应仍然生效）。
const TOKEN_TTL: Duration = Duration::from_secs(300);

impl Approvals {
    pub fn new() -> Self {
        Self::default()
    }

    /// 签发一次性令牌（同时清理过期项，避免长期运行内存增长）。
    pub fn issue(&self, tool: &str, args: &serde_json::Value) -> String {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, p| p.issued.elapsed() < TOKEN_TTL);
        let token = random_token();
        map.insert(
            token.clone(),
            Pending { tool: tool.to_string(), args: args.clone(), issued: Instant::now() },
        );
        token
    }

    /// 校验并消费令牌。绑定 (tool, arguments)：换个工具或改一个参数都算不匹配。
    pub fn redeem(&self, token: &str, tool: &str, args: &serde_json::Value) -> Result<(), String> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(p) = map.remove(token) else {
            return Err("确认令牌无效或已被使用".into());
        };
        if p.issued.elapsed() >= TOKEN_TTL {
            return Err("确认令牌已过期，请重新确认".into());
        }
        if p.tool != tool {
            return Err(format!("确认令牌与工具不匹配（令牌 {} ≠ 请求 {tool}）", p.tool));
        }
        if p.args != *args {
            return Err("确认令牌与参数不匹配（参数在确认后被改动）".into());
        }
        Ok(())
    }

    /// 撤销令牌（用户点了"拒绝"）。
    ///
    /// 返回被撤销的 (tool, arguments)：上层据此落一条 `denied_by_user` 审计，
    /// 并把该操作记入 Gate 的拒绝冷却。令牌不存在时返回 None（重复拒绝/已过期）。
    pub fn revoke(&self, token: &str) -> Option<(String, serde_json::Value)> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(token).map(|p| (p.tool, p.args))
    }
}

/// 128 位随机令牌（标准库随机哈希种子，无额外依赖）。
pub(crate) fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut h1 = std::collections::hash_map::RandomState::new().build_hasher();
    h1.write_u64(now);
    let a = h1.finish();
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(!a);
    format!("{a:016x}{:016x}", h2.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> Gate {
        Gate::for_test()
    }

    fn args() -> serde_json::Value {
        serde_json::json!({"disk": "/dev/vda"})
    }

    #[test]
    fn read_ops_pass_through() {
        let g = gate();
        assert_eq!(g.judge("read_file", Level::L0, &args(), false), Verdict::Allowed);
        assert_eq!(g.judge("desktop", Level::L1, &args(), false), Verdict::Allowed);
    }

    #[test]
    fn sensitive_ops_need_confirmation() {
        let g = gate();
        assert_eq!(g.judge("install_disk", Level::L2, &args(), false), Verdict::NeedsConfirmation);
        assert_eq!(g.judge("install_disk", Level::L3, &args(), false), Verdict::NeedsConfirmation);
        assert_eq!(g.judge("install_disk", Level::L2, &args(), true), Verdict::Allowed);
    }

    #[test]
    fn order_is_total() {
        assert!(Level::L0 < Level::L1 && Level::L1 < Level::L2 && Level::L2 < Level::L3);
    }

    /// P1-9：`Verdict::Denied` 必须可达 —— 用户拒绝过同一操作后，
    /// 该操作在冷却期内直接 Denied，而不是再次弹卡片。
    #[test]
    fn user_denial_makes_denied_reachable() {
        let g = gate();
        let a = args();
        // 拒绝前：L3 走确认流程
        assert_eq!(g.judge("install_disk", Level::L3, &a, false), Verdict::NeedsConfirmation);
        // 用户拒绝
        g.mark_denied("install_disk", &a);
        // 拒绝后：同工具同参数 → Denied（带原因）
        match g.judge("install_disk", Level::L3, &a, false) {
            Verdict::Denied(reason) => assert!(reason.contains("已被用户拒绝"), "实得 {reason}"),
            other => panic!("应被拒绝，实得 {other:?}"),
        }
        // 换参数不受影响（只拒绝被拒过的那一个操作）
        let other = serde_json::json!({"disk": "/dev/sdb"});
        assert_eq!(g.judge("install_disk", Level::L3, &other, false), Verdict::NeedsConfirmation);
        // 已获令牌的放行不受冷却影响（approved 优先）
        assert_eq!(g.judge("install_disk", Level::L3, &a, true), Verdict::Allowed);
    }

    #[test]
    fn token_redeems_once_and_binds_tool_and_args() {
        let a = Approvals::new();
        let args = serde_json::json!({"disk": "/dev/vda", "confirm": "/dev/vda"});
        let tok = a.issue("install_disk", &args);
        assert!(a.redeem(&tok, "install_disk", &args).is_ok());
        // 一次性：再次使用必须失败（防重放）
        assert!(a.redeem(&tok, "install_disk", &args).is_err());
    }

    #[test]
    fn token_rejects_changed_args_or_tool() {
        let a = Approvals::new();
        let args = serde_json::json!({"disk": "/dev/vda"});
        let tok = a.issue("install_disk", &args);
        // 同令牌换目标盘：必须拒绝（参数在确认后被改动）
        let other = serde_json::json!({"disk": "/dev/sda"});
        assert!(a.redeem(&tok, "install_disk", &other).is_err());
        assert!(a.redeem(&tok, "desktop", &args).is_err());
    }

    #[test]
    fn unknown_token_rejected() {
        let a = Approvals::new();
        assert!(a.redeem("deadbeef", "install_disk", &serde_json::json!({})).is_err());
    }

    #[test]
    fn tokens_are_distinct() {
        let a = Approvals::new();
        let args = serde_json::json!({});
        assert_ne!(a.issue("t", &args), a.issue("t", &args));
    }

    /// P1-9：撤销后令牌立即失效，且 revoke 回传被拒操作的完整身份（供审计）。
    #[test]
    fn revoke_invalidates_token_and_reports_identity() {
        let a = Approvals::new();
        let args = serde_json::json!({"disk": "/dev/vda"});
        let tok = a.issue("install_disk", &args);
        let (tool, got_args) = a.revoke(&tok).expect("撤销应返回被拒操作");
        assert_eq!(tool, "install_disk");
        assert_eq!(got_args, args);
        // 撤销后不可兑现（不能"拒绝完又放行"）
        assert!(a.redeem(&tok, "install_disk", &args).is_err());
        // 重复撤销：返回 None，不 panic
        assert!(a.revoke(&tok).is_none());
    }

    /// P2-2：审计日志超过上限要轮转，且轮转后**新日志只含新记录**。
    #[test]
    fn audit_rotates_when_oversized() {
        let dir = std::env::temp_dir().join("aether_audit_rotate_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("aether-audit.log");
        let g = Gate::new(path.clone());

        // set_len 造超限文件是 O(1)，不用真写 8MB
        std::fs::File::create(&path)
            .expect("建日志文件")
            .set_len(MAX_AUDIT_BYTES + 1)
            .expect("扩容");

        g.audit("read_file", Level::L0, "", "allowed").expect("审计应成功");

        let archived = std::fs::metadata(format!("{}.1", path.display()))
            .expect("旧内容应被归档到 .1");
        assert_eq!(archived.len(), MAX_AUDIT_BYTES + 1, "旧内容应整体归档");
        let now = std::fs::read_to_string(&path).expect("新日志可读");
        assert!(now.contains("read_file"), "新日志应含新写入的一条：{now:?}");
        assert!(now.len() < 1000, "新日志不应含旧内容（只有一条记录）：{now:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 审计文件不存在时不应因轮转检查而失败 —— 首次写入要能正常创建。
    #[test]
    fn audit_creates_file_when_absent() {
        let dir = std::env::temp_dir().join("aether_audit_create_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("aether-audit.log");
        let g = Gate::new(path.clone());

        g.audit("clipboard_get", Level::L1, "", "allowed").expect("首次写入应成功");
        assert!(path.exists(), "审计文件应被创建");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("clipboard_get"), "实得 {content:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M-2：审计日志与它的目录必须是属主独占（0600 / 0700）。
    ///
    /// 只在 Unix 上可测（Windows 没有 POSIX 权限位），因此这条在开发机上是
    /// "编译期存在、运行期不执行" —— 构建机/目标机上才会真正跑。
    #[cfg(unix)]
    #[test]
    fn audit_file_and_dir_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("aether_audit_perm_test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("aether-audit.log");
        let g = Gate::new(path.clone());
        g.audit("read_file", Level::L0, "{}", "allowed").expect("审计应成功");

        let file_mode = std::fs::metadata(&path).expect("文件应存在").permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "审计文件应为 0600，实得 {file_mode:o}");
        let dir_mode = std::fs::metadata(&dir).expect("目录应存在").permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "审计目录应为 0700，实得 {dir_mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
