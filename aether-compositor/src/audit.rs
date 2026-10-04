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

    /// 相对时间的英文版（§3.5）。
    ///
    /// 为什么单独一个函数而不是传语言进来：`audit.rs` 是**数据层**，
    /// 不该依赖 `i18n`（那是界面层的事）。两个函数各自纯、各自可测，
    /// 由界面决定用哪个。
    pub fn age_text_en(&self, now: u64) -> String {
        let d = now.saturating_sub(self.ts);
        if d < 60 {
            "just now".to_string()
        } else if d < 3600 {
            let m = d / 60;
            if m == 1 {
                "1 minute ago".to_string()
            } else {
                format!("{m} minutes ago")
            }
        } else if d < 86_400 {
            let h = d / 3600;
            if h == 1 {
                "1 hour ago".to_string()
            } else {
                format!("{h} hours ago")
            }
        } else {
            let days = d / 86_400;
            if days == 1 {
                "1 day ago".to_string()
            } else {
                format!("{days} days ago")
            }
        }
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

/// 这一页一次读多少条。**与"导出"用同一个上限** ——
/// 界面上看得到的历史，不该比导出的少（以前页面只读 8 条，导出读 200 条）。
pub const PAGE_MAX: usize = 200;

/// 一条记录是否命中查询。**空查询 = 全命中**（不做"空就什么都不显示"的怪事）。
///
/// 搜的字段：工具名 / 参数 / 结论 / 等级。**大小写不敏感** ——
/// 用户不会记得 `read_file` 里有没有下划线、`L2` 是不是大写。
pub fn matches(e: &Entry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    e.tool.to_lowercase().contains(&q)
        || e.args.to_lowercase().contains(&q)
        || e.verdict.to_lowercase().contains(&q)
        || e.level.to_lowercase().contains(&q)
}

/// 命中的**下标**（保留原顺序）。返回下标而不是引用：
/// 调用方多半要同时用"原始条数"和"命中条数"来如实显示 `M / N`。
pub fn matching(entries: &[Entry], query: &str) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches(e, query))
        .map(|(i, _)| i)
        .collect()
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

/// 把审计信息渲染成**可直接贴到 issue** 的纯文本（§5.2 导出）。
///
/// 纯函数（`now` 由调用方给）—— 于是"导出内容包含什么"可以单测，
/// 不必真的写文件。**导出必须包含文件路径与解析失败计数**：前者让读者知道数据来源，
/// 后者防止"日志格式变了但导出看起来一切正常"（那种静默最误导人）。
/// 导出成便于贴进 issue 的纯文本。
///
/// 只在 Linux 与测试下编译：唯一的生产调用点在 `export_audit()` 的 Linux 分支里
/// （非 Linux 分支没有审计文件可写，直接返回错误）。测试要在所有平台跑，
/// 所以带上 `test` —— 否则这个纯函数在开发机上从未被执行过。
#[cfg(any(target_os = "linux", test))]
pub fn export_text(info: &AuditInfo, now: u64, en: bool) -> String {
    let mut s = String::new();
    if en {
        s.push_str("AetherOS audit log export\n");
        s.push_str("========================\n");
        s.push_str(&format!("Source: {}\n", info.path));
        s.push_str(&format!(
            "File exists: {}\n",
            if info.exists { "yes" } else { "no" }
        ));
        s.push_str(&format!(
            "Total: {} · denied: {} · failed: {} · unparsable: {}\n",
            info.total, info.denied, info.failed, info.unparsed
        ));
    } else {
        s.push_str("AetherOS 审计日志导出\n");
        s.push_str("==================\n");
        s.push_str(&format!("来源：{}\n", info.path));
        s.push_str(&format!("文件存在：{}\n", if info.exists { "是" } else { "否" }));
        s.push_str(&format!(
            "总条数：{} · 被拒绝：{} · 失败：{} · 无法解析：{}\n",
            info.total, info.denied, info.failed, info.unparsed
        ));
    }
    if en {
        s.push_str(&format!("Exported at (Unix seconds): {now}\n\n"));
    } else {
        s.push_str(&format!("导出时间（Unix 秒）：{now}\n\n"));
    }
    if info.recent.is_empty() {
        s.push_str(if en { "(no records)\n" } else { "（没有记录）\n" });
        return s;
    }
    s.push_str(if en {
        "Recent records (newest first):\n"
    } else {
        "最近记录（新的在前）：\n"
    });
    for e in &info.recent {
        // 用制表符分隔，便于贴进表格工具；时间同时给"多久以前"（人看）与原始秒（机器看）。
        // 最后一列是**结论的人话**（与 verdict 的机器名并列：一个给人看、一个给脚本看）
        let verdict_zh = if e.is_denied() {
            "被拒绝"
        } else if e.is_failed() {
            "失败"
        } else {
            "正常"
        };
        let verdict_en = if e.is_denied() {
            "denied"
        } else if e.is_failed() {
            "failed"
        } else {
            "ok"
        };
        let age = if en { e.age_text_en(now) } else { e.age_text(now) };
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            e.ts,
            age,
            level_text(
                if en { crate::i18n::Lang::En } else { crate::i18n::Lang::Zh },
                &e.level
            ),
            e.tool,
            e.verdict,
            if en { verdict_en } else { verdict_zh }
        ));
    }
    s
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


