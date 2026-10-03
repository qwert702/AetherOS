//! 「工具权限」页的视图状态（FEATURE-GAPS §5.3）。
//!
//! 与相邻模块的分工：`aether-ipc` 定义线上类型，`aetherd` 存策略并判定，
//! **本模块只管界面这一侧**：拿到列表怎么显示、点一下之后变成什么。
//!
//! 因此这里全是纯逻辑（收列表、收点击，不碰 IPC/绘制），能在开发机上测全。
//! 异步读写由事件循环负责（照 `set_dns` 的套路：独立线程 + 超时 + 事件回执）。

// 绘制侧（`draw_settings_content`）在 Windows 上也会用到本模块；
// 但「点击 → 发请求 → 回执落地」这条链在 Linux 的 fbdev 事件循环里，
// 因此非目标平台上这些方法只有测试在用 —— 与 `auth`/`lock`/`palette` 同一处理。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use aether_ipc::{PolicyEntryWire, ToolPolicyWire};

/// 权限页的状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyView {
    entries: Vec<PolicyEntryWire>,
    /// 是否已经从 aetherd 取到过列表。
    ///
    /// 与"列表为空"分开：**取不到**和**一个工具都没有**是两件事，
    /// 界面必须能说清是哪一种（否则用户看到空白会以为系统没有工具）。
    loaded: bool,
    /// 最近一次失败的原因（取列表或写策略）。成功时清空。
    error: Option<String>,
    /// 正在等待回执的那一行（工具名）。界面据此显示"保存中…"，
    /// 也避免用户连点造成并发写。
    pending: Option<String>,
}

impl PolicyView {
    pub fn entries(&self) -> &[PolicyEntryWire] {
        &self.entries
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 收到列表（成功路径）。
    pub fn set_entries(&mut self, entries: Vec<PolicyEntryWire>) {
        self.entries = entries;
        self.loaded = true;
        self.error = None;
        // 列表回来了，说明没有正在等的回执了
        self.pending = None;
    }

    /// 记录一次失败（取列表失败或写策略失败）。**保留旧列表**：
    /// 写失败时把已有列表清掉，用户会以为"工具都没了"。
    pub fn set_error(&mut self, why: String) {
        self.error = Some(why);
        self.pending = None;
        // 取列表失败时 `loaded` 仍为 false（界面上要说"还没取到"而不是"没有工具"）
    }

    /// 标记正在等某一行的回执。
    pub fn mark_pending(&mut self, tool: &str) {
        self.pending = Some(tool.to_string());
        self.error = None;
    }

    /// 点击第 `i` 行：返回**要把哪个工具设成什么**。越界或正在等回执 → `None`。
    ///
    /// 循环顺序：按等级 → 每次询问 → 允许 → 禁止 → 按等级。
    /// 把「按等级」放在循环里（而不是只作为初始值）：用户改错了要能改回去，
    /// 而"改回去"必须是一次点击就能做到的事。
    pub fn cycle(&self, i: usize) -> Option<(String, ToolPolicyWire)> {
        let e = self.entries.get(i)?;
        // 等回执期间不再接受点击：连点会发多个请求，回执顺序无法保证，
        // 最后落盘的可能不是用户最后想要的那个。
        if self.pending.is_some() {
            return None;
        }
        Some((e.tool.clone(), next_policy(e.policy)))
    }

    /// 把某一行更新为服务器确认后的策略（回执落地）。
    pub fn apply_confirmed(&mut self, tool: &str, policy: ToolPolicyWire) {
        for e in self.entries.iter_mut() {
            if e.tool == tool {
                e.policy = policy;
            }
        }
        self.pending = None;
        self.error = None;
    }
}

/// 四态的循环后继。
pub fn next_policy(p: ToolPolicyWire) -> ToolPolicyWire {
    match p {
        ToolPolicyWire::Default => ToolPolicyWire::Ask,
        ToolPolicyWire::Ask => ToolPolicyWire::Allow,
        ToolPolicyWire::Allow => ToolPolicyWire::Deny,
        ToolPolicyWire::Deny => ToolPolicyWire::Default,
    }
}

/// 等级的中文说明（界面直接用；顺序与 `perm::Level` 一致）。
pub fn level_label(level: u8) -> &'static str {
    match level {
        0 => "L0 只读",
        1 => "L1 可逆写",
        2 => "L2 敏感写",
        _ => "L3 危险",
    }
}

