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
#[allow(dead_code)] // L1/L3/Denied 为完整权限模型的一部分，随 M4 二阶段工具接入
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
}

/// 闸门裁决结果。
#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Verdict {
    /// 直接执行
    Allowed,
    /// 需要用户确认（UI 弹卡片；确认后带 approval token 重试）
    NeedsConfirmation,
    /// 拒绝执行
    Denied(String),
}

impl Gate {
    /// 审计日志固定在 /var/log/aether/（持久化分区挂载点），跨重启保留。
    pub fn new(audit_path: PathBuf) -> Self {
        if let Some(parent) = audit_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self { audit_path, auto_approve_below: Level::L2 }
    }

    /// 测试专用：不落盘审计日志。
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self { audit_path: PathBuf::from(""), auto_approve_below: Level::L2 }
    }

    /// 裁决一次工具调用。
    pub fn judge(&self, level: Level, approved: bool) -> Verdict {
        if level >= self.auto_approve_below && !approved {
            return Verdict::NeedsConfirmation;
        }
        Verdict::Allowed
    }

    /// 追加一条审计记录。审计失败不阻断执行，但会显式报错给调用方记录。
    pub fn audit(&self, tool: &str, level: Level, args: &str, verdict: &str) -> std::io::Result<()> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)?;
        writeln!(f, "{ts}\t{level}\t{tool}\t{args}\t{verdict}")
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
}

/// 128 位随机令牌（标准库随机哈希种子，无额外依赖）。
fn random_token() -> String {
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
        Gate { audit_path: PathBuf::from(""), auto_approve_below: Level::L2 }
    }

    #[test]
    fn read_ops_pass_through() {
        let g = gate();
        assert_eq!(g.judge(Level::L0, false), Verdict::Allowed);
        assert_eq!(g.judge(Level::L1, false), Verdict::Allowed);
    }

    #[test]
    fn sensitive_ops_need_confirmation() {
        let g = gate();
        assert_eq!(g.judge(Level::L2, false), Verdict::NeedsConfirmation);
        assert_eq!(g.judge(Level::L3, false), Verdict::NeedsConfirmation);
        assert_eq!(g.judge(Level::L2, true), Verdict::Allowed);
    }

    #[test]
    fn order_is_total() {
        assert!(Level::L0 < Level::L1 && Level::L1 < Level::L2 && Level::L2 < Level::L3);
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
}
