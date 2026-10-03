//! 锁屏的**状态与判定**（FEATURE-GAPS §2.3）。
//!
//! 与相邻模块的分工：
//! * [`crate::sha512crypt`]：哈希算得对不对；
//! * [`crate::auth`]：该不该放行、失败怎么退避；
//! * **本模块**：锁屏自己的状态（是否锁着、输入了什么、给用户看什么话）。
//!
//! 绘制不在这里 —— 放在 `draw`（那里有全部绘制原语与字体缓存）。
//! 这样本模块保持**无副作用**，判定逻辑能在开发机上单测。

// 与 `auth`/`sha512crypt` 同一情形：`lock()`/`insert()`/`submit()` 的调用点
// 在 Linux 的 fbdev 事件循环里（`Super+L` 与锁屏吞键守卫）。非 Linux 上只有绘制侧
// （`is_locked`/`masked`/`message`）与测试用到其余部分。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use crate::auth::{self, Decision, Lockout, ShadowEntry};
use crate::textview::TextCursor;

/// 解锁时校验的账户名。
///
/// 当前系统**只有一个账户**（root）—— §2.3 的"多账户"部分没做，
/// 这里如实写死一个常量，而不是编造一个账户列表。将来引入账户模型时改这里。
pub const LOCK_USER: &str = "root";

/// 锁屏状态。
#[derive(Debug, Clone, Default)]
pub struct LockScreen {
    locked: bool,
    password: String,
    cursor: TextCursor,
    lockout: Lockout,
    /// 给用户看的话（拒绝理由、退避倒计时、未设口令说明…）。**不显示"正在校验"这种废话**。
    message: Option<String>,
    /// 累计尝试次数（仅用于显示与自我观察，判定不看它）。
    attempts: u32,
}

impl LockScreen {
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// 进入锁屏。
    ///
    /// **不清空退避状态**：连着试错之后把屏幕锁上，不该顺便把罚免掉
    /// （否则"锁一下再解锁"就是绕过退避的办法）。
    pub fn lock(&mut self) {
        self.locked = true;
        self.password.clear();
        self.cursor = TextCursor::default();
        self.message = None;
    }

    /// 直接解锁（例如"该账户未设口令"路径）。同时清掉输入与提示。
    pub fn unlock(&mut self) {
        self.locked = false;
        self.password.clear();
        self.cursor = TextCursor::default();
        self.message = None;
    }

    /// 输入口令的最大长度（防止有人按住键把内存撑起来）。
    pub const MAX_CHARS: usize = 256;

    pub fn insert(&mut self, c: char) {
        if self.password.chars().count() < Self::MAX_CHARS {
            self.cursor.insert(&mut self.password, c);
        }
        // 一开始输入就把上一条提示清掉 —— 它已经过期了
        self.message = None;
    }

    pub fn backspace(&mut self) {
        self.cursor.backspace(&mut self.password);
        self.message = None;
    }

    pub fn clear(&mut self) {
        self.password.clear();
        self.cursor = TextCursor::default();
        self.message = None;
    }

    pub fn is_empty(&self) -> bool {
        self.password.is_empty()
    }

