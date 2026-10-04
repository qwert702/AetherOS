//! 登录/锁屏的**认证策略层**（FEATURE-GAPS §2.3）。
//!
//! 与 [`crate::sha512crypt`] 的分工：那个负责"哈希算得对不对"，这里负责
//! "该不该放行、放行几次、失败怎么处理"。本模块**故意不碰文件与时钟**
//! （收文本、收时间戳），因此边界能在开发机上全部测到。
//!
//! ## 三条诚实设计
//!
// 生产消费者是**锁屏的 Linux 路径**（`LockScreen::submit` 读 `/etc/shadow`）。
// 在非 Linux（开发机）上只有测试用到它 —— 这与仓库里 `control_service` 的写法一致：
// 用按平台的 allow 表达「这些代码只在目标平台上被生产代码调用」，而不是笼统豁免。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// 与 `sha512crypt` 同样的情况：调用它的**界面**（登录/锁屏）在同一批的后续提交里接线。
// 这个 `allow` 是**有期限的**，不是长期豁免 —— 界面接线时应删掉。
//! 1. **没有口令 ≠ 拒绝**。Live 演示镜像默认不设口令（审计 M-4）。
//!    锁屏若因为"没口令"而拒绝解锁，用户会被永久关在门外 —— 那是把
//!    "安全默认"做成了自伤。正确做法是：**放行，但在界面上说清楚**
//!    "本镜像未设口令，这道锁不防物理接触"。
//! 2. **限流是内存态，不是持久态**。失败次数存在进程里，重启合成器即清零。
//!    也就是说它是**挡随手猜**的减速带，不是防住有耐心的攻击者。
//!    要持久化就得有可写位置（持久分区可能没挂），那会让"锁屏"依赖"持久分区"，
//!    反而更容易把用户关在门外。这条边界写在注释里，不假装它是防爆破。
//! 3. **不永久锁定**。指数退避而不是"错 5 次就永久锁死" —— 单用户机器上，
//!    永久锁定等于让路过的人按几下键盘就能把机主锁在门外（拒绝服务）。

use crate::sha512crypt;

/// `/etc/shadow` 里某账户的口令项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowEntry {
    /// 文件里没有这个账户（或读不到文件）—— **无法判断**，不是"没有口令"。
    Missing,
    /// 有账户、第 2 字段**就是空的** = 未设口令：登录语义上允许无口令进入。
    ///
    /// 这与 `Locked` 是**两件完全不同的事**，绝不能合并（见 `Locked` 的说明）。
    NoPassword,
    /// 口令被**禁用**：`!`、`*`、`!$6$...`（`passwd -l` 会写成这种）。
    ///
    /// **必须拒绝**：这是管理员"关掉这个口令"的明确动作。
    /// 曾经把它和 `NoPassword` 合并 → 结果是 `passwd -l` 之后锁屏**按回车就进**，
    /// 一个"增加限制"的动作反而取消了限制（见安全审计附录 L）。
    Locked,
    /// 有哈希需要校验，并带上账户过期日（shadow 第 8 字段，`None` = 不过期）。
    Hash {
        hash: String,
        expires_day: Option<i64>,
    },
}

/// `/etc/shadow` 的路径。
pub const SHADOW_PATH: &str = "/etc/shadow";

/// 前几次失败**不施加延迟**（正常手误不该被罚）。
pub const FREE_ATTEMPTS: u32 = 3;

/// 退避延迟的上限（秒）。到顶就一直是这个值，不做"永久锁定"。
pub const MAX_DELAY_SECS: u64 = 60;

/// 登录失败的指数退避。
///
/// 纯状态机：所有时间都由调用方传入（`now` 秒），因此可以在测试里"快进"。
#[derive(Debug, Clone, Default)]
pub struct Lockout {
    fails: u32,
    locked_until: u64,
}

impl Lockout {
    /// 已经失败了几次。
    pub fn fails(&self) -> u32 {
        self.fails
    }

    /// 还要等多少秒才能再试（0 = 现在就能试）。
    pub fn retry_after(&self, now: u64) -> u64 {
        self.locked_until.saturating_sub(now)
    }

