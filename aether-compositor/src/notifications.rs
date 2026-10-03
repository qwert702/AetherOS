//! 通知历史（§3.2）：把 toast 从"错过就没了"变成可回看的历史。
//!
//! ## 为什么单独一个模块
//!
//! toast 在 `main.rs` 里有 **47 个产生点**，直接往那些地方塞历史逻辑会散得到处都是。
//! 这里只放**纯逻辑**（容量、合并、未读计数），接线是另一件事 —— 于是"历史上限是多少、
//! 连续重复怎么合并、未读怎么算"这些能被单测钉住，不必靠点着看。
//!
//! ## 时间为什么是参数而不是 `SystemTime::now()`
//!
//! 与 `Settings::should_blank(Duration)` 同一手法：把"现在几点"当**参数**传进来，
//! 于是"合并窗口内/窗口外"这类边界可以在单测里当场验，而不是靠等三秒。

// 接线点在合成器主循环（Linux 专属），因此非 Linux 目标上本模块没有调用点。
// 与 `audio.rs` 同一处理：允许 dead_code，但**测试照跑**（纯逻辑正该在开发机上验）。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::VecDeque;

/// 历史上限。50 条足够回看"刚才提示了什么"，又不会让内存随运行时长无界增长 ——
/// **有界**是硬要求：一个跑几个月的桌面不该因为通知越攒越多而变慢。
pub const MAX_NOTICES: usize = 50;

/// 连续重复的合并窗口（秒）。同一文本在这个窗口内再次出现 → 合并成一条并累加次数。
///
/// 为什么需要它：有些提示天然会连发（连点、重试、网络抖动），
/// 不合并的话历史会被同一条消息刷满，等于把"历史"这个功能废掉。
pub const MERGE_WINDOW_SECS: u64 = 3;

/// 通知类别。**只分三类**：多一类就要多一套配色与文案，收益不成正比。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 一般信息（提示、状态变化）
    Info,
    /// 成功
    Ok,
    /// 失败/被拒绝 —— 需要用户注意，所以界面上必须与成功可区分
    Err,
}

impl Kind {
    /// 给界面用的一字标记（与 AI 气泡的 ✓/✗ 同一套视觉语言，不另造一套）。
    pub fn mark(&self) -> &'static str {
        match self {
            Kind::Info => "·",
            Kind::Ok => "✓",
            Kind::Err => "✗",
        }
    }
}

/// 一条通知。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Notice {
    /// 文本（已去掉换行：历史是一行一条）
    pub text: String,
    pub kind: Kind,
    /// 发生时间（Unix 秒；由调用方给，便于单测）
    pub at: u64,
    /// 合并次数：1 = 只发生过一次
    pub repeat: u32,
}

/// 通知历史（有界、可合并、带未读计数）。
#[derive(Debug)]
pub struct Notifications {
    /// **新的在前**（索引 0 是最新）：界面按时间倒序展示，这样渲染不用反转
    items: VecDeque<Notice>,
    unread: usize,
}

impl Default for Notifications {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifications {
    pub fn new() -> Self {
        Self { items: VecDeque::with_capacity(MAX_NOTICES), unread: 0 }
    }

    /// 记录一条通知。
    ///
    /// 三条规则（都有单测）：
    /// 1. **同一文本 + 同一类别**且距上一条不超过 `MERGE_WINDOW_SECS` → 合并（次数 +1、
    ///    时间刷新），**不**额外增加未读（用户还没看到，一条未读就够了）。
    /// 2. 超出上限 → 淘汰最旧的一条。
    /// 3. 文本先做净化：多行压成一行、去掉首尾空白 —— 历史是一行一条，
    ///    带换行的文本会把布局撑坏。
    pub fn push(&mut self, text: &str, kind: Kind, now_secs: u64) {
        let clean = one_line(text);
        if clean.is_empty() {
            return; // 空消息不该占一条历史
        }
        if let Some(last) = self.items.front_mut() {
            if last.text == clean
                && last.kind == kind
                && now_secs.saturating_sub(last.at) <= MERGE_WINDOW_SECS
            {
                last.repeat = last.repeat.saturating_add(1);
                last.at = now_secs;
                return;
            }
        }
        self.items.push_front(Notice { text: clean, kind, at: now_secs, repeat: 1 });
        self.unread = self.unread.saturating_add(1);
        while self.items.len() > MAX_NOTICES {
            self.items.pop_back();
        }
    }

    /// 按时间倒序遍历（新的在前）。
    pub fn iter(&self) -> impl Iterator<Item = &Notice> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 未读条数（打开通知中心即清零）。
    pub fn unread(&self) -> usize {
        self.unread
    }

    /// 全部标为已读。
    pub fn mark_all_read(&mut self) {
        self.unread = 0;
    }