    /// 显示用的掩码：一律用固定字符，**不泄露长度以外的东西**。
    ///
    /// 为什么连长度也照实显示：这是本机界面，旁观者本来就看得见你敲了几下；
    /// 而固定长度反而会让用户以为"我打的没进去"。
    pub fn masked(&self) -> String {
        "●".repeat(self.password.chars().count())
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// 用**给定的** shadow 项判定一次提交（纯逻辑，可在开发机上测全）。
    ///
    /// 返回值与 [`Decision`] 一致，便于调用方（事件循环）据此决定要不要重绘。
    pub fn submit_with(&mut self, entry: &ShadowEntry, now: u64) -> Decision {
        // 退避期内：**连次数都不增**（不去比对，也不给攻击者任何反馈差异）
        let wait = self.lockout.retry_after(now);
        if wait > 0 {
            self.message = Some(format!("尝试过于频繁，请等待 {wait} 秒"));
            self.password.clear();
            self.cursor = TextCursor::default();
            return Decision::Wait(wait);
        }
        self.attempts = self.attempts.saturating_add(1);
        let d = auth::decide(entry, &self.password, &mut self.lockout, now);
        match &d {
            Decision::Allow => {
                self.unlock();
            }
            Decision::AllowWithReason(why) => {
                // 放行，但**用户必须看到原因**（例如"本镜像未设口令"）——
                // 悄悄放行会让人以为"我输对了"，那是对安全的谎报。
                self.locked = false;
                self.password.clear();
                self.cursor = TextCursor::default();
                self.message = Some(why.clone());
            }
            Decision::Deny(why) => {
                // 口令字段**清空**（标准做法），提示留下
                self.password.clear();
                self.cursor = TextCursor::default();
                self.message = Some(why.clone());
            }
            Decision::Wait(_) => {}
        }
        d
    }

    /// 从真实 `/etc/shadow` 读该账户并判定（Linux）。
    ///
    /// 只在 Linux 编译：本机（Windows）没有 `/etc/shadow`。
    #[cfg(target_os = "linux")]
    pub fn submit(&mut self, user: &str, now: u64) -> Decision {
        let text = std::fs::read_to_string(auth::SHADOW_PATH).unwrap_or_default();
        // 读不到 → `parse_shadow` 给 Missing → 判定为"未校验但放行并说明"
        // （理由：读不到 shadow 时把机主永久关在门外，比"放行并明确告知"更糟）
        let entry = auth::parse_shadow(&text, user);
        self.submit_with(&entry, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::FREE_ATTEMPTS as FREE;

    const HASH: &str = "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/";

    fn hash_entry() -> ShadowEntry {
        ShadowEntry::Hash(HASH.to_string())
    }

    #[test]
    fn lock_and_unlock_clear_the_input() {
        let mut l = LockScreen::default();
        assert!(!l.is_locked());
        l.lock();
        assert!(l.is_locked());
        for c in "secret".chars() {
            l.insert(c);
        }
        assert_eq!(l.masked().chars().count(), 6);
        l.unlock();
        assert!(!l.is_locked());
        assert!(l.is_empty(), "解锁后口令必须清掉");
        assert!(l.message().is_none());
    }

    #[test]
    fn masked_shows_one_dot_per_char_not_per_byte() {
        let mut l = LockScreen::default();
        l.lock();
        for c in "abc".chars() {
            l.insert(c);
        }
        assert_eq!(l.masked(), "●●●");
        assert_eq!(l.masked().chars().count(), 3);
        l.clear();
        // 中文口令：按**字符**计数（按字节会显示 5 个点，用户会以为多打了）
        for c in "中文".chars() {
            l.insert(c);
        }
        assert_eq!(l.masked(), "●●");
        assert_eq!(l.masked().chars().count(), 2);
    }

    #[test]
    fn correct_password_unlocks_and_wrong_password_keeps_it_locked() {
        let mut l = LockScreen::default();
        l.lock();
        for c in "nope".chars() {
            l.insert(c);
        }
        let d = l.submit_with(&hash_entry(), 1000);
        assert!(matches!(d, Decision::Deny(_)));
        assert!(l.is_locked(), "错口令必须仍然锁着");
        assert!(l.is_empty(), "被拒后口令字段要清空");
        assert!(l.message().is_some(), "必须给出理由");
        assert_eq!(l.attempts(), 1);

        for c in "hello".chars() {
            l.insert(c);
        }
        assert_eq!(l.submit_with(&hash_entry(), 1001), Decision::Allow);
        assert!(!l.is_locked());
        assert!(l.message().is_none());
    }

    /// 未设口令：**放行但把话说清楚**（窗口期内的提示必须留给用户看）。
    #[test]
    fn no_password_unlocks_with_a_visible_reason() {
        let mut l = LockScreen::default();
        l.lock();
        let d = l.submit_with(&ShadowEntry::NoPassword, 0);
        assert!(
            matches!(d, Decision::Allow | Decision::AllowWithReason(_)),
            "无口令应放行，实得 {d:?}"
        );
        assert!(!l.is_locked());
        let msg = l.message().expect("必须留下说明");
        assert!(msg.contains("未设口令"), "说明要指明原因：{msg}");
    }

    /// 退避期内提交：不动输入、不加次数、只回 `Wait`。
    #[test]
    fn waiting_does_not_count_or_compare() {
        let mut l = LockScreen::default();
        l.lock();
        // 连续输错直到进入退避
        for i in 0..(FREE + 1) {
            l.clear();
            for c in "x".chars() {
                l.insert(c);
            }
            l.submit_with(&hash_entry(), 1000 + i as u64);
        }
        let before = l.attempts();
        let d = l.submit_with(&hash_entry(), 1000 + FREE as u64); // 同一时刻：仍在退避内
        assert!(matches!(d, Decision::Wait(_)), "退避期内应回 Wait，实得 {d:?}");
        assert_eq!(l.attempts(), before, "退避期内不该增加尝试次数");
        assert!(l.message().unwrap().contains("等待"));
    }

    /// **锁一下再解锁不能绕过退避**（退避状态在 lock() 时不重置）。
    #[test]
    fn relocking_does_not_reset_the_backoff() {
        let mut l = LockScreen::default();
        l.lock();
        for i in 0..(FREE + 1) {
            l.clear();
            for c in "x".chars() {
                l.insert(c);
            }
            l.submit_with(&hash_entry(), 2000 + i as u64);
        }
        let t = 2000 + FREE as u64;
        // 用**行为**断言（比读内部数字更结实）：此刻提交只应回 Wait
        l.clear();
        for c in "hello".chars() {
            l.insert(c);
        }
        assert!(
            matches!(l.submit_with(&hash_entry(), t), Decision::Wait(_)),
            "应处于退避中"
        );
        l.unlock();
        l.lock(); // 再锁一次
        l.clear();
        for c in "hello".chars() {
            l.insert(c);
        }
        assert!(
            matches!(l.submit_with(&hash_entry(), t), Decision::Wait(_)),
            "重新锁屏不该把退避清掉（否则「锁一下再解锁」就是绕过办法）"
        );
    }

    #[test]
    fn input_is_length_capped() {
        let mut l = LockScreen::default();
        l.lock();
        for _ in 0..(LockScreen::MAX_CHARS + 50) {
            l.insert('a');
        }
        assert_eq!(l.masked().chars().count(), LockScreen::MAX_CHARS);
    }
}