    /// 记录一次失败，返回**本次施加的等待秒数**（0 = 不罚）。
    pub fn on_failure(&mut self, now: u64) -> u64 {
        self.fails = self.fails.saturating_add(1);
        let delay = Self::delay_for(self.fails);
        if delay > 0 {
            self.locked_until = now.saturating_add(delay);
        }
        delay
    }

    /// 成功：清空计数（成功之后不该还背着上次的罚）。
    pub fn on_success(&mut self) {
        self.fails = 0;
        self.locked_until = 0;
    }

    /// 第 `fails` 次失败对应的延迟：超过免费次数后按 2 的幂增长，封顶 [`MAX_DELAY_SECS`]。
    pub fn delay_for(fails: u32) -> u64 {
        if fails <= FREE_ATTEMPTS {
            return 0;
        }
        let steps = (fails - FREE_ATTEMPTS).min(6); // 2^6 = 64 → 会被上限截住
        (1u64 << steps).min(MAX_DELAY_SECS)
    }
}

/// 一次解锁尝试的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 放行。
    Allow,
    /// 放行，但**原因必须显示给用户**（例如"本镜像未设口令"）。
    AllowWithReason(String),
    /// 拒绝，并给出理由（会显示在界面上，也会进日志）。
    Deny(String),
    /// 退避中：还要等 `secs` 秒。**此时根本不去比对口令**（否则等于免费给出比对次数）。
    Wait(u64),
}

/// 从 `/etc/shadow` 文本里取某账户的口令项。
///
/// 纯函数。格式：`user:hash:lastchg:min:max:warn:inactive:expire:reserved`，
/// 只看**第 2 个字段**（口令）。第 8 个字段（账户过期日）也看一眼 —— 过期账户不该放行。
pub fn parse_shadow(text: &str, user: &str) -> ShadowEntry {
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split(':');
        let Some(name) = f.next() else { continue };
        if name != user {
            continue;
        }
        let hash = f.next().unwrap_or("");
        // 第 8 个字段是"账户过期日"（自 1970-01-01 起的天数，空 = 不过期）
        // 第 8 字段：账户过期日（自 1970-01-01 起的天数，空 = 不过期）。
        // **只解析出来，不在这里判"是否已过期"** —— 那需要"今天"，
        // 而本函数拿不到时钟（模块的设计约定：不碰时钟，时钟由调用方传入）。
        let expires_day = f.nth(5).unwrap_or("").trim().parse::<i64>().ok();
        return classify_hash(hash, expires_day);
    }
    ShadowEntry::Missing
}

/// 把 shadow 的第 2 字段分类。
///
/// **三个分支对应三种完全不同的语义**（这张表就是本函数存在的意义）：
///
/// | 取值 | 含义 | 处置 |
/// |---|---|---|
/// | 空 | 未设口令 | [`ShadowEntry::NoPassword`]（允许，带理由） |
/// | `!` / `*` / `!$6$…` | **口令被禁用 / 账户被锁定** | [`ShadowEntry::Locked`]（**拒绝**） |
/// | `$6$…` 等 | 有哈希 | [`ShadowEntry::Hash`]（校验） |
fn classify_hash(hash: &str, expires_day: Option<i64>) -> ShadowEntry {
    let h = hash.trim();
    if h.is_empty() {
        // 只有**真的空**才是"未设口令"。
        // 注意与下面那条的区别：这里允许进入，`Locked` 一律拒绝。
        return ShadowEntry::NoPassword;
    }
    if h.starts_with('!') || h.starts_with('*') {
        // `passwd -l` 会写成 `!$6$...`（保留哈希但禁用）；`*` 是"永不可能匹配的占位"。
        // 两者都是**管理员明确关掉了这个口令**，不能当成"没有口令"。
        return ShadowEntry::Locked;
    }
    ShadowEntry::Hash {
        hash: h.to_string(),
        expires_day,
    }
}

