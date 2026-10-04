//! 「快照与回滚」页的视图状态（FEATURE-GAPS §5.6）。
//!
//! ## 两步确认是这里的核心逻辑
//!
//! 回滚改的是**系统配置**（`/etc/localtime`、`resolv.conf`、策略等），
//! 一次误点就会把用户当前能用的设置换掉。所以：
//! **第一次点击只是选中（提示"再点一次确认"）**，第二次才真的发请求。
//!
//! 这条规则放在状态机里（而不是散在事件循环里），因为它有边界要守：
//! 选中的必须是**清单里真实存在的**那个快照、等回执期间不许再点、
//! 列表刷新后确认对象要清掉（否则会出现"确认的是上个版本里的快照"）。
//!
//! ## 名字里的"配置"
//!
//! 这是**配置快照**，不是盘镜像 —— 界面文案必须这么说，
//! 否则用户会以为"回滚能救回我删掉的照片"。

// 绘制侧（设置页的列表与提示）在 Windows 上也会用到本模块；
// 但「点击 → 两步确认 → 发请求」这条链在 Linux 的 fbdev 事件循环里，
// 因此非目标平台上这些方法只有测试在用 —— 与 `palette`/`policyview`/`spaces` 同一处理。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use aether_ipc::SnapshotWire;

/// 快照页的状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapView {
    items: Vec<SnapshotWire>,
    loaded: bool,
    error: Option<String>,
    /// 有请求在飞（创建或回滚）。期间不接受新的点击。
    pending: bool,
    /// 已选中、等待第二次点击确认回滚的快照 id。
    confirming: Option<String>,
}