    /// 清空历史（用户主动"清空"，与"标为已读"是两件事）。
    pub fn clear(&mut self) {
        self.items.clear();
        self.unread = 0;
    }
}

/// 压成一行：多行合并、去掉控制字符与首尾空白。
///
/// 控制字符必须去掉：通知文本可能来自外部（IPC 返回的错误信息），
/// 原样显示进界面就是注入（换行撑坏布局、ESC 序列改终端状态）。
fn one_line(text: &str) -> String {
    let joined: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out = String::with_capacity(joined.len());
    let mut prev_space = false;
    for c in joined.chars() {
        if c == ' ' {
            if !prev_space {
                out.push(c);
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_drops_oldest() {
        let mut n = Notifications::new();
        for i in 0..(MAX_NOTICES + 10) {
            n.push(&format!("第 {i} 条"), Kind::Info, i as u64 * 100);
        }
        assert_eq!(n.len(), MAX_NOTICES, "上限必须生效（内存有界）");
        // 最新的在最前，最旧的已被淘汰
        assert_eq!(n.iter().next().unwrap().text, format!("第 {} 条", MAX_NOTICES + 9));
        let oldest = n.iter().last().unwrap().text.clone();
        assert_eq!(oldest, format!("第 {} 条", 10), "最旧的 10 条应被淘汰");
    }

    #[test]
    fn consecutive_duplicates_merge_within_window() {
        let mut n = Notifications::new();
        n.push("网络不可达", Kind::Err, 100);
        n.push("网络不可达", Kind::Err, 101); // 窗口内 → 合并
        n.push("网络不可达", Kind::Err, 103); // 边界（恰好 3 秒）→ 仍合并
        assert_eq!(n.len(), 1, "窗口内的重复应合并成一条");
        let only = n.iter().next().unwrap();
        assert_eq!(only.repeat, 3, "合并次数应累加");
        assert_eq!(only.at, 103, "时间应刷新为最后一次");
        // 未读**不**因合并而累加：用户还没看到，一条未读就够了
        assert_eq!(n.unread(), 1, "合并不应把未读刷成 3");
    }

    #[test]
    fn duplicates_outside_window_are_separate() {
        let mut n = Notifications::new();
        n.push("网络不可达", Kind::Err, 100);
        n.push("网络不可达", Kind::Err, 104); // 超过 3 秒 → 不合并
        assert_eq!(n.len(), 2);
        assert_eq!(n.unread(), 2);
    }

    #[test]
    fn different_kind_or_text_never_merges() {
        let mut n = Notifications::new();
        n.push("同一句话", Kind::Ok, 100);
        n.push("同一句话", Kind::Err, 100); // 类别不同
        n.push("另一句话", Kind::Err, 100); // 文本不同
        assert_eq!(n.len(), 3);
    }

    #[test]
    fn empty_or_whitespace_messages_are_ignored() {
        let mut n = Notifications::new();
        n.push("", Kind::Info, 0);
        n.push("   ", Kind::Info, 0);
        n.push("\n\t ", Kind::Info, 0);
        assert!(n.is_empty(), "空消息不该占历史");
        assert_eq!(n.unread(), 0);
    }

    #[test]
    fn multiline_and_control_chars_are_flattened() {
        let mut n = Notifications::new();
        // 外部来源的文本可能带换行/ESC（IPC 错误信息），必须压成一行
        n.push("第一行\n第二行\t带制表\x1b[31m与 ESC", Kind::Err, 0);
        let t = &n.iter().next().unwrap().text;
        assert!(!t.contains('\n'), "不能含换行：{t:?}");
        assert!(!t.contains('\x1b'), "不能含 ESC：{t:?}");
        assert!(!t.contains('\t'), "不能含制表：{t:?}");
        assert!(t.starts_with("第一行 第二行"), "应压成一行：{t:?}");
        // 连续空格压成一个，且不留首尾空白
        let mut m = Notifications::new();
        m.push("  多   空格  ", Kind::Info, 0);
        assert_eq!(m.iter().next().unwrap().text, "多 空格");
    }

    #[test]
    fn read_and_clear_are_distinct() {
        let mut n = Notifications::new();
        n.push("a", Kind::Info, 0);
        n.push("b", Kind::Info, 1);
        assert_eq!(n.unread(), 2);
        n.mark_all_read();
        assert_eq!(n.unread(), 0);
        assert_eq!(n.len(), 2, "标为已读不该删掉历史");
        n.clear();
        assert!(n.is_empty());
        assert_eq!(n.unread(), 0, "清空后未读必须归零");
    }

    /// 三类标记必须互不相同 —— 否则界面上"成功"和"失败"看起来一样。
    #[test]
    fn kind_marks_are_distinct() {
        let m = [Kind::Info.mark(), Kind::Ok.mark(), Kind::Err.mark()];
        assert_ne!(m[0], m[1]);
        assert_ne!(m[1], m[2]);
        assert_ne!(m[0], m[2]);
    }
}
