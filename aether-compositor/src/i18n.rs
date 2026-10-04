//! 界面多语言（FEATURE-GAPS §3.5）。
//!
//! ## 为什么用 `tr(中文, English)` 而不是键值表
//!
//! 键值表（`tr("settings.appearance")` + 一个注册表）有两个真实代价：
//! ① **漏翻会被漏掉**——新增界面文案时忘了加注册项，运行时才显形（或静静显示键名）；
//! ② 需要维护一份"所有键"的清单，而它自己也会漂移。
//!
//! 在调用点同时写两种语言则**不可能漏**：写下一个字符串时，两种语言就在旁边。
//! 代价是文案分散在各处、没有集中词汇表 —— 对这个规模（几百条）是划算的。
//!
//! ## 迁移是**增量**的，且如实标注
//!
//! 目前迁移的只有**设置中心左栏（页名与分组）+ 语言开关本身 + 锁屏文案**。
//! 设置页正文、控制中心、菜单栏、通知、终端等仍是中文 ——
//! 这是**未完成**，不是"已支持英文"，README 与缺口清单里要按这个口径写。
//!
//! 为什么不一口气全替换：那是一次改几千行、无法逐行审的改动，而且没有测试能覆盖
//! "每个界面字符串都被翻译了"。宁可小步、可数、可测。

use std::sync::atomic::{AtomicU8, Ordering};

/// 界面语言。数值与 `Settings::locale` 的下标一致（0 中文 / 1 English）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// 中文（默认）
    #[default]
    Zh = 0,
    /// English
    En = 1,
}

impl Lang {
    /// 从设置里的下标取（**越界一律退回中文**，不 panic）。
    pub fn from_locale(locale: u8) -> Lang {
        match locale {
            1 => Lang::En,
            _ => Lang::Zh,
        }
    }

    /// 语言自身的显示名（用**该语言自己**写：英文界面里不该出现"英语"两个字）。
    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "English",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(Lang::Zh as u8);

pub fn set_lang(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    Lang::from_locale(LANG.load(Ordering::Relaxed))
}

/// 按语言二选一（**纯函数**：测试不必碰全局状态，也就不会有并行用例互相污染）。
/// 当前是不是英文。**给"要把语言传进数据层"的调用点用**（例如
/// `audit::export_text(…, en)`、`Entry::age_text_en`）——
/// 数据层不该依赖 `i18n`，所以那里收的是 `bool`，由界面层用这个函数转。
pub fn is_en() -> bool {
    lang() == Lang::En
}

pub fn pick(l: Lang, zh: &'static str, en: &'static str) -> &'static str {
    match l {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// 按**当前**语言二选一。调用点同时写下两种文案，因此不可能漏翻。
pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    pick(lang(), zh, en)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_mapping_round_trips_and_clamps_out_of_range() {
        assert_eq!(Lang::from_locale(0), Lang::Zh);
        assert_eq!(Lang::from_locale(1), Lang::En);
        // 越界：**退回中文**而不是 panic（设置文件是外部输入，可能被手改成任意值）
        assert_eq!(Lang::from_locale(2), Lang::Zh);
        assert_eq!(Lang::from_locale(255), Lang::Zh);
        // 存回去的下标由 `Settings::locale` 持有（apply_hit 直接存下标），
        // 所以这里不需要 as_locale —— 不为"可能有用"留接口。
        assert_eq!(Lang::En as u8, 1);
        assert_eq!(Lang::Zh as u8, 0);
    }

    #[test]
    fn pick_is_pure_and_both_languages_are_provided() {
        assert_eq!(pick(Lang::Zh, "外观", "Appearance"), "外观");
        assert_eq!(pick(Lang::En, "外观", "Appearance"), "Appearance");
        // 语言名用该语言自己写（英文界面里出现"英语"两个字是半成品的样子）
        assert_eq!(Lang::Zh.label(), "中文");
        assert_eq!(Lang::En.label(), "English");
    }
}