/// 审计行里 `level` 那一列的**显示文本**（§3.5）。
///
/// 日志里存的是稳定 token（`L0`…`L3`）。**旧日志**里可能还是「L2 敏感写」
/// 这种人话名 —— 对不认识的值**原样透传**，这样老日志不会显示成空白。
pub fn level_text(lang: crate::i18n::Lang, raw: &str) -> String {
    let en = lang == crate::i18n::Lang::En;
    match raw.trim() {
        "L0" => if en { "L0 read-only" } else { "L0 只读" }.to_string(),
        "L1" => if en { "L1 reversible write" } else { "L1 可逆写" }.to_string(),
        "L2" => if en { "L2 sensitive write" } else { "L2 敏感写" }.to_string(),
        "L3" => if en { "L3 dangerous" } else { "L3 危险" }.to_string(),
        other => other.to_string(),
    }
}


    /// §3.5：日志存 token，显示时取词；**旧日志里的人话名原样透传**。
    #[test]
    fn level_text_maps_tokens_and_passes_through_legacy_names() {
        use crate::i18n::Lang;
        for (raw, zh, en) in [
            ("L0", "L0 只读", "L0 read-only"),
            ("L1", "L1 可逆写", "L1 reversible write"),
            ("L2", "L2 敏感写", "L2 sensitive write"),
            ("L3", "L3 危险", "L3 dangerous"),
        ] {
            assert_eq!(level_text(Lang::Zh, raw), zh, "{raw}");
            assert_eq!(level_text(Lang::En, raw), en, "{raw}");
            assert!(
                !level_text(Lang::En, raw)
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "英文里不该有汉字：{raw}"
            );
        }
        // 旧日志（2026-10-04 之前写的）里是人话名 —— 必须原样显示，不能变空
        assert_eq!(level_text(Lang::En, "L2 敏感写"), "L2 敏感写");
        assert_eq!(level_text(Lang::Zh, "L2 sensitive write"), "L2 sensitive write");
        // 前后空白容错
        assert_eq!(level_text(Lang::Zh, " L1 "), "L1 可逆写");
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
    /// §5.2：导出文本必须**如实包含**来源、计数与解析失败数 ——
    /// 尤其"无法解析"那一项：日志格式变了却看不出来，是最误导人的静默失败。
    #[test]
    fn export_text_is_faithful_about_what_it_contains() {
        let info = AuditInfo {
            path: "/var/log/aether/aether-audit.log".into(),
            exists: true,
            recent: vec![Entry {
                ts: 1000,
                level: "L3".into(),
                tool: "system_power".into(),
                verdict: "denied_no_ui".into(),
                args: String::new(),
            }],
            total: 7,
            denied: 2,
            failed: 1,
            unparsed: 3,
        };
        let out = export_text(&info, 1060, false);
        assert!(out.contains("/var/log/aether/aether-audit.log"), "应含来源路径");
        assert!(out.contains("总条数：7"), "{out}");
        assert!(out.contains("被拒绝：2"), "{out}");
        assert!(out.contains("无法解析：3"), "解析失败数必须如实导出：{out}");
        assert!(out.contains("system_power"), "应含具体记录");
        assert!(out.contains("被拒绝"), "应标出被拒绝的条目");
        // 原始秒必须原样出现在导出里（机器可读那一列由格式保证）
        assert!(out.contains("1000"), "应含原始时间戳：{out}");

        // 空记录也要能导出（导出"什么都没有"是有用信息，不是错误）
        let empty = AuditInfo { exists: false, ..AuditInfo::default() };
        let out2 = export_text(&empty, 0, false);
        assert!(out2.contains("没有记录"), "{out2}");
        assert!(out2.contains("文件存在：否"), "{out2}");
    }

    /// 搜索：空查询全命中、大小写不敏感、四个字段都能搜到、搜不到就是空。
    #[test]
    fn audit_search_covers_the_four_fields_and_is_case_insensitive() {
        let e = Entry {
            ts: 1,
            level: "L2".into(),
            tool: "read_file".into(),
            args: "{/etc/hosts}".into(),
            verdict: "denied_by_user".into(),
        };
        // 空查询（含纯空白）= 全命中
        assert!(matches(&e, ""));
        assert!(matches(&e, "   "));
        // 四个字段都能搜到，且大小写不敏感
        assert!(matches(&e, "read_file"));
        assert!(matches(&e, "READ_FILE"), "大小写不敏感");
        assert!(matches(&e, "l2"), "等级也能搜");
        assert!(matches(&e, "etc/hosts"), "参数也能搜");
        assert!(matches(&e, "denied"), "结论也能搜");
        // 搜不到
        assert!(!matches(&e, "write_file"));
        // 过滤保留原顺序，且返回的是下标
        let list = vec![
            e.clone(),
            Entry { tool: "clipboard_set".into(), ..e.clone() },
            Entry { tool: "read_file".into(), ts: 9, ..e.clone() },
        ];
        assert_eq!(matching(&list, "read_file"), vec![0, 2], "保留原顺序");
        assert_eq!(matching(&list, ""), vec![0, 1, 2], "空查询=全命中");
        assert!(matching(&list, "nope").is_empty());
    }

    /// §3.5：英文导出**必须是英文**，且**不能少字段**。
    ///
    /// 这条测试盯的是"半翻译"：头换了英文、条目行还是中文 —— 那种导出
    /// 贴进 issue 会更让人困惑。所以逐项断言英文关键词都在。
    #[test]
    fn english_export_is_fully_english() {
        let info = AuditInfo {
            path: "/var/log/aether/aether-audit.log".into(),
            exists: true,
            recent: vec![Entry {
                ts: 1000,
                level: "L2".into(),
                tool: "read_file".into(),
                args: "{/etc/hosts}".into(),
                verdict: "denied_by_user".into(),
            }],
            total: 1,
            denied: 1,
            failed: 0,
            unparsed: 0,
        };
        let en = export_text(&info, 2000, true);
        for needle in [
            "AetherOS audit log export",
            "Source:",
            "File exists: yes",
            "Total: 1",
            "denied: 1",
            "Exported at (Unix seconds): 2000",
            "Recent records (newest first):",
            "16 minutes ago",
        ] {
            assert!(en.contains(needle), "英文导出缺少「{needle}」：\n{en}");
        }
        // 不允许残留中文（半翻译是最糟的形态）
        assert!(
            !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "英文导出里不该有汉字：\n{en}"
        );
        // 中文那份不受影响
        let zh = export_text(&info, 2000, false);
        assert!(zh.contains("审计日志导出") && zh.contains("被拒绝"), "中文导出不该被改坏");
        // 空记录也要分语言
        let empty = AuditInfo { exists: true, ..Default::default() };
        assert!(export_text(&empty, 0, true).contains("(no records)"));
        assert!(export_text(&empty, 0, false).contains("（没有记录）"));
    }

    /// §3.5：`age_text_en` 的单复数（"1 minute ago" 不是 "1 minutes ago"）。
    #[test]
    fn english_age_text_handles_singular_and_plural() {
        let mk = |ts: u64| Entry {
            ts,
            level: "L0".into(),
            tool: "t".into(),
            args: String::new(),
            verdict: "allowed".into(),
        };
        let now = 1_000_000u64;
        assert_eq!(mk(now - 5).age_text_en(now), "just now");
        assert_eq!(mk(now - 60).age_text_en(now), "1 minute ago");
        assert_eq!(mk(now - 120).age_text_en(now), "2 minutes ago");
        assert_eq!(mk(now - 3600).age_text_en(now), "1 hour ago");
        assert_eq!(mk(now - 7200).age_text_en(now), "2 hours ago");
        assert_eq!(mk(now - 86_400).age_text_en(now), "1 day ago");
        assert_eq!(mk(now - 172_800).age_text_en(now), "2 days ago");
        // 时钟回拨（ts 在未来）也要给"just now"，不能算成负数天
        assert_eq!(mk(now + 999).age_text_en(now), "just now");
    }

}
