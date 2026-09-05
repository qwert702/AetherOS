//! 权限模型与审计日志 —— docs/ai-permissions.md 的实现。
//!
//! 原则：AI 无所不知（可读），但不会不问就动手（受控可写）。

use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub fn new(audit_path: PathBuf) -> Self {
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
}