pub fn level_label_en(level: u8) -> &'static str {
    match level {
        0 => "L0 read-only",
        1 => "L1 reversible",
        2 => "L2 sensitive",
        _ => "L3 dangerous",
    }
}

/// 策略的界面文案。
pub fn policy_label(p: ToolPolicyWire) -> &'static str {
    match p {
        ToolPolicyWire::Default => "按等级",
        ToolPolicyWire::Ask => "每次询问",
        ToolPolicyWire::Allow => "允许",
        ToolPolicyWire::Deny => "禁止",
    }
}

pub fn policy_label_en(p: ToolPolicyWire) -> &'static str {
    match p {
        ToolPolicyWire::Default => "By level",
        ToolPolicyWire::Ask => "Always ask",
        ToolPolicyWire::Allow => "Allow",
        ToolPolicyWire::Deny => "Deny",
    }
}

impl PolicyView {
    /// 一行的副标题（按语言）。
    pub fn row_hint(&self, e: &PolicyEntryWire, lang: crate::i18n::Lang) -> String {
        let lvl = crate::i18n::pick(lang, level_label(e.level), level_label_en(e.level));
        let pol = crate::i18n::pick(lang, policy_label(e.policy), policy_label_en(e.policy));
        if self.pending.as_deref() == Some(e.tool.as_str()) {
            return format!("{lvl} · {}", crate::i18n::tr("保存中…", "Saving…"));
        }
        let next = crate::i18n::pick(lang, policy_label(next_policy(e.policy)), policy_label_en(next_policy(e.policy)));
        let click = crate::i18n::pick(lang, "点击改为", "click for");
        format!("{lvl} · {pol} · {click} {next}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tool: &str, level: u8, policy: ToolPolicyWire) -> PolicyEntryWire {
        PolicyEntryWire { tool: tool.to_string(), level, policy }
    }

    #[test]
    fn starts_unloaded_and_distinguishes_empty_from_failed() {
        let v = PolicyView::default();
        assert!(!v.is_loaded());
        assert!(v.is_empty());
        // 关键区分：还没取到 ≠ 没有工具
        assert!(v.error().is_none());
        let mut v2 = PolicyView::default();
        v2.set_error("连接失败".into());
        assert!(!v2.is_loaded(), "取失败时不该变成已加载");
        assert_eq!(v2.error(), Some("连接失败"));
    }

    #[test]
    fn cycle_order_is_default_ask_allow_deny_and_back() {
        use ToolPolicyWire as W;
        assert_eq!(next_policy(W::Default), W::Ask);
        assert_eq!(next_policy(W::Ask), W::Allow);
        assert_eq!(next_policy(W::Allow), W::Deny);
        // 转一圈回到起点：用户改错了要能改回去（一次点击就够）
        assert_eq!(next_policy(next_policy(next_policy(next_policy(W::Default)))), W::Default);
    }

    #[test]
    fn clicking_a_row_returns_the_tool_and_next_policy() {
        let mut v = PolicyView::default();
        v.set_entries(vec![
            entry("read_file", 0, ToolPolicyWire::Default),
            entry("system_power", 3, ToolPolicyWire::Deny),
        ]);
        assert_eq!(
            v.cycle(0),
            Some(("read_file".to_string(), ToolPolicyWire::Ask))
        );
        assert_eq!(
            v.cycle(1),
            Some(("system_power".to_string(), ToolPolicyWire::Default))
        );
        // 越界
        assert_eq!(v.cycle(2), None);
    }

    /// 等回执期间**不接受点击**：连点会发多个请求，回执顺序无法保证，
    /// 最后落盘的可能不是用户最后想要的那个。
    #[test]
    fn clicks_are_ignored_while_a_save_is_pending() {
        let mut v = PolicyView::default();
        v.set_entries(vec![entry("read_file", 0, ToolPolicyWire::Default)]);
        v.mark_pending("read_file");
        assert_eq!(v.cycle(0), None, "等回执时不该再发请求");
        // 回执落地后恢复
        v.apply_confirmed("read_file", ToolPolicyWire::Ask);
        assert!(v.pending.is_none());
        assert_eq!(v.cycle(0), Some(("read_file".to_string(), ToolPolicyWire::Allow)));
        assert_eq!(v.entries()[0].policy, ToolPolicyWire::Ask, "确认后的值要落到列表上");
    }

    /// **写失败时保留旧列表**：清掉的话用户会以为"工具都没了"。
    #[test]
    fn a_failure_keeps_the_existing_list() {
        let mut v = PolicyView::default();
        v.set_entries(vec![entry("read_file", 0, ToolPolicyWire::Default)]);
        v.set_error("写盘失败".into());
        assert_eq!(v.entries().len(), 1, "失败不该清空列表");
        assert!(v.is_loaded());
        assert_eq!(v.error(), Some("写盘失败"));
        assert!(v.pending.is_none(), "失败后要解除 pending，否则界面会一直卡在保存中");
    }

    /// 回执只更新**对应的那一行**（别的行不能被顺手改掉）。
    #[test]
    fn confirmation_updates_only_the_matching_row() {
        let mut v = PolicyView::default();
        v.set_entries(vec![
            entry("a", 0, ToolPolicyWire::Default),
            entry("b", 0, ToolPolicyWire::Default),
        ]);
        v.apply_confirmed("b", ToolPolicyWire::Deny);
        assert_eq!(v.entries()[0].policy, ToolPolicyWire::Default);
        assert_eq!(v.entries()[1].policy, ToolPolicyWire::Deny);
    }

    /// 不存在的工具名落地时**不改任何东西**（也不能 panic）。
    #[test]
    fn confirmation_for_an_unknown_tool_is_a_noop() {
        let mut v = PolicyView::default();
        v.set_entries(vec![entry("a", 0, ToolPolicyWire::Default)]);
        v.apply_confirmed("nope", ToolPolicyWire::Deny);
        assert_eq!(v.entries()[0].policy, ToolPolicyWire::Default);
    }

    #[test]
    fn labels_cover_every_state_and_level() {
        use ToolPolicyWire as W;
        for (p, zh, en) in [
            (W::Default, "按等级", "By level"),
            (W::Ask, "每次询问", "Always ask"),
            (W::Allow, "允许", "Allow"),
            (W::Deny, "禁止", "Deny"),
        ] {
            assert_eq!(policy_label(p), zh);
            assert_eq!(policy_label_en(p), en);
            assert_ne!(policy_label(p), policy_label_en(p), "两种语言不该一样");
        }
        for lvl in 0..=3u8 {
            assert!(!level_label(lvl).is_empty());
            assert!(!level_label_en(lvl).is_empty());
        }
        assert_eq!(level_label(3), "L3 危险");
        assert_eq!(level_label(9), "L3 危险", "越界按最高等级处理（保守）");
    }

    /// 副标题要说明"点一下会变成什么"（否则用户不敢点）。
    #[test]
    fn row_hint_says_what_clicking_will_do() {
        use crate::i18n::Lang;
        let mut v = PolicyView::default();
        v.set_entries(vec![entry("read_file", 0, ToolPolicyWire::Default)]);
        let e = v.entries()[0].clone();
        let zh = v.row_hint(&e, Lang::Zh);
        assert!(zh.contains("L0"), "要带等级：{zh}");
        assert!(zh.contains("每次询问"), "要说明点一下会变成什么：{zh}");
        let en = v.row_hint(&e, Lang::En);
        assert!(en.contains("L0") && en.contains("Always ask"), "英文副标题：{en}");
        // 保存中要如实显示，而不是继续显示"点击改为…"
        v.mark_pending("read_file");
        let pending_hint = v.row_hint(&e, Lang::Zh);
        assert!(pending_hint.contains("保存中"), "等回执时要显示保存中：{pending_hint}");
    }
}
