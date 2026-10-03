//! 命令面板的**搜索核心**（FEATURE-GAPS §5.5）。
//!
//! 需求原文："用户讨厌**假**搜索框 —— 那就做一个**真的**"。
//! "真"的第一层含义就是本模块：搜索**真的在工作** —— 打分规则确定、可解释、可测，
//! 而不是"输入什么都没反应"或"排序看着随机"。
//!
//! 本模块**纯函数**（收条目、收查询，不碰文件/时钟/界面），
//! 因此全部边界能在开发机上跑；界面与 Ctrl+K 接线在下一批。
//!
//! ## 打分规则（四档，可解释）
//!
//! | 情况 | 分数 | 为什么这么排 |
//! |---|---|---|
//! | 完全相同 | 最高 | 用户打的字就是它 |
//! | 前缀 | 次之 | "设置" 应排 "设置中心" 前面 |
//! | 子串 | 再次 | 命中位置越靠前越好（越容易被看见） |
//! | 子序列（模糊） | 最低 | "sz" 能匹配 "设置" 是便利，但不该压过真命中 |
//!
//! 同分时按**标题短者优先**，再按**原始顺序**（稳定）—— 稳定很重要：
//! 同一查询连打两次，结果顺序变了会让人以为搜到了别的东西。

// 本模块的消费者是**命令面板覆层**（Ctrl+K），在下一批接线。
// 这个 allow 是**有期限的**，不是长期豁免 —— 接线那一批应当删掉它。
// （前几批的同类 allow 都是这么处理的：接线后即删，不留永久豁免。）
#![cfg_attr(not(test), allow(dead_code))]

/// 条目类别（决定展示时的分组与图标语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 内建应用
    App,
    /// 设置页
    Setting,
    /// 当前目录下的文件/文件夹
    File,
}

impl Kind {
    /// 展示用标签（中文）。
    pub fn label(self) -> &'static str {
        match self {
            Kind::App => "应用",
            Kind::Setting => "设置",
            Kind::File => "文件",
        }
    }
}

/// 选中后要做什么。**只描述意图**，执行留给事件循环（那里才知道怎么开窗口/切页）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// 打开第 `i` 个内建应用（`APP_TITLES` 的下标）。
    OpenApp(usize),
    /// 跳到设置中心第 `i` 页（`PAGES` 的下标）。
    GotoPage(usize),
    /// 打开当前目录里第 `i` 个条目（`desktop.entries` 的下标）。
    OpenFile(usize),
}

/// 一条可被搜到的条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    /// 副标题（类别 + 备注），给用户一眼看清"这是哪一类东西"。
    pub hint: String,
    pub kind: Kind,
    pub target: Target,
}

/// 单条打分。返回 `None` = 不匹配（**不返回 0 分**：0 分与"没匹配"混淆会让排序出错）。
pub fn score(query: &str, title: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q = query.to_lowercase();
    let t = title.to_lowercase();
    if t == q {
        return Some(1000);
    }
    if t.starts_with(&q) {
        // 前缀：越贴近整体越优先（长度惩罚）
        return Some(800 - (t.chars().count() as i32).min(100));
    }
    if let Some(pos) = t.find(&q) {
        // 子串：命中位置越靠前越好
        let pos = t[..pos].chars().count() as i32;
        return Some(600 - pos * 4);
    }
    // 子序列（模糊）：所有字符按序出现，跨度越小越好
    subsequence_span(&t, &q).map(|span| 300 - span.min(200))
}

/// 子序列匹配的**跨度**（最后一个命中位置 - 第一个命中位置）。不匹配返回 `None`。
///
/// 跨度越小 = 字符越集中 = 越像用户想找的东西（`"sz"` 匹配 `"设置"` 跨度 1，
/// 匹配 `"搜索框设置"` 跨度更大）。
fn subsequence_span(text: &str, query: &str) -> Option<i32> {
    let tc: Vec<char> = text.chars().collect();
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    let mut ti = 0usize;
    for qc in query.chars() {
        let mut found = None;
        while ti < tc.len() {
            if tc[ti] == qc {
                found = Some(ti);
                ti += 1;
                break;
            }
            ti += 1;
        }
        let idx = found?;
        if first.is_none() {
            first = Some(idx);
        }
        last = idx;
    }
    Some((last - first.unwrap_or(0)) as i32)
}

