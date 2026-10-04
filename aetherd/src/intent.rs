//! 离线快速意图通道：高频系统指令的确定性匹配。
//!
//! 设计动机：'把窗口排成两列'这类操作不需要大模型——本地规则秒级、
//! 离线可用、零成本、隐私零外泄。LLM 只接管规则覆盖不了的自然语言。
//! 这是 AI-First OS 的"快慢双思"架构：快思考（规则）先行，慢思考（LLM）兜底。

use serde_json::{json, Value};

/// 桌面行为指令（经 Response::Action 下发给 Shell/合成器执行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAction {
    pub name: String,
    pub arguments: Value,
}

impl DesktopAction {
    fn layout(layout: &str) -> Self {
        Self { name: "layout_set".into(), arguments: json!({ "layout": layout }) }
    }
}

/// 意图处理结果：回复文本 + 可选的桌面行为。
pub type Intent = (String, Option<DesktopAction>);

/// 对用户输入做快速意图匹配；命中返回 (回复, 行为)，未命中返回 None（交给 LLM）。
///
/// `en`：回复用英文还是中文（§3.5）。收参数而不读全局：这个模块是**纯规则匹配**，
/// 保持可测；语言由调用方（IPC 请求）带来。
pub fn try_handle(text: &str, en: bool) -> Option<Intent> {
    let t = text.replace(' ', "");
    let lower = text.to_lowercase();
    // 英文触发词用"完整短语或整词"匹配原始输入：过宽的子串（如 "time"、"float"）
    // 会把 "解释 time complexity"、"把 float 变量改成 int" 误当成系统指令（P2-16）。
    // 中文短语不受词边界影响，继续在剥离空格的文本上匹配。
    let en_phrase = |p: &str| lower.contains(p);
    let en_word = |w: &str| {
        lower
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|tok| tok == w)
    };

    // 布局指令（支持中英混合说法）
    if contains_any(&t, &["两列", "排成两列", "两栏"]) || en_phrase("two columns") || en_phrase("two col") {
        return Some((
            reply(en, "好的，已把窗口排成两列。", "Done — windows are now in two columns."),
            Some(DesktopAction::layout("two_col")),
        ));
    }
    if contains_any(&t, &["三列", "排成三列", "三栏"]) || en_phrase("three columns") || en_phrase("three col") {
        return Some((
            reply(en, "好的，已把窗口排成三列。", "Done — windows are now in three columns."),
            Some(DesktopAction::layout("three_col")),
        ));
    }
    if contains_any(&t, &["独占", "堆叠", "铺满", "最大化桌面"]) || en_word("monocle") {
        return Some((
            reply(en, "好的，已切换为独占堆叠。", "Done — switched to monocle (full-screen stack)."),
            Some(DesktopAction::layout("monocle")),
        ));
    }
    if contains_any(&t, &["自由布局", "自由模式"]) || en_phrase("float layout") || en_phrase("floating layout") {
        return Some((
            reply(en, "好的，已切换为自由布局。", "Done — switched to free (floating) layout."),
            Some(DesktopAction::layout("float")),
        ));
    }
    // 模糊指令的确定性解释："整理桌面" = 平铺全部窗口
    if contains_any(&t, &["整理", "收拾", "排列窗口"]) || en_word("tidy") {
        return Some((
            reply(
                en,
                "已为您整理桌面：窗口平铺成两列。",
                "Tidied your desktop — windows are tiled into two columns.",
            ),
            Some(DesktopAction::layout("two_col")),
        ));
    }

    // 时间
    if contains_any(&t, &["几点", "时间"]) || en_phrase("what time") || en_phrase("current time") {
        let clock = crate::text_clock();
        return Some((if en {
            format!("It is {clock} now.")
        } else {
            format!("现在是 {clock}。")
        }, None));
    }

    // 系统状态
    if contains_any(&t, &["系统状态", "内存", "服务状态"]) || en_phrase("system status") || en_phrase("status report") {
        return Some((crate::sys_brief(en), None));
    }

    None
}

/// 同一条快速意图回复的两种语言（§3.5）。
///
/// 与合成器里的 `pick_zh_en` 同一个用意：让"这条话必须有两种语言"在代码里可见。
fn reply(en: bool, zh: &'static str, english: &'static str) -> String {
    if en {
        english.to_string()
    } else {
        zh.to_string()
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

#[cfg(test)]
mod tests {

/// §3.5：快速意图的回复**两种语言都要有**，且英文里不许出现汉字。
///
/// 这条测试能存在，是因为 `try_handle` 收了 `en: bool` 而不是在内部读全局 ——
/// 纯函数才好这样成对断言。
#[test]
fn fast_intents_reply_in_both_languages() {
    for (input, has_action) in [
        ("把窗口排成两列", true),
        ("three columns please", true),
        ("切换成自由布局", true),
        ("现在几点了", false),
        ("系统状态", false),
    ] {
        let (zh, a_zh) = try_handle(input, false).unwrap_or_else(|| panic!("{input} 应命中"));
        let (en, a_en) = try_handle(input, true).unwrap_or_else(|| panic!("{input} 应命中（英文）"));
        assert_eq!(a_zh.is_some(), has_action, "{input} 行为判定");
        assert_eq!(a_en.is_some(), has_action, "{input} 行为判定（英文）");
        assert!(!zh.is_empty() && !en.is_empty(), "{input} 两种语言都要有回复");
        assert!(
            !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{input} 的英文回复里有汉字：{en}"
        );
    }
}
    use super::*;

    #[test]
    fn two_col_intent() {
        let (reply, action) = try_handle("把窗口排成两列", false).unwrap();
        assert!(reply.contains("两列"));
        assert_eq!(action.unwrap().arguments["layout"], "two_col");
    }

    #[test]
    fn three_col_with_english() {
        let (_, action) = try_handle("three columns please", false).unwrap();
        assert_eq!(action.unwrap().arguments["layout"], "three_col");
    }

    #[test]
    fn english_substrings_do_not_misfire() {
        // 英文关键词过宽会误命中普通对话（P2-16 回归）
        assert!(try_handle("解释一下 time complexity", false).is_none());
        assert!(try_handle("把 float 变量改成 int", false).is_none());
        assert!(try_handle("sort these two numbers", false).is_none());
    }

    #[test]
    fn clock_intent_no_action() {
        let (_, action) = try_handle("现在几点了", false).unwrap();
        assert!(action.is_none());
    }

    #[test]
    fn unmatched_goes_to_llm() {
        assert!(try_handle("帮我在 Blender 里做一个金属材质", false).is_none());
        assert!(try_handle("解释一下量子纠缠", false).is_none());
    }

    #[test]
    fn privacy_thing_not_matched_here() {
        // 含"密码"的请求不走快速通道，交给路由器（会强制本地 LLM）
        assert!(try_handle("帮我生成一个强密码", false).is_none());
    }

    #[test]
    fn tidy_desktop_maps_to_tiling() {
        let (reply, action) = try_handle("帮我整理一下桌面", false).unwrap();
        assert!(reply.contains("整理"));
        assert_eq!(action.unwrap().arguments["layout"], "two_col");
    }

    #[test]
    fn open_app_intent_via_action_tool_only() {
        // 打开应用走 desktop 工具（LLM 或后续规则扩展），快速通道不处理
        assert!(try_handle("打开终端", false).is_none());
    }
}
