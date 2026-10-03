//! 应用商店的目录模型（FEATURE-GAPS §5.11）。
//!
//! 后端早已完备（`aetherd` 的 `app_list` / `app_install` / `app_remove` +
//! `mkapp.py` 造包），缺的是"让装应用从命令行变成点一下"。本模块负责那一半的**数据**：
//! 扫出可安装的包、判断"该装还是已装"。
//!
//! ## 为什么合成器自己解析 `app.json`
//!
//! `aetherd` 是 bin crate，合成器**不能**依赖它（那会把服务端整个链进 UI）。
//! 所以这里独立解析 —— 代价是字段名可能与服务端漂移，而漂移的表现是
//! "商店里显示的应用点了装不上"（很难查）。因此有一条**交叉核对测试**
//! （`catalog_fields_match_the_installer_manifest`）直接去读 `aetherd/src/apps.rs`
//! 的 `Manifest`，字段名对不上就红。
//!
//! ## `id` 必须校验
//!
//! `id` 会被用来拼安装路径（`<root>/<id>`），所以它**不是**普通文本：
//! 含 `/`、`..`、绝对路径的 id 必须拒绝。这里按 `aetherd` 的同一条规则校验
//! （只允许 `[a-z0-9]` 与 `-` `_` `.`，首字符为字母数字）。

// 绘制侧（「应用商店」页）在 Windows 上也会用到本模块的视图状态；
// 但「进页面扫描 / 点安装」这条链在 Linux 的 fbdev 事件循环里，
// 因此非目标平台上这些函数只有测试在用 —— 与 `palette`/`spaces`/`setup` 同一处理。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use std::path::Path;

/// 可安装包的目录。
///
/// 为什么是这个路径：`/var/lib/aether/` 已经是本系统的"持久状态"目录
/// （设置文件也在那儿），而 `/var/apps` 是**已安装**的位置（`apps::DEFAULT_DIR`）——
/// 两者必须分开，否则"商店里有什么"与"装了哪些"会混成一个目录，那就再也分不清了。
pub const CATALOG_DIR: &str = "/var/lib/aether/store";

/// 清单文件名（与 `aetherd::apps::MANIFEST_NAME` 一致，有交叉核对测试盯着）。
pub const MANIFEST_NAME: &str = "app.json";

/// 商店里的一个可安装项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogItem {
    pub id: String,
    pub name: String,
    pub version: String,
    pub desc: String,
}

/// 这一项当前该做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreAction {
    /// 还没装
    Install,
    /// 装了但是版本不同（重新安装 / 升级）
    Reinstall,
    /// 已装且版本一致
    UpToDate,
}

impl StoreAction {
    pub fn label(self) -> &'static str {
        match self {
            StoreAction::Install => "安装",
            StoreAction::Reinstall => "重新安装",
            StoreAction::UpToDate => "已安装",
        }
    }

    pub fn label_en(self) -> &'static str {
        match self {
            StoreAction::Install => "Install",
            StoreAction::Reinstall => "Reinstall",
            StoreAction::UpToDate => "Installed",
        }
    }

    /// 是否可点（"已安装"不该像个按钮一样能点）。
    pub fn actionable(self) -> bool {
        !matches!(self, StoreAction::UpToDate)
    }
}

