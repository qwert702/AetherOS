//! 审计日志读取（P4.3 设置中心「权限与隐私」页）。
//!
//! ## 数据来源与"为什么不走 IPC"
//!
//! `aetherd` 的权限门 `perm::Gate` 把每次工具调用的判定**追加**写入
//! `/var/log/aether/aether-audit.log`，格式就是它 `audit()` 里那一行：
//!
//! ```text
//! {unix秒}\t{等级}\t{工具}\t{参数}\t{结论}
//! ```
//!
//! 合成器是 root，和读 `/var/apps`（已装应用）、`/proc/net/*`（网络状态）一样，
//! 这个文件它**本来就能读**。所以这里直接读真值，**不新增 IPC 变体** ——
//! 新增变体要同步 `aetherd/src/server.rs` 的鉴权分支与 `request_variants_are_gated` 清单，
//! 而这一页只做**只读展示**，没有需要跨进程授权的新能力。
//!
//! ## 解析是纯函数
//!
//! [`parse_line`] 收文本不读文件，因此能在开发机上用样例行单测。

/// 一条审计记录。
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    /// Unix 秒
    pub ts: u64,
    /// 权限等级（`L0` / `L1` / `L2`）
    pub level: String,
    /// 工具名（`read_file` / `clipboard_set` …）
    pub tool: String,
    /// 参数（可能是 JSON，展示时会截断）
    pub args: String,
    /// 结论：`allowed` / `denied_by_user` / `rejected_no_ui` / `failed` …
    pub verdict: String,
}

impl Entry {
    /// 结论是否属于"被拒绝"（用户拒绝 + 无 UI 可确认时拒绝）。
    pub fn is_denied(&self) -> bool {
        self.verdict.contains("denied") || self.verdict.contains("rejected")
    }

    /// 结论是否属于"失败"。
    pub fn is_failed(&self) -> bool {
        self.verdict == "failed"
    }

    /// 相对时间描述。刻意**不显示绝对日期**：审计时间戳是 Unix 秒，
    /// 转本地日历时间需要一套历法换算，而这一页真正想回答的是"多久以前"。
    pub fn age_text(&self, now: u64) -> String {
        let d = now.saturating_sub(self.ts);
        if d < 60 {
            "刚刚".to_string()
        } else if d < 3600 {
            format!("{} 分钟前", d / 60)
        } else if d < 86_400 {
            format!("{} 小时前", d / 3600)
        } else {
            format!("{} 天前", d / 86_400)
        }
    }
}

/// 整页要显示的东西。
#[derive(Clone, PartialEq, Debug, Default)]
pub struct AuditInfo {
    /// 日志文件路径（原样显示，让用户知道数据从哪来）
    pub path: String,
    /// 文件是否存在（不存在时界面显示"暂无记录"，不是错误）
    pub exists: bool,
    /// 最近的记录（新的在前）
    pub recent: Vec<Entry>,
    /// 总条数
    pub total: usize,
    /// 其中被拒绝的条数
    pub denied: usize,
    /// 其中失败的条数
    pub failed: usize,
    /// 行格式不对而被跳过的条数（**显式计数**，不静默丢弃）
    pub unparsed: usize,
}

/// 解析一行审计记录。字段数不对或时间戳不是数字 → `None`。
pub fn parse_line(line: &str) -> Option<Entry> {
    let f: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
    if f.len() < 5 {
        return None;
    }
    let ts = f[0].trim().parse::<u64>().ok()?;
    Some(Entry {
        ts,
        level: f[1].trim().to_string(),
        tool: f[2].trim().to_string(),
        args: f[3].to_string(),
        verdict: f[4].trim().to_string(),
    })
}

/// 解析整份日志，返回（全部记录, 无法解析的行数）。顺序保持文件顺序。
pub fn parse_log(text: &str) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let mut bad = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Some(e) => out.push(e),
            None => bad += 1,
        }
    }
    (out, bad)
}

/// 日志路径（与 `aetherd` 的 `Gate::new` 一致；轮转档是 `.1`）。
pub const AUDIT_PATH: &str = "/var/log/aether/aether-audit.log";

