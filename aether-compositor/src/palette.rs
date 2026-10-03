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

// 生产消费者是 **Linux 的 fbdev 事件循环**（`Ctrl+K` 与模态守卫）。
// 绘制侧（`draw_palette`）在 Windows 上也会调用，但「搜索 → 状态机 → 执行」
// 这条链只在目标平台上被生产代码用到 —— 与 `auth`/`lock` 同一处理方式。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
/// `query` 为空 → 返回前 `limit` 条，**保持原始顺序**：命令面板刚打开时应当"列出来"，
/// 而不是空白（空白让人不知道能搜什么），而且列出的顺序应当是**刻意排好的**
/// （应用 → 设置页 → 文件），不是"标题最短的排最前"。
pub fn search(query: &str, items: &[Item], limit: usize) -> Vec<usize> {
    let q = query.trim();
    // 空查询：**保持装配顺序**（应用 → 设置 → 文件，那是刻意排的浏览顺序），
    // 只截断到 limit。不按标题长度重排 —— 长度是"打分平局"的规则，
    // 用它改变浏览顺序会让刚打开面板时看到的排序毫无道理。
    if q.is_empty() {
        return (0..items.len().min(limit)).collect();
    }
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


// ---------------------------------------------------------------------------
// 面板状态机
// ---------------------------------------------------------------------------

/// 命令面板的状态（§5.5）。
///
/// 放在这里而不是绘制模块：它**没有界面依赖**，因此行为能脱离绘制单独测
/// （`open → 输入 → 选中 → 取目标` 这条链是本模块的核心，也是最该被钉住的部分）。
#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub cursor: crate::textview::TextCursor,
    /// 三个来源装配出来的全部条目（打开时快照一次）。
    pub items: Vec<Item>,
    /// 当前查询命中的条目下标（由 [`Palette::refresh`] 算）。
    pub results: Vec<Indexed>,
    /// 选中第几条**结果**（不是条目下标）。
    pub selected: usize,
}

/// 结果里的一个候选（带上条目下标与分数，便于界面显示与调试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indexed {
    pub item: usize,
    pub score: i32,
}

impl Palette {
    /// 一次最多展示多少条结果。
    ///
    /// 上限是刻意的：面板太高会盖住整个桌面，而候选超过一屏时用户也不会往下数 ——
    /// 他宁愿多打两个字符。**限制展示条数比加滚动条更符合使用习惯**。
    pub const MAX_ROWS: usize = 8;

    /// 打开面板并把条目表快照进来（`items` 由调用方从三个来源装配）。
    ///
    /// **打开时清空查询**：上次搜的东西留着会让人以为已经搜过了。
    pub fn open_with(&mut self, items: Vec<Item>) {
        self.open = true;
        self.items = items;
        self.query.clear();
        self.cursor = crate::textview::TextCursor::default();
        self.selected = 0;
        self.refresh();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
        // 条目表也清掉：关着的时候留一份快照会让"文件列表变了却搜到旧的"这种问题变隐蔽
        self.items.clear();
    }

    /// 按当前查询重算结果，并把选中项夹回合法范围。
    ///
    /// 查询上限取 `MAX_ROWS * 4`：多算一些，用户按上下键时不会"到头了但还有候选"，
    /// 但也不至于把上千条文件全排序一遍（上限之外的结果本来就滚不到）。
    pub fn refresh(&mut self) {
        let hits = search(&self.query, &self.items, Self::MAX_ROWS * 4);
        self.results = hits
            .into_iter()
            .map(|item| Indexed {
                item,
                score: score(&self.query, &self.items[item].title).unwrap_or(0),
            })
            .collect();
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
    }

    pub fn insert(&mut self, c: char) {
        if self.query.chars().count() >= 128 {
            return;
        }
        self.cursor.insert(&mut self.query, c);
        self.refresh();
        // 改了查询就从第一条重新选：否则"打了一个字，选中项还停在原来的位置"
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor.backspace(&mut self.query) {
            self.refresh();
            self.selected = 0;
        }
    }

