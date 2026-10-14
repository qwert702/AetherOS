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

/// 一条**结构化双语**的通知正文（FEATURE-GAPS §3.5 遗留项 #43）。
///
/// ## 为什么要把两种语言绑成一个值
///
/// 通知正文此前是各发送方写好的 `String`，于是「漏掉英文」在编译期**看不见**：
/// 2026-10-11 实测 117 个生产调用点里，有 5 处真的只有中文（英文界面会冒出中文），
/// 另有 7 处把 `if is_en() {…}` 写在调用点上、38 处停在旧的 `pick_zh_en`。
///
/// 绑成类型之后：
/// * **漏一种语言 → 编译不过**（构造器要求两种都给）；
/// * 英文位里混进中文 → `scripts/i18n-check.py` 判红；
/// * 忘了改某个调用点 → `scripts/toast-i18n-audit.py --check` 判红。
///
/// 三条判据互相独立，但都指向同一个事实：**这条通知有没有英文**。
///
/// ## 与 `tr()` 的分工
///
/// `tr()` 返回 `&'static str`，适合"当场渲染"的界面文案；
/// 通知正文要**存进历史**（可能之后才显示、还可能中途切语言），
/// 所以这里持有 `String`，取词发生在显示层（与审计 `level`/`verdict`
/// 存结构化 token、显示层取词同一原则）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastMsg {
    zh: String,
    en: String,
}

impl ToastMsg {
    /// 两种语言都写全。**不提供"只给一种"的构造器** —— 要用自外部文案见 [`Self::from_external`]。
    pub fn new(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self { zh: zh.into(), en: en.into() }
    }

    /// 两种语言各带**一个**占位符 `{}`（与既有 `main::pick_zh_en_1` 同一约定）。
    pub fn new_1(zh: &str, en: &str, a: impl std::fmt::Display) -> Self {
        Self { zh: fill(zh, &[&a]), en: fill(en, &[&a]) }
    }

    /// 文案由**别处**（aetherd）按其连接语言渲染好了：两种语言存同一份，**不再翻译**。
    ///
    /// 为什么必须显式写出来：给一条已经本地化的文案再套一层双语，会得到中英混排。
    /// 用这个方法等于在调用点上声明「这里的语言由对端负责」——
    /// 审计脚本 `toast-i18n-audit.py` 会把它单列一档。
    pub fn from_external(text: impl Into<String>) -> Self {
        let t = text.into();
        Self { zh: t.clone(), en: t }
    }

    /// 按指定语言取词（**纯函数**，测试不必碰全局状态）。
    pub fn for_lang(&self, l: Lang) -> &str {
        match l {
            Lang::Zh => &self.zh,
            Lang::En => &self.en,
        }
    }

    /// 按**当前**语言取词（显示层用）。
    pub fn text(&self) -> &str {
        self.for_lang(lang())
    }
}

/// 按顺序替换模板里的 `{}`；占位符少于参数时**多余参数被丢弃**（不 panic）。
///
/// 与 `format!` 的区别：模板可以是**运行时字符串**，所以中英两种模板能并排写在调用点上。
fn fill(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + args.len() * 8);
    let mut rest = template;
    for a in args {
        match rest.find("{}") {
            Some(i) => {
                out.push_str(&rest[..i]);
                out.push_str(&a.to_string());
                rest = &rest[i + 2..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
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

    /// 结构化双语：两种语言都留着，取词按语言走（**历史在切语言后要能重渲染**）。
    #[test]
    fn toast_msg_keeps_both_languages() {
        let m = ToastMsg::new("已保存", "Saved");
        assert_eq!(m.for_lang(Lang::Zh), "已保存");
        assert_eq!(m.for_lang(Lang::En), "Saved");
        // 相等性按**整体**比：合并判定依赖它（两种语言都变了才算"同一条"）
        assert_eq!(m, ToastMsg::new("已保存", "Saved"));
        assert_ne!(m, ToastMsg::new("已保存", "Stored"));
    }

    /// 占位符按各自的模板填 —— 中英模板并排写，改一处不会漏另一处。
    #[test]
    fn toast_msg_fills_placeholders_per_language() {
        let one = ToastMsg::new_1("正在重启服务 {}…", "Restarting service {}…", "aetherd");
        assert_eq!(one.for_lang(Lang::Zh), "正在重启服务 aetherd…");
        assert_eq!(one.for_lang(Lang::En), "Restarting service aetherd…");

        // 两个及以上占位符：中英各写一条完整句（模板形态不同时最稳妥，也不会串位）
        let two = ToastMsg::new(
            format!("移入回收站：{}（{} 项）", "a.txt", 3),
            format!("Moved to trash: {} ({} items)", "a.txt", 3),
        );
        assert_eq!(two.for_lang(Lang::Zh), "移入回收站：a.txt（3 项）");
        assert_eq!(two.for_lang(Lang::En), "Moved to trash: a.txt (3 items)");
    }

    /// 模板缺占位符时**不 panic**（多余参数丢掉），否则一条文案就能让桌面崩。
    #[test]
    fn toast_msg_with_missing_placeholders_does_not_panic() {
        let m = ToastMsg::new_1("没有占位符", "no placeholder", 42);
        assert_eq!(m.for_lang(Lang::Zh), "没有占位符");
        assert_eq!(m.for_lang(Lang::En), "no placeholder");
    }

    /// 外部来源（aetherd 已按其连接语言渲染）：两种语言是同一份，**不再翻译**。
    #[test]
    fn external_text_is_not_translated() {
        let m = ToastMsg::from_external("AI 回复");
        assert_eq!(m.for_lang(Lang::Zh), "AI 回复");
        assert_eq!(m.for_lang(Lang::En), "AI 回复");
    }
}
