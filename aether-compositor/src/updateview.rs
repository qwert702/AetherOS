//! 「系统更新」页的视图状态（FEATURE-GAPS §3.1 的界面入口）。
//!
//! ## 界面不判断"能不能装"
//!
//! 界面只拿到**包名**（清单里的版本号在包内），而"必须严格新于当前版本"
//! 这条规则在后端。所以界面**不猜**：猜错会让用户看到"能装"然后失败，
//! 比直接由后端给结论更糟。界面负责把后端的结论（含失败原因）如实显示出来。
//!
//! ## 为什么"暂存中"要挡住重复点击
//!
//! 暂存会读整个包、算哈希、拷文件（可能几百 MB）。连点会并发跑几份 ——
//! 而 `stage()` 里有"先清掉上一次暂存"的步骤，并发会让两份互相清掉。

// 绘制侧（设置页的列表与状态行）在 Windows 上也会用到本模块；
// 但「点击 → 发请求」这条链在 Linux 的 fbdev 事件循环里，
// 因此非目标平台上部分方法只有测试在用 —— 与 `snapview` 同一处理。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// 「系统更新」页的状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateView {
    packages: Vec<String>,
    current_version: String,
    staged_version: Option<String>,
    loaded: bool,
    /// 正在暂存的包名（挡住重复点击）
    pending: Option<String>,
    /// **当前是否处于错误状态**（连不上 aetherd、后端拒绝之类）。
    ///
    /// 一次成功的拉取会清掉它 —— 它是"现在有没有问题"，不是"历史上哪次失败过"。
    error: Option<String>,
    /// **上一次操作的结果**（成功与失败都留，含后端给的原因）。
    ///
    /// 刷新列表**不会**清掉它：用户刚点了一次"暂存"，刷新把这个结果抹掉，
    /// 会让人以为那次操作没发生。
    message: Option<String>,
}