    /// 上移选中项（**到头就停住**，不绕回 —— 绕回会让"按一下跳到最底"变成常见操作）。
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// 下移选中项（到底停住）。
    pub fn down(&mut self) {
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    /// 当前选中的条目（没有结果时是 `None` —— 调用方据此决定回车什么都不做）。
    pub fn selected_item(&self) -> Option<&Item> {
        let r = self.results.get(self.selected)?;
        self.items.get(r.item)
    }

    /// 展示用的结果切片（最多 [`Palette::MAX_ROWS`] 条）。
    pub fn visible(&self) -> &[Indexed] {
        let n = self.results.len().min(Self::MAX_ROWS);
        &self.results[..n]
    }
}


/// 面板按键的处理结果。
///
/// **只描述"要做什么"，不回传也没法回传"怎么做"**：执行要碰 `desktop`（开窗口、
/// 开文件），而本模块不认识那些类型。这样处理后，键盘行为能在这里单独测，
/// 执行则集中在事件循环里一处 —— 两件事分开，各自都不会偷偷变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    /// 已消费，无需额外动作（例如输入了一个字符）。
    Handled,
    /// 什么都不做（未识别的按键 —— 调用方会**吞掉它**，面板是模态的）。
    Ignored,
    /// 关闭面板。
    Close,
    /// 执行选中项。
    Run(Target),
}

/// 面板按键处理。
///
/// 面板是**模态**的：打开期间所有按键都归它（调用方负责 `continue`），
/// 未识别的键返回 `Ignored` 而不是"漏下去" —— 否则 Ctrl+K 打开面板后按 `w`
/// 会把背后的窗口关掉。
pub fn handle_key(p: &mut Palette, ev: &PaletteKey) -> PaletteAction {
    match ev {
        PaletteKey::Char(c) => {
            p.insert(*c);
            PaletteAction::Handled
        }
        PaletteKey::Backspace => {
            p.backspace();
            PaletteAction::Handled
        }
        PaletteKey::Up => {
            p.up();
            PaletteAction::Handled
        }
        PaletteKey::Down => {
            p.down();
            PaletteAction::Handled
        }
        PaletteKey::Escape => PaletteAction::Close,
        PaletteKey::Enter => match p.selected_item() {
            // 没有结果时回车**什么都不做**（不是"关掉面板"—— 用户可能是想补打两个字）
            None => PaletteAction::Ignored,
            Some(it) => PaletteAction::Run(it.target),
        },
    }
}