/// `id` 是否合法：只允许 `[a-z0-9]` 与 `-` `_` `.`，且首字符为字母或数字。
///
/// **安全相关**：这个值要拼进安装路径（`<root>/<id>`），
/// 所以 `..`、`/`、绝对路径、空串都必须拒绝 —— 与 `aetherd` 侧同一规则。
pub fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// 解析一份清单。**缺 `id`/`name` 或 id 不合法 → `None`**（该包被跳过）。
///
/// 其余字段缺省为空串（`mkapp.py` 生成的清单里 `version` 可能就是空的）。
pub fn parse_manifest(text: &str) -> Option<CatalogItem> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
    if !valid_id(id) || name.trim().is_empty() {
        return None;
    }
    Some(CatalogItem {
        id: id.to_string(),
        name: name.to_string(),
        version: v.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        desc: v.get("desc").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

/// 扫描目录里的全部可安装项，**按 id 排序**。
///
/// 顺序固定是刻意的：扫描顺序依赖文件系统，而界面顺序一跳变，用户会以为列表变了。
/// 单个包坏掉只跳过它（不整批失败）—— 一个清单写错不该让整个商店打不开。
pub fn scan_catalog(dir: &Path) -> Vec<CatalogItem> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path.join(MANIFEST_NAME)) else {
            continue;
        };
        if let Some(item) = parse_manifest(&text) {
            out.push(item);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 这一项现在该做什么。`installed` 是 (`id`, `version`) 列表（来自 `apps::list`）。
///
/// 版本都是空串时视为"已装" —— 造包工具默认不写版本，
/// 若把"两边都空"判成需要重装，用户每次打开商店都会看到一排"重新安装"。
pub fn action_for(item: &CatalogItem, installed: &[(String, String)]) -> StoreAction {
    match installed.iter().find(|(id, _)| *id == item.id) {
        None => StoreAction::Install,
        Some((_, v)) if *v == item.version => StoreAction::UpToDate,
        Some(_) => StoreAction::Reinstall,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, version: &str) -> CatalogItem {
        CatalogItem {
            id: id.to_string(),
            name: format!("应用 {id}"),
            version: version.to_string(),
            desc: String::new(),
        }
    }

    /// **交叉核对**：本模块独立解析清单，字段名必须与安装器的权威定义一致。
    ///
    /// 漂移的表现是"商店里显示的应用点了装不上"，很难查 —— 所以直接盯源码。
    #[test]
    fn catalog_fields_match_the_installer_manifest() {
        let src = include_str!("../../aetherd/src/apps.rs");
        for field in ["id", "name", "version", "desc"] {
            assert!(
                src.contains(&format!("pub {field}:")),
                "`aetherd::apps::Manifest` 里找不到字段 `{field}` —— 两边解析的清单字段名漂移了"
            );
        }
        assert!(
            src.contains("app.json"),
            "安装器侧的清单文件名变了（本模块的 MANIFEST_NAME 要跟着改）"
        );
    }

    #[test]
    fn parses_a_typical_manifest() {
        let text = r#"{"id":"notes","name":"记事本","version":"1.2","entry":"bin/notes","args":[],"desc":"随手记点东西"}"#;
        let it = parse_manifest(text).expect("应当能解析");
        assert_eq!(it.id, "notes");
        assert_eq!(it.name, "记事本");
        assert_eq!(it.version, "1.2");
        assert_eq!(it.desc, "随手记点东西");
        // 最小清单（造包工具默认 version 为空）也要能解析
        let min = parse_manifest(r#"{"id":"x","name":"X"}"#).unwrap();
        assert_eq!(min.version, "");
        assert_eq!(min.desc, "");
    }

    #[test]
    fn rejects_manifests_without_a_usable_id_or_name() {
        for bad in [
            "",
            "not json",
            "{}",
            r#"{"name":"没有 id"}"#,
            r#"{"id":"","name":"空 id"}"#,
            r#"{"id":"x"}"#,                        // 缺 name
            r#"{"id":"x","name":"   "}"#,           // 空白 name
            r#"{"id":123,"name":"id 不是字符串"}"#,
            // **路径穿越**：id 会被拼成安装路径，这三类必须拒绝
            r#"{"id":"../etc","name":"穿越"}"#,
            r#"{"id":"a/b","name":"带斜杠"}"#,
            r#"{"id":"/abs","name":"绝对路径"}"#,
            r#"{"id":".hidden","name":"点开头"}"#,
        ] {
            assert!(parse_manifest(bad).is_none(), "不该接受：{bad}");
        }
        // 合法字符集要放行（含 - _ .，但不能以它们开头）
        for ok in ["a", "a1", "my-app", "my_app", "my.app", "A1"] {
            assert!(valid_id(ok), "{ok} 应当合法");
        }
        assert!(!valid_id(&"x".repeat(65)), "过长要拒绝");
    }

    #[test]
    fn scanning_sorts_by_id_and_skips_broken_packages() {
        let dir = std::env::temp_dir().join(format!(
            "aether-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // 两个合法包（故意让目录名与 id 顺序不同）、一个坏清单、一个非目录文件
        for (sub, json) in [
            ("z", r#"{"id":"zebra","name":"斑马"}"#),
            ("a", r#"{"id":"apple","name":"苹果"}"#),
            ("broken", "{ 这不是 json"),
        ] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join(MANIFEST_NAME), json).unwrap();
        }
        std::fs::write(dir.join("README"), "not a dir").unwrap();

        let items = scan_catalog(&dir);
        assert_eq!(items.len(), 2, "坏包要跳过、文件要跳过：{items:?}");
        assert_eq!(items[0].id, "apple", "要按 id 排序（顺序固定，界面才不会跳）");
        assert_eq!(items[1].id, "zebra");
        // 目录不存在 → 空列表，而不是 panic
        assert!(scan_catalog(&dir.join("nope")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn action_reflects_whether_and_which_version_is_installed() {
        let it = item("notes", "1.2");
        assert_eq!(action_for(&it, &[]), StoreAction::Install);
        assert_eq!(
            action_for(&it, &[("notes".into(), "1.2".into())]),
            StoreAction::UpToDate
        );
        assert_eq!(
            action_for(&it, &[("notes".into(), "1.0".into())]),
            StoreAction::Reinstall
        );
        // 别的应用装了不算
        assert_eq!(action_for(&it, &[("other".into(), "9".into())]), StoreAction::Install);
        // **两边版本都为空 → 已装**（否则每次打开商店都是一排"重新安装"）
        let nov = item("x", "");
        assert_eq!(action_for(&nov, &[("x".into(), "".into())]), StoreAction::UpToDate);
    }

    #[test]
    fn only_actionable_states_are_clickable() {
        assert!(StoreAction::Install.actionable());
        assert!(StoreAction::Reinstall.actionable());
        assert!(!StoreAction::UpToDate.actionable(), "「已安装」不该像个按钮");
        // 四种文案都要有，且中英不同
        for a in [StoreAction::Install, StoreAction::Reinstall, StoreAction::UpToDate] {
            assert!(!a.label().is_empty() && !a.label_en().is_empty());
            assert_ne!(a.label(), a.label_en());
        }
    }
}

// ---------------------------------------------------------------------------
// 视图状态
// ---------------------------------------------------------------------------

/// 「应用商店」页的状态。
///
/// 目录扫描是**本地**操作（不像 DNS/策略要经 aetherd），但仍然放在这里而不是绘制里：
/// 绘制每帧都跑，扫描每帧都读一遍磁盘是自找的卡顿 —— 所以扫描只在"进页面/点重新扫描"时做。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreView {
    dir: std::path::PathBuf,
    items: Vec<CatalogItem>,
    installed: Vec<(String, String)>,
    scanned: bool,
    error: Option<String>,
}

impl StoreView {
    /// 重新扫描：读目录 + 记下当前已安装的应用。
    ///
    /// 目录不存在**不算错误**：全新系统上还没有商店目录是正常的，
    /// 界面该说"还没有可安装的应用"，而不是弹一个吓人的错误。
    pub fn refresh(&mut self, dir: &std::path::Path, installed: Vec<(String, String)>) {
        self.dir = dir.to_path_buf();
        self.items = scan_catalog(dir);
        self.installed = installed;
        self.scanned = true;
        self.error = None;
    }

    /// 只更新"已安装"这份信息（安装成功后调用，不必重扫目录）。
    pub fn set_installed(&mut self, installed: Vec<(String, String)>) {
        self.installed = installed;
    }

    pub fn items(&self) -> &[CatalogItem] {
        &self.items
    }

    pub fn is_scanned(&self) -> bool {
        self.scanned
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// 第 `i` 项该做什么（越界 → `None`）。
    pub fn action_of(&self, i: usize) -> Option<StoreAction> {
        self.items.get(i).map(|it| action_for(it, &self.installed))
    }

    /// 第 `i` 项的**包目录**（安装时交给 `app_install` 的 `path`）。
    pub fn path_of(&self, i: usize) -> Option<std::path::PathBuf> {
        let it = self.items.get(i)?;
        Some(self.dir.join(&it.id))
    }

    /// 行的副标题：版本 + 该做什么。
    pub fn row_hint(&self, i: usize, lang: crate::i18n::Lang) -> String {
        let Some(it) = self.items.get(i) else {
            return String::new();
        };
        let act = action_for(it, &self.installed);
        let label = crate::i18n::pick(lang, act.label(), act.label_en());
        let ver = if it.version.is_empty() {
            crate::i18n::tr("（未标版本）", "(no version)")
        } else {
            &it.version
        };
        let desc = if it.desc.is_empty() { "" } else { &it.desc };
        if desc.is_empty() {
            format!("{ver} · {label}")
        } else {
            format!("{ver} · {label} · {desc}")
        }
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aether-storeview-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_pkg(dir: &std::path::Path, id: &str, name: &str, version: &str, desc: &str) {
        std::fs::create_dir_all(dir.join(id)).unwrap();
        std::fs::write(
            dir.join(id).join(MANIFEST_NAME),
            format!(r#"{{"id":"{id}","name":"{name}","version":"{version}","desc":"{desc}"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn refresh_scans_and_classifies_against_installed() {
        let dir = tmpdir("scan");
        write_pkg(&dir, "notes", "记事本", "1.0", "随手记");
        write_pkg(&dir, "paint", "画图", "2.0", "");

        let mut v = StoreView::default();
        assert!(!v.is_scanned());
        v.refresh(&dir, vec![("notes".into(), "1.0".into())]);
        assert!(v.is_scanned() && !v.is_empty());
        assert_eq!(v.items().len(), 2);
        assert_eq!(v.action_of(0), Some(StoreAction::UpToDate), "notes 已装且版本一致");
        assert_eq!(v.action_of(1), Some(StoreAction::Install), "paint 没装");
        assert_eq!(v.action_of(9), None, "越界要给 None 而不是 panic");
        // 包目录要拼对（安装时把它交给 app_install）
        assert_eq!(v.path_of(0), Some(dir.join("notes")));
        assert_eq!(v.path_of(9), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 安装成功后只更新"已安装"那份信息，不必重扫目录 —— 那一行立刻变成"已安装"。
    #[test]
    fn set_installed_flips_a_row_without_rescanning() {
        let dir = tmpdir("installed");
        write_pkg(&dir, "notes", "记事本", "1.0", "");
        let mut v = StoreView::default();
        v.refresh(&dir, vec![]);
        assert_eq!(v.action_of(0), Some(StoreAction::Install));
        v.set_installed(vec![("notes".into(), "1.0".into())]);
        assert_eq!(v.action_of(0), Some(StoreAction::UpToDate), "装完那一行要变成已安装");
        // 已装列表本身不暴露计数：状态由 `action_of` 如实反映（那才是界面用的东西）
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **目录不存在不是错误**：全新系统上还没有商店目录是正常的。
    #[test]
    fn a_missing_catalog_directory_is_not_an_error() {
        let mut v = StoreView::default();
        v.refresh(&tmpdir("missing").join("nope"), vec![]);
        assert!(v.is_scanned(), "扫过就要记「扫过」，界面才好区分「还没扫」");
        assert!(v.is_empty());
        assert!(v.error().is_none(), "目录不存在不该报错");
    }

    #[test]
    fn row_hint_carries_version_and_action_in_both_languages() {
        use crate::i18n::Lang;
        let dir = tmpdir("hint");
        write_pkg(&dir, "notes", "记事本", "1.0", "随手记");
        let mut v = StoreView::default();
        v.refresh(&dir, vec![]);
        let zh = v.row_hint(0, Lang::Zh);
        assert!(zh.contains("1.0") && zh.contains("安装") && zh.contains("随手记"), "{zh}");
        let en = v.row_hint(0, Lang::En);
        assert!(en.contains("1.0") && en.contains("Install"), "{en}");
        // 没标版本要说清楚，而不是留一段空白
        let dir2 = tmpdir("hint2");
        write_pkg(&dir2, "x", "X", "", "");
        let mut v2 = StoreView::default();
        v2.refresh(&dir2, vec![]);
        assert!(v2.row_hint(0, Lang::Zh).contains("未标版本"));
        assert!(v2.row_hint(9, Lang::Zh).is_empty(), "越界给空串");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