/// 判定一次解锁尝试。
///
/// 顺序**很重要**：先看退避，再看账户形态，最后才比对口令。
/// 退避中若还去比对，等于给攻击者"免费的比对次数"，指数退避就白设了。
pub fn decide(
    entry: &ShadowEntry,
    password: &str,
    lockout: &mut Lockout,
    now: u64,
) -> Decision {
    // 1) 退避中：直接让等，**不比对**
    let wait = lockout.retry_after(now);
    if wait > 0 {
        return Decision::Wait(wait);
    }
    // 2) 账户形态
    match entry {
        ShadowEntry::Missing => Decision::AllowWithReason(
            "读不到 /etc/shadow 里的该账户 —— **本次未校验口令**".to_string(),
        ),
        ShadowEntry::NoPassword => Decision::AllowWithReason(
            "该账户未设口令（Live 演示镜像的默认状态）—— 这道锁不防物理接触".to_string(),
        ),
        // **口令被禁用 = 一律拒绝**。这里是"管理员关掉了这个口令"，
        // 与"读不到 shadow"（无法判断）不是一回事：前者必须拒绝，后者才是放行并说明。
        ShadowEntry::Locked => Decision::Deny(
            "该账户的口令已被禁用（/etc/shadow 里的 ! 或 * —— 通常是 passwd -l 的结果）"
                .to_string(),
        ),
        ShadowEntry::Hash { hash, expires_day } => {
            // 过期日与"现在"比：**未来才过期的不算过期**。
            // （原来把任何非零值都当已过期，会把 2030 年到期的账户现在就拒掉。）
            // 注意 `entry` 是引用，所以解构出来的是 `&i64` —— 比之前先解引用。
            if let Some(days) = *expires_day {
                if days > 0 && days.saturating_mul(86_400) <= now as i64 {
                    return Decision::Deny(format!(
                        "账户已于第 {days} 天过期（/etc/shadow 的过期字段）"
                    ));
                }
            }
            match sha512crypt::verify_password(password, hash) {
            Ok(()) => {
                lockout.on_success();
                Decision::Allow
            }
                Err(why) => {
                    // 口令错 → 记失败并施加退避；**其它错误（方案不支持/格式坏）也拒绝**，
                    // 但不计入失败次数 —— 那不是用户手误，罚他等待毫无意义，
                    // 而且会让"哈希方案不对"这种系统问题被误认为"他一直输错"。
                    if why == "口令不正确" {
                        let delay = lockout.on_failure(now);
                        if delay > 0 {
                            Decision::Deny(format!(
                                "{why}（第 {} 次失败，请等待 {delay} 秒后再试）",
                                lockout.fails()
                            ))
                        } else {
                            Decision::Deny(why)
                        }
                    } else {
                        Decision::Deny(why)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHADOW_LOCKED: &str = "root:$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/:19000:0:99999:7:::\n";
    const SHADOW_NO_PW: &str = "root::19000:0:99999:7:::\ndaemon:*:19000:0:99999:7:::\n";
    const SHADOW_LOCKED_HASH: &str = "root:!$6$abcdefgh$xx:19000:0:99999:7:::\n";
    const SHADOW_EXPIRED: &str = "root:$6$abcdefgh$bp46:19000:0:99999:7::1:\n";

    /// 路径是契约：写错成 `/etc/passwd`（那里没有哈希）会让"锁屏永远放行"，
    /// 而且看起来像"口令校验失效"。这条断言把它钉住。
    #[test]
    fn shadow_path_is_the_real_one() {
        assert_eq!(SHADOW_PATH, "/etc/shadow");
        assert_ne!(SHADOW_PATH, "/etc/passwd", "passwd 里没有口令哈希");
    }

    #[test]
    fn shadow_parsing_distinguishes_the_four_states() {
        assert_eq!(
            parse_shadow(SHADOW_LOCKED, "root"),
            ShadowEntry::Hash { hash: "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/".to_string(), expires_day: None }
        );
        assert_eq!(parse_shadow(SHADOW_NO_PW, "root"), ShadowEntry::NoPassword);
        // `*` = 占位符，永不可能匹配 —— **是「口令被禁用」，不是「没有口令」**
        assert_eq!(parse_shadow(SHADOW_NO_PW, "daemon"), ShadowEntry::Locked);
        // `!$6$...` 是 `passwd -l` 的写法：保留哈希但禁用 → **必须拒绝**
        assert_eq!(parse_shadow(SHADOW_LOCKED_HASH, "root"), ShadowEntry::Locked);
        // 账户过期：解析出的是**过期日**，是否已过期由 `decide` 用「现在」判断
        assert_eq!(
            parse_shadow(SHADOW_EXPIRED, "root"),
            ShadowEntry::Hash { hash: "$6$abcdefgh$bp46".to_string(), expires_day: Some(1) }
        );
        // 不存在的账户
        assert_eq!(parse_shadow(SHADOW_LOCKED, "nobody"), ShadowEntry::Missing);
        // 空文本 / 注释行
        assert_eq!(parse_shadow("", "root"), ShadowEntry::Missing);
        assert_eq!(parse_shadow("# 注释\n", "root"), ShadowEntry::Missing);
        // 字段不全的行不该 panic（格式漂移的现实存在）
        assert_eq!(parse_shadow("root:\n", "root"), ShadowEntry::NoPassword);
        assert_eq!(parse_shadow("root\n", "root"), ShadowEntry::NoPassword);
    }

    #[test]
    fn backoff_grows_then_caps_and_never_locks_forever() {
        // 前几次免费
        assert_eq!(Lockout::delay_for(0), 0);
        assert_eq!(Lockout::delay_for(1), 0);
        assert_eq!(Lockout::delay_for(FREE_ATTEMPTS), 0);
        // 之后 2 的幂增长
        assert_eq!(Lockout::delay_for(FREE_ATTEMPTS + 1), 2);
        assert_eq!(Lockout::delay_for(FREE_ATTEMPTS + 2), 4);
        assert_eq!(Lockout::delay_for(FREE_ATTEMPTS + 3), 8);
        // 封顶，且**不会**变成"永久"
        for f in 4..40 {
            assert!(
                Lockout::delay_for(FREE_ATTEMPTS + f) <= MAX_DELAY_SECS,
                "第 {f} 次失败的上限被突破了"
            );
        }
        assert_eq!(Lockout::delay_for(1000), MAX_DELAY_SECS);
    }

    #[test]
    fn lockout_blocks_retries_until_the_wait_elapses() {
        let mut lk = Lockout::default();
        assert_eq!(lk.retry_after(100), 0);
        // 三次免费失败
        for _ in 0..FREE_ATTEMPTS {
            assert_eq!(lk.on_failure(100), 0);
        }
        // 第四次开始罚
        assert_eq!(lk.on_failure(100), 2);
        assert_eq!(lk.retry_after(100), 2, "刚失败应当要等 2 秒");
        assert_eq!(lk.retry_after(101), 1);
        assert_eq!(lk.retry_after(102), 0, "等到了就该放行重试");
        // 成功清零
        lk.on_success();
        assert_eq!(lk.fails(), 0);
        assert_eq!(lk.retry_after(102), 0);
    }

    /// 退避中**根本不去比对口令**（否则指数退避形同虚设）。
    #[test]
    fn waiting_short_circuits_before_any_password_comparison() {
        let mut lk = Lockout::default();
        for _ in 0..(FREE_ATTEMPTS + 1) {
            lk.on_failure(1000);
        }
        assert!(lk.retry_after(1000) > 0);
        let entry = ShadowEntry::Hash { hash: "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/".to_string(), expires_day: None };
        // 即便口令**是对的**，退避期内也只回 Wait
        let d = decide(&entry, "hello", &mut lk, 1000);
        match d {
            Decision::Wait(s) => assert!(s > 0),
            other => panic!("退避期内应回 Wait，实得 {other:?}"),
        }
        // 等过了就要正常放行（且不再多罚）
        let d2 = decide(&entry, "hello", &mut lk, 2000);
        assert_eq!(d2, Decision::Allow);
    }

    /// 没有口令 → **放行但带理由**（锁屏不能因为"没口令"把机主关在门外）。
    #[test]
    fn no_password_allows_with_an_honest_reason() {
        let mut lk = Lockout::default();
        for entry in [ShadowEntry::NoPassword, ShadowEntry::Missing] {
            let d = decide(&entry, "", &mut lk, 0);
            assert!(
                matches!(d, Decision::Allow | Decision::AllowWithReason(_)),
                "{entry:?} 应放行，实得 {d:?}"
            );
            match d {
                Decision::AllowWithReason(r) => {
                    assert!(!r.is_empty(), "必须给出可显示的理由");
                    assert!(
                        r.contains("未校验") || r.contains("未设口令"),
                        "理由要说清是「没校验」而不是「校验通过」：{r}"
                    );
                }
                other => panic!("应带理由，实得 {other:?}"),
            }
        }
    }

    #[test]
    fn wrong_password_denies_and_counts_but_scheme_errors_do_not() {
        let mut lk = Lockout::default();
        let ok = ShadowEntry::Hash { hash: "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/".to_string(), expires_day: None };
        // 错口令 → 计入失败
        assert!(matches!(decide(&ok, "nope", &mut lk, 0), Decision::Deny(_)));
        assert_eq!(lk.fails(), 1);
        // 方案不支持 / 格式坏 → 拒绝但**不计失败**（那不是用户手误，
        // 计进去会让"哈希方案不对"被误认为"他一直输错"）
        let md5 = ShadowEntry::Hash { hash: "$1$abc$def".to_string(), expires_day: None };
        let d = decide(&md5, "nope", &mut lk, 0);
        assert!(matches!(d, Decision::Deny(_)));
        assert_eq!(lk.fails(), 1, "系统性的错误不该计入用户失败次数");
        // 正确口令 → 放行并清零
        assert_eq!(decide(&ok, "hello", &mut lk, 0), Decision::Allow);
        assert_eq!(lk.fails(), 0);
    }

    /// 过期账户被拒 —— 用**正确口令**证明拒绝的原因是过期，而不是口令错。
    #[test]
    fn expired_account_is_denied() {
        let mut lk = Lockout::default();
        // 1970-01-02（第 1 天）：早就过期了
        let past = ShadowEntry::Hash { hash: "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/".to_string(), expires_day: Some(1) };
        let d = decide(&past, "hello", &mut lk, 1_791_112_953);
        assert!(matches!(d, Decision::Deny(_)), "过期账户即便口令正确也要拒：{d:?}");
        assert_eq!(lk.fails(), 0, "过期不是用户手误，不该计入失败");
    }

    /// **审计发现的严重问题（附录 L）**：`!` / `*` 是「**口令被禁用**」，必须**拒绝**。
    ///
    /// 修之前这两者被归入 `NoPassword` 并**放行** —— 于是管理员执行 `passwd -l user`
    /// （或写入 `*`）之后，锁屏**按回车就能进**：一个"增加限制"的动作反而取消了限制。
    /// 这条测试把两种写法都钉死。
    #[test]
    fn locked_password_is_denied_and_never_allowed() {
        let mut lk = Lockout::default();
        for shadow in [SHADOW_LOCKED_HASH, "root:*:19000:0:99999:7:::\n"] {
            let entry = parse_shadow(shadow, "root");
            assert_eq!(entry, ShadowEntry::Locked, "应当解析成 Locked");
            // 空口令、随便什么口令，都不能进
            for pw in ["", "hello", "!$6$abcdefgh$xx"] {
                let d = decide(&entry, pw, &mut lk, 0);
                assert!(
                    matches!(d, Decision::Deny(_)),
                    "口令被禁用的账户用 {pw:?} 竟然放行了：{d:?}"
                );
            }
        }
        assert_eq!(lk.fails(), 0, "「口令被禁用」不是用户输错，不该计入失败次数");
    }

    /// 过期日在**未来**时不算过期（原来把所有非零值都当已过期，会误拒正常账户）。
    #[test]
    fn future_expiry_date_is_not_expired() {
        let mut lk = Lockout::default();
        // 第 8 字段是"自 1970-01-01 起的天数"：47500 ≈ 2100 年
        let future = ShadowEntry::Hash { hash: "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/".to_string(), expires_day: Some(47_500) };
        let d = decide(&future, "hello", &mut lk, 1_791_112_953);
        assert_eq!(d, Decision::Allow, "未来才过期的账户不该现在被拒：{d:?}");
        // 同一个哈希、过期日已到 → 拒绝
        let past = ShadowEntry::Hash { hash: "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/".to_string(), expires_day: Some(20_000) };
        assert!(
            matches!(decide(&past, "hello", &mut lk, 1_791_112_953), Decision::Deny(_)),
            "已到期就必须拒"
        );
    }

    /// UUID 那句测试占位：失败计入只针对"口令不正确"，不针对其它错误。
    #[test]
    fn only_wrong_password_is_counted() {
        let mut lk = Lockout::default();
        let bad_fmt = ShadowEntry::Hash { hash: "$6$".to_string(), expires_day: None };
        for _ in 0..5 {
            let _ = decide(&bad_fmt, "x", &mut lk, 0);
        }
        assert_eq!(lk.fails(), 0);
    }
}