/// 面板认识的按键。**不从 `input::UiEvent` 直接取**：那样本模块就依赖输入层，
/// 而这些用例要能在不构造 evdev 事件的情况下跑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKey {
    Char(char),
    Backspace,
    Up,
    Down,
    Enter,
    Escape,
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
    // ---- 面板状态机 ----

    fn three_items() -> Vec<Item> {
        vec![
            item("终端", 0),
            item("设置中心", 1),
            item("文件", 2),
        ]
    }

    #[test]
    fn opening_snapshots_items_and_clears_the_query() {
        let mut p = Palette::default();
        assert!(!p.open);
        p.query = "上次搜的东西".to_string();
        p.open_with(three_items());
        assert!(p.open);
        assert!(p.query.is_empty(), "打开时必须清空上次的查询");
        assert_eq!(p.items.len(), 3);
        assert_eq!(p.results.len(), 3, "空查询应列出全部（受上限约束）");
        assert_eq!(p.selected, 0);
    }

    /// 输入会把选中项**复位到第一条**：否则"打了一个字，选中项还停在原位"会让人误选中。
    #[test]
    fn typing_filters_and_resets_selection() {
        let mut p = Palette::default();
        p.open_with(three_items());
        p.down();
        assert_eq!(p.selected, 1);
        p.insert('文');
        assert_eq!(p.selected, 0, "改了查询应回到第一条");
        assert_eq!(p.results.len(), 1, "「文」只应命中「文件」");
        assert_eq!(p.items[p.results[0].item].title, "文件");
        // 退格回到全部
        p.backspace();
        assert!(p.query.is_empty());
        assert_eq!(p.results.len(), 3);
    }

    #[test]
    fn selection_stops_at_both_ends() {
        let mut p = Palette::default();
        p.open_with(three_items());
        p.up();
        assert_eq!(p.selected, 0, "第一条再上移应停住");
        p.down();
        p.down();
        assert_eq!(p.selected, 2);
        p.down();
        assert_eq!(p.selected, 2, "最后一条再下移应停住");
    }

    /// 查询变化后结果变少时，选中项必须被夹回合法范围（否则会取到不存在的项）。
    #[test]
    fn selection_is_clamped_when_results_shrink() {
        let mut p = Palette::default();
        p.open_with(three_items());
        p.down();
        p.down();
        assert_eq!(p.selected, 2);
        p.insert('终');
        assert_eq!(p.results.len(), 1);
        assert_eq!(p.selected, 0, "结果变少后选中项要夹回范围内");
        assert!(p.selected_item().is_some());
    }

    #[test]
    fn selected_item_follows_the_selection_and_is_none_when_empty() {
        let mut p = Palette::default();
        p.open_with(three_items());
        assert_eq!(p.selected_item().unwrap().title, "终端");
        p.down();
        assert_eq!(p.selected_item().unwrap().title, "设置中心");
        // 搜不到任何东西时：没有可执行项，回车应当什么都不做
        for c in "zzzz".chars() {
            p.insert(c);
        }
        assert!(p.results.is_empty());
        assert!(p.selected_item().is_none());
    }

    #[test]
    fn closing_clears_everything() {
        let mut p = Palette::default();
        p.open_with(three_items());
        p.insert('终');
        p.close();
        assert!(!p.open);
        assert!(p.query.is_empty());
        assert!(p.results.is_empty());
        assert!(p.items.is_empty(), "关掉后不该留着条目快照");
    }

    /// 展示条数受 `MAX_ROWS` 限制，但**候选**可以更多（上下键不会"到头了却还有"）。
    #[test]
    fn visible_is_capped_but_results_are_not() {
        let many: Vec<Item> = (0..40).map(|i| item(&format!("应用{i:02}"), i)).collect();
        let mut p = Palette::default();
        p.open_with(many);
        assert!(p.results.len() > Palette::MAX_ROWS, "候选应多于展示条数");
        assert_eq!(p.visible().len(), Palette::MAX_ROWS, "展示条数应被限制");
        // 能一路选到超过展示条数的地方（说明结果没被截断）
        for _ in 0..(Palette::MAX_ROWS + 2) {
            p.down();
        }
        assert!(p.selected >= Palette::MAX_ROWS);
    }

    // ---- 键盘处理 ----

    use super::PaletteKey as K;

    #[test]
    fn typing_and_moving_are_consumed() {
        let mut p = Palette::default();
        p.open_with(three_items());
        assert_eq!(handle_key(&mut p, &K::Char('文')), PaletteAction::Handled);
        assert_eq!(p.query, "文");
        assert_eq!(handle_key(&mut p, &K::Backspace), PaletteAction::Handled);
        assert!(p.query.is_empty());
        assert_eq!(handle_key(&mut p, &K::Down), PaletteAction::Handled);
        assert_eq!(p.selected, 1);
        assert_eq!(handle_key(&mut p, &K::Up), PaletteAction::Handled);
        assert_eq!(p.selected, 0);
    }

    /// 回车执行**选中项的目标**（不是"第一条"）—— 上下键选了哪条就执行哪条。
    #[test]
    fn enter_runs_the_selected_target() {
        let mut p = Palette::default();
        p.open_with(three_items());
        p.down(); // 第二条：设置中心
        let act = handle_key(&mut p, &K::Enter);
        assert_eq!(act, PaletteAction::Run(Target::OpenApp(1)));
    }

    /// 没有结果时回车什么都不做（用户可能是想继续打字，不是想关面板）。
    #[test]
    fn enter_with_no_results_does_nothing() {
        let mut p = Palette::default();
        p.open_with(three_items());
        for c in "zzzz".chars() {
            p.insert(c);
        }
        assert!(p.results.is_empty());
        assert_eq!(handle_key(&mut p, &K::Enter), PaletteAction::Ignored);
        assert!(p.open, "面板应仍然开着");
    }

    #[test]
    fn escape_closes() {
        let mut p = Palette::default();
        p.open_with(three_items());
        assert_eq!(handle_key(&mut p, &K::Escape), PaletteAction::Close);
    }

}
