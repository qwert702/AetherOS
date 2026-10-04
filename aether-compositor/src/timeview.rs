//! 「时钟与时区」页里对时与时区那部分的状态（FEATURE-GAPS §3.3）。
//!
//! 与相邻模块的分工：`aetherd::timesync` 做对时与写系统时区，本模块只管界面这一侧 ——
//! 上次对时结果怎么显示、时区名候选怎么切换。所以这里全是纯逻辑，能脱离 IPC 测全。
//!
//! ## 为什么时区名要有"候选列表"而不是让用户手打
//!
//! 时区名（`Asia/Shanghai`）是 IANA 名字，用户不该背。候选列表覆盖常见几档，
//! 点一下就换；**手打留给"候选里没有"的情况**（那是后续的输入框，不是这批）。
//! 列表不追求全 —— 追求"常见的不必查"（全量 IANA 有几百条，塞进设置页只会更难找）。
//!
//! ## 为什么"当前系统时区"要单独显示
//!
//! 下面那个候选是**待写入的名字**，而系统当前用的是另一个（可能不同）：
//! 两者混在一起，用户会以为"我已经改好了"（其实还没点写入）。

// 绘制侧（时钟页的显示行）在 Windows 上也会用到本模块；
// 但「对时 / 换候选 / 写系统时区」这条链在 Linux 的 fbdev 事件循环里，
// 因此非目标平台上这些方法只有测试在用 —— 与 palette/spaces/setup 同一处理。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// 当前 Unix 秒（拿不到就 0）。
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "多久以前"的**共用**文案（时间页与快照页都用它）。
///
/// 抽出来是因为：两个页面各写一份，必然在某个边界上分叉
/// （一个说"刚刚"、另一个说"0 分钟前"），而用户看到两套说法会以为是两件事。
pub fn relative_ago(now: i64, then: i64, lang: crate::i18n::Lang) -> String {
    let ago = (now - then).max(0);
    if ago < 60 {
        crate::i18n::pick(lang, "刚刚", "just now").to_string()
    } else if ago < 3600 {
        format!(
            "{} {} {}",
            crate::i18n::pick(lang, "约", "about"),
            ago / 60,
            crate::i18n::pick(lang, "分钟前", "min ago")
        )
    } else if ago < 86_400 {
        format!(
            "{} {} {}",
            crate::i18n::pick(lang, "约", "about"),
            ago / 3600,
            crate::i18n::pick(lang, "小时前", "h ago")
        )
    } else {
        format!(
            "{} {} {}",
            crate::i18n::pick(lang, "约", "about"),
            ago / 86_400,
            crate::i18n::pick(lang, "天前", "d ago")
        )
    }
}

/// 常见时区候选（够用就好：全量 IANA 有几百条，塞进设置页只会更难找）。
pub const TZ_CANDIDATES: &[&str] = &[
    "UTC",
    "Asia/Shanghai",
    "Asia/Tokyo",
    "Asia/Singapore",
    "Europe/London",
    "Europe/Paris",
    "America/New_York",
    "America/Los_Angeles",
];

/// 时钟页的对时/时区状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeView {
    /// 系统当前时区（读 `/etc/timezone`，读不到就是空）。
    pub system_tz: String,
    /// 上次对时偏移（毫秒）；`None` = **从未对过**（不与"偏移 0"混淆）。
    pub last_offset_ms: Option<i64>,
    /// 上次对时的 Unix 秒（0 = 从未对过）。
    pub last_sync_unix: i64,
    /// 待写入的候选下标。
    pub picked: usize,
    /// 是否有请求在飞（避免连点）。
    pub pending: bool,
    /// 最近一次失败原因。
    pub error: Option<String>,
    /// 是否已经从 aetherd 取到过状态。
    pub loaded: bool,
}

impl TimeView {
    /// 收到状态（`/etc/timezone` 的名字 + 上次对时结果）。
    pub fn on_status(&mut self, system_tz: String, last_offset_ms: i64, last_sync_unix: i64) {
        // `i64::MIN` 是"从未对过"的约定（见 `aetherd::server` 的说明），不是偏移
        self.last_offset_ms = if last_offset_ms == i64::MIN {
            None
        } else {
            Some(last_offset_ms)
        };
        self.last_sync_unix = last_sync_unix;
        // 候选下标跟到系统当前时区上（若它在候选里）：这样"写入"前看到的名字
        // 就是系统现在用的那个，不会一进来就显示一个不相干的候选
        if let Some(i) = TZ_CANDIDATES.iter().position(|c| *c == system_tz) {
            self.picked = i;
        }
        self.system_tz = system_tz;
        self.loaded = true;
        self.pending = false;
        self.error = None;
    }

    pub fn mark_pending(&mut self) {
        self.pending = true;
        self.error = None;
    }

    pub fn on_failure(&mut self, why: String) {
        self.pending = false;
        self.error = Some(why);
    }

    /// 切换候选（循环）。
    pub fn cycle(&mut self) {
        if TZ_CANDIDATES.is_empty() {
            return;
        }
        self.picked = (self.picked + 1) % TZ_CANDIDATES.len();
    }