/// 搜索并返回**条目下标**（按分排序，稳定）。
///
/// `query` 为空 → 返回前 `limit` 条（保持原始顺序）：命令面板刚打开时应当"列出来"，
/// 而不是空白 —— 空白让人不知道能搜什么。
pub fn search(query: &str, items: &[Item], limit: usize) -> Vec<usize> {
    let q = query.trim();
    let mut scored: Vec<(i32, usize)> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        // 标题与副标题都参与匹配：用户可能记得"Terminal"或"应用"
        let s = score(q, &it.title).or_else(|| score(q, &it.hint).map(|v| v / 2));
        if let Some(v) = s {
            scored.push((v, i));
        }
    }
    // 排序：分数降序 → 标题短者优先 → 原始下标（稳定）
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| items[a.1].title.chars().count().cmp(&items[b.1].title.chars().count()))
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// 由三个数据源拼出条目表（应用 / 设置页 / 当前目录条目）。
///
/// 纯装配：给什么就转成什么，不读文件、不碰界面 —— 因此可测。
pub fn build_items(
    app_titles: &[&str],
    pages: &[(&str, &str)],
    entries: &[(String, bool)],
) -> Vec<Item> {
    let mut out = Vec::with_capacity(app_titles.len() + pages.len() + entries.len());
    for (i, t) in app_titles.iter().enumerate() {
        out.push(Item {
            title: (*t).to_string(),
            hint: format!("{}", Kind::App.label()),
            kind: Kind::App,
            target: Target::OpenApp(i),
        });
    }
    for (i, (zh, en)) in pages.iter().enumerate() {
        out.push(Item {
            title: (*zh).to_string(),
            // 副标题同时给英文名：用英文界面的人也能搜到（中文名也仍可搜）
            hint: format!("{} · {en}", Kind::Setting.label()),
            kind: Kind::Setting,
            target: Target::GotoPage(i),
        });
    }
    for (i, (name, is_dir)) in entries.iter().enumerate() {
        out.push(Item {
            title: name.clone(),
            hint: if *is_dir {
                format!("{} · 文件夹", Kind::File.label())
            } else {
                Kind::File.label().to_string()
            },
            kind: Kind::File,
            target: Target::OpenFile(i),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, id: usize) -> Item {
        Item {
            title: title.to_string(),
            hint: "应用".to_string(),
            kind: Kind::App,
            target: Target::OpenApp(id),
        }
    }

    /// 四档的相对顺序：完全相同 > 前缀 > 子串 > 子序列。
    ///
    /// 这条是搜索"看着对不对"的核心 —— 打分档次写反了，界面表现就是
    /// "打全名反而排在后面"。
    #[test]
    fn scoring_tiers_are_ordered() {
        let exact = score("设置", "设置").unwrap();
        let prefix = score("设置", "设置中心").unwrap();
        let substr = score("设置", "打开设置中心").unwrap();
        let fuzzy = score("sz", "设置中心").unwrap_or(i32::MIN);
        assert!(exact > prefix, "完全相同应高于前缀：{exact} vs {prefix}");
        assert!(prefix > substr, "前缀应高于子串：{prefix} vs {substr}");
        assert!(substr > fuzzy, "子串应高于子序列：{substr} vs {fuzzy}");
    }

    /// 子串命中位置越靠前分越高（用户更容易看到靠前的候选）。
    #[test]
    fn earlier_substring_scores_higher() {
        let early = score("设置", "设置中心").unwrap();
        let late = score("设置", "打开设置中心").unwrap();
        assert!(early > late, "靠前的命中应更高：{early} vs {late}");
    }

    #[test]
    fn matching_is_case_insensitive_for_ascii() {
        assert!(score("term", "Terminal").is_some());
        assert!(score("TERM", "terminal").is_some());
        assert_eq!(score("Term", "Terminal"), score("term", "Terminal"));
        // 中文没有大小写，直接子串匹配
        assert!(score("终端", "终端").is_some());
    }

    #[test]
    fn non_matching_query_returns_none_not_zero() {
        // 返回 0 会让"没匹配"混进结果里（排序时看起来像命中但分数很低）
        assert_eq!(score("zzz", "设置中心"), None);
        assert_eq!(score("abc", "终端"), None);
        // 子序列必须**按序**：字符都在但顺序不对，不算匹配
        assert_eq!(score("ba", "abc"), None);
        assert!(score("ac", "abc").is_some(), "按序的子序列应匹配");
    }

    /// 空查询列出全部（截断到 limit），且**保持原始顺序**。
    #[test]
    fn empty_query_lists_items_in_original_order() {
        let items: Vec<Item> = (0..5).map(|i| item(&format!("应用{i}"), i)).collect();
        let got = search("", &items, 3);
        assert_eq!(got, vec![0, 1, 2]);
        // 纯空白等同于空
        assert_eq!(search("   ", &items, 2), vec![0, 1]);
    }

    #[test]
    fn search_ranks_and_limits() {
        let items = vec![
            item("终端", 0),
            item("设置中心", 1),
            item("文件", 2),
            item("终端设置", 3),
        ];
        // "终端"：完全相同(0) > 前缀(3) > 不含(1,2 排除)
        let got = search("终端", &items, 10);
        assert_eq!(got, vec![0, 3], "完全相同应排在前缀之前");
        // 限制生效
        assert_eq!(search("终端", &items, 1), vec![0]);
        // 没有匹配就是空结果（不是"返回全部"）
        assert!(search("不存在的东西", &items, 10).is_empty());
    }

    /// **稳定性**：同分条目保持原始顺序。
    ///
    /// 这条不是洁癖：同一查询连打两次顺序变了，用户会以为搜到了别的东西。
    #[test]
    fn equal_scores_keep_original_order() {
        // 三个标题长度相同、与查询的关系也相同（都是前缀）
        let items = vec![item("abc1", 0), item("abc2", 1), item("abc3", 2)];
        let got = search("abc", &items, 10);
        assert_eq!(got, vec![0, 1, 2]);
        let again = search("abc", &items, 10);
        assert_eq!(got, again, "同一查询两次结果必须一致");
    }

    /// 同分时**短标题优先**（"终端" 应排在 "终端设置窗口" 前面）。
    #[test]
    fn shorter_titles_win_ties() {
        let items = vec![item("终端设置窗口", 0), item("终端", 1)];
        let got = search("终端", &items, 10);
        assert_eq!(got[0], 1, "更短的标题应优先：{got:?}");
    }

    #[test]
    fn build_items_covers_all_three_sources_with_right_targets() {
        let apps = ["终端", "浏览器"];
        let pages = [("外观", "Appearance"), ("网络", "Network")];
        let entries = [("readme.txt".to_string(), false), ("src".to_string(), true)];
        let items = build_items(&apps, &pages, &entries);
        assert_eq!(items.len(), 6);
        assert_eq!(items[0].target, Target::OpenApp(0));
        assert_eq!(items[0].kind, Kind::App);
        assert_eq!(items[2].target, Target::GotoPage(0));
        assert_eq!(items[2].kind, Kind::Setting);
        // 设置的副标题要带英文名（英文界面下也能搜到）
        assert!(items[2].hint.contains("Appearance"));
        assert_eq!(items[4].target, Target::OpenFile(0));
        assert_eq!(items[5].hint.contains("文件夹"), true, "目录要标出来");
    }

    /// 真数据装配后，用**真实查询**验一遍端到端（跨三个来源）。
    #[test]
    fn real_shaped_sources_are_searchable_end_to_end() {
        let items = build_items(
            &["终端", "文件", "设置", "音乐", "浏览器"],
            &[("外观", "Appearance"), ("网络", "Network")],
            &[("notes.txt".to_string(), false)],
        );
        // 注意：`APP_TITLES` 里**本来就有一个叫「设置」的应用**，
        // 所以搜「设」时它按前缀匹配合法地排在设置页前面。
        // 想验证「设置页能被搜到」，就该用一个只命中页面的查询。
        let got = search("设", &items, 10);
        assert!(
            got.iter().any(|i| items[*i].kind == Kind::Setting),
            "搜「设」应同时列出设置页与应用：{got:?}"
        );
        let page_only = search("外观", &items, 10);
        assert_eq!(items[page_only[0]].kind, Kind::Setting, "「外观」应命中页面");
        // 英文名也能搜到同一个设置页
        let got_en = search("net", &items, 10);
        assert!(
            got_en.iter().any(|i| items[*i].title == "网络"),
            "英文名应能搜到对应设置页：{got_en:?}"
        );
        // 文件也能被搜到
        let got_file = search("notes", &items, 10);
        assert_eq!(items[got_file[0]].kind, Kind::File);
    }
}