/// 读日志并汇总。`limit` 是"最近多少条"要送进界面。
///
/// 读不到文件**不是错误**：Live 会话里可能还没产生任何工具调用，
/// 这时 `exists=false`，界面显示"暂无审计记录"。
pub fn read(path: &str, limit: usize) -> AuditInfo {
    let mut info = AuditInfo {
        path: path.to_string(),
        ..AuditInfo::default()
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return info;
    };
    info.exists = true;
    let (all, bad) = parse_log(&text);
    info.unparsed = bad;
    info.total = all.len();
    info.denied = all.iter().filter(|e| e.is_denied()).count();
    info.failed = all.iter().filter(|e| e.is_failed()).count();
    // 新的在前
    info.recent = all.into_iter().rev().take(limit).collect();
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1780000000\tL1\tclipboard_get\t\tallowed\n\
                          1780000010\tL2\twrite_file\t{\"path\":\"/etc/x\"}\tdenied_by_user\n\
                          1780000020\tL2\treload_config\t\trejected_no_ui\n\
                          1780000030\tL1\trun_shell\tls\tfailed\n\
                          这行是坏的\n\
                          \n";

    #[test]
    fn parses_fields_in_order() {
        let e = parse_line("1780000000\tL1\tclipboard_get\t\tallowed").expect("应能解析");
        assert_eq!(e.ts, 1_780_000_000);
        assert_eq!(e.level, "L1");
        assert_eq!(e.tool, "clipboard_get");
        assert_eq!(e.args, "");
        assert_eq!(e.verdict, "allowed");
    }

    /// 坏行必须被**计数**而不是静默丢弃 —— 审计页最不该做的事就是假装没有记录。
    #[test]
    fn bad_lines_are_counted_not_silently_dropped() {
        let (all, bad) = parse_log(SAMPLE);
        assert_eq!(all.len(), 4);
        assert_eq!(bad, 1);
        assert_eq!(parse_line("缺字段\tL1"), None);
        assert_eq!(parse_line("不是时间戳\tL1\ttool\t\tallowed"), None);
    }

    #[test]
    fn verdict_classification() {
        let (all, _) = parse_log(SAMPLE);
        assert_eq!(all.iter().filter(|e| e.is_denied()).count(), 2, "用户拒绝 + 无 UI 拒绝");
        assert_eq!(all.iter().filter(|e| e.is_failed()).count(), 1);
        assert!(!all[0].is_denied(), "allowed 不算拒绝");
    }

    /// 相对时间：只回答"多久以前"，不编造日历时间。
    #[test]
    fn age_text_buckets() {
        let mk = |ts: u64| Entry {
            ts,
            level: "L0".into(),
            tool: "t".into(),
            args: String::new(),
            verdict: "allowed".into(),
        };
        let now = 1_000_000u64;
        assert_eq!(mk(now - 5).age_text(now), "刚刚");
        assert_eq!(mk(now - 120).age_text(now), "2 分钟前");
        assert_eq!(mk(now - 7200).age_text(now), "2 小时前");
        assert_eq!(mk(now - 172_800).age_text(now), "2 天前");
        // 时间戳在未来（时钟被调过）不能 panic，也不能显示负数
        assert_eq!(mk(now + 999).age_text(now), "刚刚");
    }

    /// 读不到文件时：`exists=false`、计数全 0，**不报错也不 panic**。
    #[test]
    fn missing_file_is_not_an_error() {
        let i = read("/nonexistent/aether-audit.log", 5);
        assert!(!i.exists);
        assert_eq!(i.total, 0);
        assert!(i.recent.is_empty());
        assert_eq!(i.unparsed, 0);
    }

    /// 真的写一份日志再读回来（含 `.1` 之外的正常路径）。
    #[test]
    fn reads_real_file_and_keeps_newest_first() {
        let dir = std::env::temp_dir().join(format!("aether-audit-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let p = dir.join("aether-audit.log");
        std::fs::write(&p, SAMPLE).ok();
        let i = read(p.to_str().unwrap(), 3);
        assert!(i.exists);
        assert_eq!(i.total, 4);
        assert_eq!(i.denied, 2);
        assert_eq!(i.failed, 1);
        assert_eq!(i.unparsed, 1);
        assert_eq!(i.recent.len(), 3);
        assert_eq!(i.recent[0].ts, 1_780_000_030, "最新的在最前");
        std::fs::remove_dir_all(&dir).ok();
    }
}