    pub fn picked_name(&self) -> &'static str {
        TZ_CANDIDATES.get(self.picked).copied().unwrap_or("UTC")
    }

    /// 待写入的名字与系统当前是否**已经一致**（一致时"写入"其实不必点）。
    pub fn already_system_timezone(&self) -> bool {
        self.picked_name() == self.system_tz
    }

    /// 对时状态那一行（中文/英文）。
    pub fn sync_line(&self, lang: crate::i18n::Lang) -> String {
        match self.last_offset_ms {
            None => crate::i18n::pick(lang, "尚未对时", "Never synced").to_string(),
            Some(off) => {
                let when = if self.last_sync_unix == 0 {
                    String::new()
                } else {
                    // 只显示相对时间：绝对时间在不同时区/不同步的机器上都会误导人
                    format!(" · {}", relative_ago(now_unix(), self.last_sync_unix, lang))
                };
                if off == 0 {
                    format!(
                        "{}{}",
                        crate::i18n::pick(lang, "上次对时：完全准", "Last sync: exact"),
                        when
                    )
                } else {
                    format!(
                        "{}{}",
                        format_args!(
                            "{} {off:+} ms",
                            crate::i18n::pick(lang, "上次对时偏移", "Last offset")
                        ),
                        when
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn defaults_are_never_synced_and_not_loaded() {
        let v = TimeView::default();
        assert!(v.last_offset_ms.is_none(), "默认必须是「从未对过」");
        assert!(!v.loaded);
        assert_eq!(v.picked_name(), "UTC");
        assert!(v.sync_line(Lang::Zh).contains("尚未对时"));
        assert!(v.sync_line(Lang::En).contains("Never synced"));
    }

    /// `i64::MIN` 是"从未对过"的线上约定，**不能**被当成一个偏移显示出来。
    #[test]
    fn a_never_synced_status_does_not_look_like_an_offset() {
        let mut v = TimeView::default();
        v.on_status("UTC".into(), i64::MIN, 0);
        assert!(v.last_offset_ms.is_none(), "i64::MIN 要翻成「从未对过」");
        assert!(v.sync_line(Lang::Zh).contains("尚未对时"), "不该显示一个天文数字的偏移");
        // 而偏移 0 是一个**有意义**的结果（完全准），必须区分开
        v.on_status("UTC".into(), 0, 1_791_619_200);
        assert_eq!(v.last_offset_ms, Some(0));
        assert!(v.sync_line(Lang::Zh).contains("完全准"), "偏移 0 要说「完全准」");
    }

    #[test]
    fn status_snaps_the_picked_candidate_to_the_system_timezone() {
        let mut v = TimeView::default();
        v.on_status("Asia/Tokyo".into(), 5, 0);
        assert_eq!(v.picked_name(), "Asia/Tokyo", "候选要跟到系统当前时区");
        assert!(v.already_system_timezone(), "跟上了就说明「不必再写」");
        // 系统时区不在候选里：picked 保持原样，但 system_tz 要如实记下来
        let mut v2 = TimeView::default();
        v2.on_status("Antarctica/Troll".into(), 0, 0);
        assert_eq!(v2.system_tz, "Antarctica/Troll");
        assert!(!v2.already_system_timezone(), "候选里没有它，写入是有意义的");
    }

    #[test]
    fn cycling_wraps_around_and_never_panics() {
        let mut v = TimeView::default();
        let first = v.picked_name();
        for _ in 0..TZ_CANDIDATES.len() {
            v.cycle();
        }
        assert_eq!(v.picked_name(), first, "转一圈要回到起点");
        for _ in 0..(TZ_CANDIDATES.len() * 3 + 1) {
            v.cycle();
            assert!(!v.picked_name().is_empty());
        }
    }

    #[test]
    fn pending_blocks_visual_duplication_and_failure_clears_it() {
        let mut v = TimeView::default();
        v.mark_pending();
        assert!(v.pending && v.error.is_none());
        v.on_failure("连接 aetherd 失败".into());
        assert!(!v.pending, "失败后必须解除 pending，否则界面永远卡着");
        assert_eq!(v.error.as_deref(), Some("连接 aetherd 失败"));
        // 成功回来时错误要清掉
        v.on_status("UTC".into(), 1, 100);
        assert!(v.error.is_none() && !v.pending && v.loaded);
    }

    /// 时区候选必须是**合法的 IANA 名**（含 `/`、非空、不以 `/` 开头）。
    /// 这里只做形状检查：真正能不能写由服务端校验（它才是权威）。
    #[test]
    fn candidates_look_like_iana_names() {
        assert!(!TZ_CANDIDATES.is_empty());
        for c in TZ_CANDIDATES {
            assert!(!c.is_empty() && !c.starts_with('/') && !c.contains(".."));
            assert!(c.len() <= 64);
        }
        // 至少要有 UTC 与一个中国时区（本项目的默认使用场景）
        assert!(TZ_CANDIDATES.contains(&"UTC"));
        assert!(TZ_CANDIDATES.contains(&"Asia/Shanghai"));
    }

    /// 相对时间要能读：刚同步过显示"刚刚"，久一点显示分钟/小时。
    #[test]
    fn the_sync_line_shows_a_relative_time_not_an_absolute_one() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let mut v = TimeView::default();
        v.on_status("UTC".into(), 12, now);
        assert!(v.sync_line(Lang::Zh).contains("刚刚"), "{}", v.sync_line(Lang::Zh));
        v.on_status("UTC".into(), 12, now - 120);
        let zh = v.sync_line(Lang::Zh);
        assert!(zh.contains("分钟前"), "两分钟前要说分钟：{zh}");
        v.on_status("UTC".into(), -3000, now - 7200);
        let zh2 = v.sync_line(Lang::Zh);
        assert!(zh2.contains("小时前") && zh2.contains("-3000"), "要带偏移：{zh2}");
    }
}