impl SnapView {
    pub fn items(&self) -> &[SnapshotWire] {
        &self.items
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn confirming(&self) -> Option<&str> {
        self.confirming.as_deref()
    }

    pub fn mark_pending(&mut self) {
        self.pending = true;
        self.error = None;
    }

    /// 收到清单。**会清掉确认态**：刷新后的清单与确认时的可能不是同一份，
    /// 留着确认态就会出现"确认的是上一次列表里的快照"。
    pub fn on_list(&mut self, items: Vec<SnapshotWire>) {
        self.items = items;
        self.loaded = true;
        self.error = None;
        self.pending = false;
        self.confirming = None;
    }

    pub fn on_failure(&mut self, why: String) {
        self.pending = false;
        self.error = Some(why);
    }

    /// 第一次点击：选中某条（或从已选中的切换过去）。
    ///
    /// 点**不存在的** id 不改变状态（越界点击不该把界面带进"要确认一个不存在的东西"）。
    pub fn begin_confirm(&mut self, id: &str) -> bool {
        if self.pending || !self.items.iter().any(|s| s.id == id) {
            return false;
        }
        self.confirming = Some(id.to_string());
        true
    }

    pub fn cancel_confirm(&mut self) {
        self.confirming = None;
    }

    /// 第二次点击：取出要回滚的 id（并清确认态）。
    ///
    /// 只有**当前选中的那一条**能被确认 —— 点别的行只会改变选中对象，不会直接触发。
    pub fn take_confirmed(&mut self, id: &str) -> Option<String> {
        if self.pending {
            return None;
        }
        match self.confirming.as_deref() {
            Some(cur) if cur == id => {
                self.confirming = None;
                Some(id.to_string())
            }
            _ => None,
        }
    }

    /// 某一行的副标题（时间 + 文件数 + 当前状态）。
    pub fn row_hint(&self, i: usize, lang: crate::i18n::Lang) -> String {
        let Some(s) = self.items.get(i) else {
            return String::new();
        };
        let when = crate::timeview::relative_ago(crate::timeview::now_unix(), s.created_unix, lang);
        let files = format!(
            "{} {}/{}",
            crate::i18n::pick(lang, "文件", "files"),
            s.file_count,
            s.total
        );
        if self.confirming.as_deref() == Some(s.id.as_str()) {
            return format!(
                "{when} · {files} · {}",
                crate::i18n::pick(lang, "再点一次确认回滚", "click again to confirm rollback")
            );
        }
        format!("{when} · {files}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, created: i64, files: usize, total: usize) -> SnapshotWire {
        SnapshotWire { id: id.to_string(), created_unix: created, file_count: files, total }
    }

    #[test]
    fn starts_unloaded_and_distinguishes_empty_from_failed() {
        let v = SnapView::default();
        assert!(!v.is_loaded() && v.is_empty() && v.error().is_none());
        let mut v2 = SnapView::default();
        v2.on_failure("连接失败".into());
        assert!(!v2.is_loaded(), "失败不该变成已加载");
        assert_eq!(v2.error(), Some("连接失败"));
    }

    /// **两步确认**：第一次点击只选中，第二次才回滚。
    ///
    /// 这是本页最重要的一条：回滚改的是系统配置，误点一次就换掉了用户当前能用的设置。
    #[test]
    fn rollback_needs_two_clicks() {
        let mut v = SnapView::default();
        v.on_list(vec![item("20261004T112233", 1_791_112_953, 3, 6)]);
        // 第一次点击：只是选中
        assert!(v.begin_confirm("20261004T112233"));
        assert_eq!(v.confirming(), Some("20261004T112233"));
        // 点别处不会触发（这里没有别的行，点不存在的 id 也不该触发）
        assert_eq!(v.take_confirmed("nope"), None, "没选中的不能被确认");
        // 第二次点击同一条：才真的回滚
        assert_eq!(
            v.take_confirmed("20261004T112233").as_deref(),
            Some("20261004T112233")
        );
        assert!(v.confirming().is_none(), "确认后要清掉确认态（否则第三次点击又回滚一次）");
        // 再点一次不会重复触发
        assert_eq!(v.take_confirmed("20261004T112233"), None);
    }

    #[test]
    fn confirming_switches_target_and_can_be_cancelled() {
        let mut v = SnapView::default();
        v.on_list(vec![
            item("a1", 1_791_112_953, 1, 6),
            item("b2", 1_791_112_954, 2, 6),
        ]);
        v.begin_confirm("a1");
        v.begin_confirm("b2");
        assert_eq!(v.confirming(), Some("b2"), "选中的应当是最后点的那个");
        assert_eq!(v.take_confirmed("a1"), None, "选中已切到 b2，a1 不该被确认");
        v.cancel_confirm();
        assert!(v.confirming().is_none());
        assert_eq!(v.take_confirmed("b2"), None, "取消之后不该还能确认");
    }

    /// 点不存在的 id 不改变状态（越界点击不该把界面带进"要确认一个不存在的东西"）。
    #[test]
    fn confirming_a_missing_snapshot_is_refused() {
        let mut v = SnapView::default();
        v.on_list(vec![item("a1", 1_791_112_953, 1, 6)]);
        assert!(!v.begin_confirm("不存在"));
        assert!(v.confirming().is_none());
    }

    /// **刷新会清掉确认态**：清单换了一份之后，确认的必须重新点过。
    #[test]
    fn refreshing_the_list_clears_the_pending_confirmation() {
        let mut v = SnapView::default();
        v.on_list(vec![item("a1", 1_791_112_953, 1, 6)]);
        v.begin_confirm("a1");
        v.on_list(vec![item("a1", 1_791_112_953, 1, 6), item("b2", 1_791_112_955, 2, 6)]);
        assert!(v.confirming().is_none(), "刷新后确认态必须清掉");
        assert_eq!(v.take_confirmed("a1"), None, "刷新后需要重新点两次");
    }

    /// 有请求在飞时**不接受点击**（连点会发多个请求，回滚顺序无法保证）。
    #[test]
    fn clicks_are_ignored_while_a_request_is_in_flight() {
        let mut v = SnapView::default();
        v.on_list(vec![item("a1", 1_791_112_953, 1, 6)]);
        v.begin_confirm("a1");
        v.mark_pending();
        assert!(!v.begin_confirm("a1"), "等回执期间不该改变选中");
        assert_eq!(v.take_confirmed("a1"), None, "等回执期间不该触发回滚");
        v.on_failure("写盘失败".into());
        assert!(!v.is_pending(), "失败后要解除 pending，否则界面永远卡着");
        // 失败后可以重试
        assert!(v.begin_confirm("a1"));
    }

    #[test]
    fn row_hint_shows_time_files_and_confirmation_state() {
        use crate::i18n::Lang;
        let mut v = SnapView::default();
        let now = crate::timeview::now_unix();
        v.on_list(vec![item("a1", now - 120, 3, 6)]);
        let zh = v.row_hint(0, Lang::Zh);
        assert!(zh.contains("分钟前"), "要显示相对时间：{zh}");
        assert!(zh.contains("3/6"), "要显示文件数：{zh}");
        assert!(!zh.contains("再点一次"), "还没选中时不该提示确认");
        v.begin_confirm("a1");
        let zh2 = v.row_hint(0, Lang::Zh);
        assert!(zh2.contains("再点一次确认回滚"), "选中后要说清下一步：{zh2}");
        let en = v.row_hint(0, Lang::En);
        assert!(en.contains("click again"), "英文也要有：{en}");
        assert!(v.row_hint(9, Lang::Zh).is_empty(), "越界给空串");
    }
}