impl UpdateView {
    pub fn packages(&self) -> &[String] {
        &self.packages
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// 收到列表：**清 `error`（当前没问题了）但保留 `message`（上一次操作结果）**。
    pub fn on_list(&mut self, packages: Vec<String>, current: String, staged: Option<String>) {
        self.packages = packages;
        self.current_version = current;
        self.staged_version = staged;
        self.loaded = true;
        self.error = None;
        self.pending = None;
    }

    pub fn mark_pending(&mut self, name: &str) {
        self.pending = Some(name.to_string());
        self.error = None;
        self.message = None;
    }

    /// 暂存回执（成功或失败都由后端给结论）。
    pub fn on_result(&mut self, ok: bool, message: String, version: Option<String>) {
        self.pending = None;
        if ok {
            self.error = None;
            // 服务端已把新版本落盘，这里同步显示（不必等下一次拉取）
            if let Some(v) = version {
                self.staged_version = Some(v);
            }
        } else {
            self.error = Some(message.clone());
        }
        self.message = Some(message);
    }

    /// 某个包现在能不能点（正在暂存时一律不能，避免并发互清）。
    pub fn can_click(&self) -> bool {
        self.pending.is_none()
    }

    /// 某一行的副标题。
    pub fn row_hint(&self, i: usize, lang: crate::i18n::Lang) -> String {
        let Some(name) = self.packages.get(i) else {
            return String::new();
        };
        if self.pending.as_deref() == Some(name.as_str()) {
            return crate::i18n::pick(lang, "正在校验并暂存…", "verifying & staging…").to_string();
        }
        crate::i18n::pick(
            lang,
            "点一下暂存；版本与哈希由 aetherd 校验，重启后生效",
            "click to stage; verified by aetherd, applies on reboot",
        )
        .to_string()
    }

    /// 顶部状态行（当前版本 / 待应用版本）。
    pub fn status_line(&self, lang: crate::i18n::Lang) -> String {
        let cur = if self.current_version.is_empty() {
            crate::i18n::pick(lang, "未知", "unknown").to_string()
        } else {
            self.current_version.clone()
        };
        match self.staged_version.as_deref() {
            Some(v) => format!(
                "{} {cur} · {} {v}（{}）",
                crate::i18n::pick(lang, "当前版本", "current"),
                crate::i18n::pick(lang, "待应用", "staged"),
                crate::i18n::pick(lang, "重启后生效", "applies on reboot"),
            ),
            None => format!(
                "{} {cur} · {}",
                crate::i18n::pick(lang, "当前版本", "current"),
                crate::i18n::pick(lang, "没有待应用的更新", "nothing staged"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn starts_unloaded_and_distinguishes_empty_from_failed() {
        let v = UpdateView::default();
        assert!(!v.is_loaded() && v.packages().is_empty() && v.error().is_none());
        // 失败走的是"回执"这条路（异步函数把连接失败也映射成 ok=false 的回执）
        let mut v2 = UpdateView::default();
        v2.on_result(false, "连接 aetherd 失败".into(), None);
        assert!(!v2.is_loaded(), "失败不该变成已加载");
        assert_eq!(v2.error(), Some("连接 aetherd 失败"));
    }

    /// **暂存中挡住重复点击**：连点会并发跑几份 `stage()`，
    /// 而它里面有"先清掉上一次暂存"的步骤 —— 两份会互相清掉。
    #[test]
    fn staging_blocks_further_clicks_until_the_reply_arrives() {
        let mut v = UpdateView::default();
        v.on_list(vec!["pkg-1".into()], "1.0.0".into(), None);
        assert!(v.can_click());
        v.mark_pending("pkg-1");
        assert!(!v.can_click(), "暂存期间不能再点");
        assert!(v.row_hint(0, Lang::Zh).contains("正在校验"));
        // 回执到了才恢复
        v.on_result(true, "已暂存".into(), Some("1.0.1".into()));
        assert!(v.can_click(), "回执到了就恢复可点");
    }

    /// 失败时**必须留下后端给的原因**（而不是只标一个"失败"）。
    #[test]
    fn failures_keep_the_backend_reason() {
        let mut v = UpdateView::default();
        v.on_list(vec!["pkg".into()], "1.0.0".into(), None);
        v.mark_pending("pkg");
        v.on_result(false, "拒绝降级/同版本：包内版本 1.0.0，当前 1.0.0（要回滚请用快照回滚）".into(), None);
        assert!(v.can_click(), "失败后要恢复可点");
        assert!(
            v.error().unwrap().contains("拒绝降级"),
            "后端原因要原样留着：{:?}",
            v.error()
        );
        assert!(v.message().is_some(), "结果消息也要显示");
        // 失败不能把"待应用版本"写成成功 —— 从**用户可见的状态行**上断言
        assert!(
            v.status_line(Lang::Zh).contains("没有待应用的更新"),
            "失败后状态行不该显示待应用：{}",
            v.status_line(Lang::Zh)
        );
    }

    /// 成功后立刻显示"待应用版本"（不必等下一次拉取）。
    #[test]
    fn a_successful_stage_shows_the_new_staged_version() {
        let mut v = UpdateView::default();
        v.on_list(vec!["pkg".into()], "1.0.0".into(), None);
        v.mark_pending("pkg");
        v.on_result(true, "已暂存更新 1.0.1（2 个文件）· 重启后生效".into(), Some("1.0.1".into()));
        assert!(v.status_line(Lang::Zh).contains("1.0.1"));
        assert!(v.status_line(Lang::Zh).contains("重启后生效"));
    }

    /// **刷新列表不会抹掉刚发生的操作结果**（否则用户以为操作没发生）。
    #[test]
    fn refreshing_the_list_keeps_the_last_result_message() {
        let mut v = UpdateView::default();
        v.on_list(vec!["pkg".into()], "1.0.0".into(), None);
        v.on_result(false, "哈希不符：整包拒绝".into(), None);
        v.on_list(vec!["pkg".into()], "1.0.0".into(), None);
        assert_eq!(
            v.message(),
            Some("哈希不符：整包拒绝"),
            "刷新不该抹掉刚才的结果（否则用户以为操作没发生）"
        );
        // `error` 是"当前是否处于错误状态"：一次成功的拉取说明现在没问题了，就该清掉。
        // 失败**原因**由 `message` 承载 —— 这正是两者分开的理由。
        assert!(v.error().is_none(), "成功的刷新应当清掉当前错误状态");
    }

    #[test]
    fn status_line_covers_both_languages_and_both_states() {
        let mut v = UpdateView::default();
        assert!(v.status_line(Lang::Zh).contains("未知"), "没有版本时要说未知");
        v.on_list(vec![], "1.0.0".into(), None);
        assert!(v.status_line(Lang::Zh).contains("没有待应用的更新"));
        assert!(v.status_line(Lang::En).contains("nothing staged"));
        v.on_list(vec![], "1.0.0".into(), Some("1.1.0".into()));
        assert!(v.status_line(Lang::En).contains("staged 1.1.0"));
        assert!(v.status_line(Lang::Zh).contains("待应用 1.1.0"));
    }

    #[test]
    fn row_hint_is_bilingual_and_safe_on_out_of_range() {
        let mut v = UpdateView::default();
        v.on_list(vec!["a".into()], "1.0.0".into(), None);
        assert!(v.row_hint(0, Lang::Zh).contains("暂存"));
        assert!(v.row_hint(0, Lang::En).contains("stage"));
        assert!(v.row_hint(9, Lang::Zh).is_empty(), "越界给空串，不 panic");
    }
}
