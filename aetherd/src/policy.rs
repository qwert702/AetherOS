//! 逐工具的权限策略（FEATURE-GAPS §5.3 权限策略编辑器）。
//!
//! 现状：闸门只有一个**全局阈值** `Gate::auto_approve_below`，用户没法说
//! "这个工具每次都问我，那个工具直接允许"。本模块提供那份**逐工具**策略，
//! 以及"策略 + 等级 → 该怎么办"的判定规则。
//!
//! ## 两条不可违背的安全规则
//!
//! 1. **禁止永远优先**。用户明确禁掉的工具，**任何等级都不能绕过** ——
//!    否则"我禁了它"就成了一句空话，而这恰恰是用户设置里最该被信任的一条。
//! 2. **"允许"不能自动放行 L3**。安全审计的结论是"L3 永远不可能 always allow"。
//!    所以对 L3 工具，`Allow` 会**降级为询问**（并在回执里说明原因），
//!    而不是照单全收 —— 用户想省事，但不能省掉"危险操作必须确认"这条底线。
//!
//! ## 解析要**宽容但可控**
//!
//! 策略文件是用户可手改的。坏 JSON 不能让 aetherd 起不来（那是把"配置写错"
//! 升级成"系统不可用"），所以：解析失败 → **退回全默认**（都问）；
//! 单条策略值不认识 → **跳过那一条**（那条退回默认"问"），其余照常。
//! 两种情况都要能在日志里看出来，而不是静默。

// 消费者是**闸门判定**与设置页的「权限」编辑器，在下一批接线。
// 这个 allow 是**有期限的**：接线那一批应当删掉它（前几批都是这么处理的）。
#![cfg_attr(not(test), allow(dead_code))]
use crate::perm::Level;
use std::collections::BTreeMap;

/// 策略文件路径（与 `model.json` 同目录：aetherd 的配置都在 `/etc/aether/` 下）。
pub const POLICY_PATH: &str = "/etc/aether/policy.json";

impl Policy {
    /// 从文件读取；文件不存在或内容坏掉 → **全默认**（不 panic、不放行）。
    ///
    /// 读失败**不报错**是刻意的：aetherd 起不来比"策略退回默认"严重得多。
    /// 但调用方应当把结果写进日志（否则用户会以为自己的设置生效了）。
    pub fn load(path: &std::path::Path) -> (Policy, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => (Policy::parse(&text), None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Policy::default(), None),
            Err(e) => (Policy::default(), Some(format!("读取 {} 失败：{e}", path.display()))),
        }
    }

    /// 原子写入 + 0600。
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 复用 DNS 那套原子写入：临时文件 → fsync → rename。
        // 半截的策略文件比没有策略更危险（解析出一半等于悄悄放宽了另一半）。
        crate::netset::write_atomic(path, &self.to_json())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            /// 与 `model.json` 同级的权限收紧：**策略文件决定「什么能自动执行」，
            /// 被改宽就是安全边界的改动**，所以同样 0600。
            ///
            /// 常量定义**放在这个 cfg 块里**：非 unix 平台根本不会有这个权限概念，
            /// 定义在外面只会得到一个"从未使用"的警告。
            const POLICY_MODE: u32 = 0o600;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(POLICY_MODE));
        }
        Ok(())
    }
}

/// 单个工具的策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    /// **默认**：按等级闸门走（L0/L1 放行、L2+ 询问）。未设置时就是它。
    ///
    /// 为什么必须有这一态：若默认是「每次询问」，那么"加了个策略编辑器"
    /// 就等于"连读文件都要点确认" —— 那是把系统变难用，不是加功能。
    Default,
    /// 每次询问：**即便 L0 也问**（用户可以刻意收紧某个工具）。
    Ask,
    /// 允许（不弹确认，但**受等级上限约束**，见 [`Policy::effective`]）。
    Allow,
    /// 禁止（一律拒绝，且优先于任何"允许"）。
    Deny,
}

impl ToolPolicy {
    /// 存进文件用的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            ToolPolicy::Default => "default",
            ToolPolicy::Ask => "ask",
            ToolPolicy::Allow => "allow",
            ToolPolicy::Deny => "deny",
        }
    }

    /// 从文件读。**不认识就是 `None`**（调用方跳过那条，退回默认）。
    pub fn parse(s: &str) -> Option<ToolPolicy> {
        match s {
            "default" => Some(ToolPolicy::Default),
            "ask" => Some(ToolPolicy::Ask),
            "allow" => Some(ToolPolicy::Allow),
            "deny" => Some(ToolPolicy::Deny),
            _ => None,
        }
    }

    /// 界面上的中文说明（设置页直接显示它）。
    pub fn label(self) -> &'static str {
        match self {
            ToolPolicy::Default => "按等级",
            ToolPolicy::Ask => "每次询问",
            ToolPolicy::Allow => "允许",
            ToolPolicy::Deny => "禁止",
        }
    }
}

/// 策略判定结果（"该怎么办"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// 自动放行。
    Allow,
    /// 需要用户确认（带上要显示给用户的原因）。
    Ask(&'static str),
    /// 直接拒绝（带上原因）。
    Deny(&'static str),
}

impl Effect {
    pub fn is_allow(&self) -> bool {
        matches!(self, Effect::Allow)
    }
}

/// 逐工具策略表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// 只存**非默认**的条目：默认是"每次都问"，存下来只会让文件变长、
    /// 还会让"用户从没设过"与"用户设成了问"混在一起。
    entries: BTreeMap<String, ToolPolicy>,
}

impl Policy {
    /// 某个工具的原始策略（没设过 → [`ToolPolicy::Default`]）。
    pub fn get(&self, tool: &str) -> ToolPolicy {
        self.entries.get(tool).copied().unwrap_or(ToolPolicy::Default)
    }

    /// 设置策略。设回 `Default` 会**删掉条目**（保持"只存非默认"的约定）。
    pub fn set(&mut self, tool: &str, p: ToolPolicy) {
        if p == ToolPolicy::Default {
            self.entries.remove(tool);
        } else {
            self.entries.insert(tool.to_string(), p);
        }
    }

    /// 已显式设置的条目数（界面据此显示"已自定义 N 项"）。
    pub fn customized(&self) -> usize {
        self.entries.len()
    }

    /// **判定规则**（本模块的核心，也是安全上最该审的一段）。
    ///
    /// 顺序不能改：
    /// 1. `Deny` → 拒绝（优先于一切）；
    /// 2. `Ask` → 询问；
    /// 3. `Allow` → 只有**等级不高于闸门的自动放行上限**时才允许，
    ///    否则降级为询问并说明原因。
    pub fn effective(&self, tool: &str, level: Level, max_auto: Level) -> Effect {
        match self.get(tool) {
            ToolPolicy::Deny => Effect::Deny("该工具已被用户明确禁止"),
            ToolPolicy::Ask => Effect::Ask("该工具被设为「每次询问」"),
            // 按等级：低于阈值放行，否则询问。**这正是闸门本来的行为**，
            // 所以"没设过"与"设过"不会在默认状态下产生差异。
            ToolPolicy::Default => {
                if (level as u8) < (max_auto as u8) {
                    Effect::Allow
                } else {
                    Effect::Ask("按等级：该等级需要确认")
                }
            }
            ToolPolicy::Allow => {
                // 绝对规则（与阈值无关）：**L3 永远不能免确认**。
                if level == Level::L3 {
                    Effect::Ask("L3 是危险级：按设计永远需要确认，即便设为「允许」")
                } else if (level as u8) <= (max_auto as u8) {
                    Effect::Allow
                } else {
                    Effect::Ask("该工具的等级高于自动放行上限：即便设为「允许」也必须确认")
                }
            }
        }
    }

    /// 序列化成 JSON（只写非默认条目）。
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"version\":1,\"policy\":{");
        for (i, (k, v)) in self.entries.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('"');
            s.push_str(&escape(k));
            s.push_str("\":\"");
            s.push_str(v.as_str());
            s.push('"');
        }
        s.push_str("}}\n");
        s
    }

    /// 从 JSON 解析。**宽容但可控**：坏 JSON → 空策略（全默认"问"）；
    /// 单条值不认识 → 跳过那一条。
    ///
    /// 为什么不用 `serde` 直接反序列化：那份 `Value` 解析在 aetherd 里已经有
    /// （`modelcfg` 用的就是它），但策略文件要的是"**逐条**容错"——
    /// 一条坏值不该把整份策略丢掉。手写这十几行更直白，也更好测。
    pub fn parse(text: &str) -> Policy {
        let mut p = Policy::default();
        let Some(obj) = extract_policy_object(text) else {
            return p;
        };
        for (k, v) in obj {
            if let Some(pol) = ToolPolicy::parse(&v) {
                p.set(&k, pol);
            }
            // 不认识的策略值：跳过（该条退回默认），其余照常
        }
        p
    }
}

/// 转义 JSON 字符串里的 `"` 与 `\`（工具名来自配置，不能假设它干净）。
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 从文本里抠出 `"policy": { ... }` 里的键值对。
///
/// 手写扫描而不用 serde：这里要的是**逐条容错**（单条坏值不牵连整份），
/// 而 serde 的报错是整份的。扫描规则很窄 —— 只认 `"key":"value"`。
fn extract_policy_object(text: &str) -> Option<Vec<(String, String)>> {
    let start = text.find("\"policy\"")?;
    let rest = &text[start..];
    let open = rest.find('{')?;
    let body = &rest[open + 1..];
    let mut out = Vec::new();
    let mut i = 0;
    let bytes: Vec<char> = body.chars().collect();
    while i < bytes.len() {
        if bytes[i] == '}' {
            break;
        }
        if bytes[i] != '"' {
            i += 1;
            continue;
        }
        // 读 key
        i += 1;
        let mut key = String::new();
        while i < bytes.len() && bytes[i] != '"' {
            if bytes[i] == '\\' && i + 1 < bytes.len() {
                i += 1;
            }
            key.push(bytes[i]);
            i += 1;
        }
        i += 1; // 跳过收尾引号
        // 找冒号与值的开引号
        while i < bytes.len() && bytes[i] != '"' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1;
        let mut val = String::new();
        while i < bytes.len() && bytes[i] != '"' {
            if bytes[i] == '\\' && i + 1 < bytes.len() {
                i += 1;
            }
            val.push(bytes[i]);
            i += 1;
        }
        i += 1;
        out.push((key, val));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **默认 = 按等级**：不动现有行为（L0/L1 放行、L2+ 询问）。
    ///
    /// 这条是防"加了个编辑器却把系统变难用"：若默认是"每次询问"，
    /// 那么所有工具（包括读文件）都会开始弹确认。
    #[test]
    fn default_follows_the_level_gate() {
        let p = Policy::default();
        assert_eq!(p.get("read_file"), ToolPolicy::Default);
        assert_eq!(p.customized(), 0);
        assert!(p.effective("read_file", Level::L0, Level::L2).is_allow());
        assert!(p.effective("read_file", Level::L1, Level::L2).is_allow());
        assert!(matches!(
            p.effective("read_file", Level::L2, Level::L2),
            Effect::Ask(_)
        ));
    }

    /// **显式「每次询问」连 L0 也要问**（用户刻意收紧某个工具）。
    #[test]
    fn explicit_ask_asks_even_for_l0() {
        let mut p = Policy::default();
        p.set("read_file", ToolPolicy::Ask);
        assert!(matches!(
            p.effective("read_file", Level::L0, Level::L2),
            Effect::Ask(_)
        ));
    }

    /// **L3 永远是绝对例外**：即便把闸门阈值开到 L3，「允许」也不能自动放行。
    #[test]
    fn allow_never_auto_approves_l3_even_with_max_threshold() {
        let mut p = Policy::default();
        p.set("system_power", ToolPolicy::Allow);
        assert!(
            matches!(p.effective("system_power", Level::L3, Level::L3), Effect::Ask(_)),
            "阈值开到 L3 也不该放行 L3 工具"
        );
    }

    /// **规则 1：禁止优先**。任何等级、即便闸门阈值很高，也不能绕过用户的禁止。
    #[test]
    fn deny_beats_everything() {
        let mut p = Policy::default();
        p.set("system_power", ToolPolicy::Deny);
        for level in [Level::L0, Level::L1, Level::L2, Level::L3] {
            let e = p.effective("system_power", level, Level::L3);
            assert!(
                matches!(e, Effect::Deny(_)),
                "{level:?} 下禁止策略被绕过了：{e:?}"
            );
        }
    }

    /// **规则 2：允许不能自动放行 L3**（审计原话："L3 永远不可能 always allow"）。
    #[test]
    fn allow_never_auto_approves_l3() {
        let mut p = Policy::default();
        p.set("system_power", ToolPolicy::Allow);
        let e = p.effective("system_power", Level::L3, Level::L2);
        assert!(
            matches!(e, Effect::Ask(_)),
            "L3 被「允许」自动放行了 —— 这是审计明确否掉的行为：{e:?}"
        );
        // 说明原因里要把这件事讲清楚（用户会问"我明明设了允许"）
        if let Effect::Ask(why) = e {
            assert!(
                why.contains("L3") || why.contains("上限"),
                "原因要说明是等级限制：{why}"
            );
        }
    }

    #[test]
    fn allow_auto_approves_within_the_ceiling() {
        let mut p = Policy::default();
        p.set("read_file", ToolPolicy::Allow);
        assert!(p.effective("read_file", Level::L0, Level::L2).is_allow());
        assert!(p.effective("read_file", Level::L2, Level::L2).is_allow());
        // 高于上限则降级为询问
        assert!(matches!(
            p.effective("read_file", Level::L3, Level::L2),
            Effect::Ask(_)
        ));
    }

    /// 策略的作用域**只限那一个工具**（设了 A 不该影响 B）。
    #[test]
    fn policy_is_per_tool() {
        let mut p = Policy::default();
        p.set("clipboard_read", ToolPolicy::Allow);
        // 设了策略的那个：L2 也自动放行（用户显式放宽）
        assert!(p.effective("clipboard_read", Level::L2, Level::L2).is_allow());
        // 没设的那个：仍按等级走 —— L0 放行、**L2 要确认**
        assert!(p.effective("clipboard_write", Level::L0, Level::L2).is_allow());
        assert!(
            matches!(p.effective("clipboard_write", Level::L2, Level::L2), Effect::Ask(_)),
            "策略只该作用于被设置的那个工具"
        );
    }

    #[test]
    fn setting_back_to_default_removes_the_entry() {
        let mut p = Policy::default();
        p.set("t", ToolPolicy::Deny);
        assert_eq!(p.customized(), 1);
        // 设回**默认**（不是"每次询问"）才删条目：显式的"每次询问"是一个**真实的选择**，
        // 它比默认更严，删掉它就等于把用户刻意收紧的东西悄悄放宽了。
        p.set("t", ToolPolicy::Ask);
        assert_eq!(p.customized(), 1, "显式的「每次询问」应当被保留");
        assert_eq!(p.get("t"), ToolPolicy::Ask);
        p.set("t", ToolPolicy::Default);
        assert_eq!(p.customized(), 0, "设回默认才删条目");
        assert_eq!(p.get("t"), ToolPolicy::Default);
    }

    #[test]
    fn round_trips_through_json() {
        let mut p = Policy::default();
        p.set("system_power", ToolPolicy::Deny);
        p.set("read_file", ToolPolicy::Allow);
        let json = p.to_json();
        let back = Policy::parse(&json);
        assert_eq!(back, p);
        assert_eq!(back.get("system_power"), ToolPolicy::Deny);
        assert_eq!(back.get("read_file"), ToolPolicy::Allow);
        // 只写非默认条目
        assert!(!json.contains("\"ask\""), "不该把默认值写进文件：{json}");
    }

    /// **坏输入不能让 aetherd 起不来**：坏 JSON → 全默认（都问），而不是报错或放行。
    #[test]
    fn broken_json_falls_back_to_ask_everything() {
        for bad in [
            "",
            "not json at all",
            "{}",
            "{\"policy\": }",
            "{\"policy\": {\"a\": }}",
            "[1,2,3]",
        ] {
            let p = Policy::parse(bad);
            assert_eq!(p.customized(), 0, "坏输入「{bad}」不该产生条目");
            assert_eq!(p.get("read_file"), ToolPolicy::Default);
            // 关键：坏输入**不能放松任何东西** —— L2 工具照样要确认，
            //（注意不能断言"L0 也要问"：默认是"按等级"，L0 本来就放行。
            //  这里要守的是"坏文件不会让本该确认的操作变成自动执行"。）
            assert!(matches!(
                p.effective("install_disk", Level::L2, Level::L2),
                Effect::Ask(_)
            ));
            assert!(matches!(
                p.effective("install_disk", Level::L3, Level::L2),
                Effect::Ask(_)
            ));
        }
    }

    /// 单条策略值不认识 → **只跳过那一条**，其余照常（这是逐条容错的意义）。
    #[test]
    fn unknown_value_skips_only_that_entry() {
        let json = r#"{"version":1,"policy":{"read_file":"allow","weird":"maybe","system_power":"deny"}}"#;
        let p = Policy::parse(json);
        assert_eq!(p.get("read_file"), ToolPolicy::Allow);
        assert_eq!(p.get("weird"), ToolPolicy::Default, "不认识的值应退回默认");
        assert_eq!(p.get("system_power"), ToolPolicy::Deny, "其余条目不该被牵连");
        assert_eq!(p.customized(), 2);
    }

    /// 工具名里的引号要转义，读回来还得是同一个名字（否则策略会"设了不生效"）。
    #[test]
    fn tool_names_with_quotes_round_trip() {
        let mut p = Policy::default();
        let weird = r#"tool"with\quotes"#;
        p.set(weird, ToolPolicy::Deny);
        let back = Policy::parse(&p.to_json());
        assert_eq!(back.get(weird), ToolPolicy::Deny, "含引号的工具名读回来必须一致");
    }

    #[test]
    fn policy_labels_are_user_facing() {
        assert_eq!(ToolPolicy::Ask.label(), "每次询问");
        assert_eq!(ToolPolicy::Allow.label(), "允许");
        assert_eq!(ToolPolicy::Deny.label(), "禁止");
        // 字面量与解析必须成对（写错一个就会"设了不生效"）
        for p in [ToolPolicy::Ask, ToolPolicy::Allow, ToolPolicy::Deny] {
            assert_eq!(ToolPolicy::parse(p.as_str()), Some(p));
        }
        assert_eq!(ToolPolicy::parse("ALLOW"), None, "大小写敏感，避免歧义");
        assert_eq!(ToolPolicy::Default.label(), "按等级");
    }
    /// 持久化：**写出去、读回来**必须一致（否则"设了不生效"很难查）。
    #[test]
    fn policy_saves_and_loads_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "aether-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("policy.json");

        let mut p = Policy::default();
        p.set("system_power", ToolPolicy::Deny);
        p.set("read_file", ToolPolicy::Ask);
        p.save(&path).expect("保存应成功");

        let (back, warn) = Policy::load(&path);
        assert!(warn.is_none(), "读正常文件不该有告警：{warn:?}");
        assert_eq!(back, p, "写出再读回必须一致");
        assert_eq!(back.get("system_power"), ToolPolicy::Deny);
        assert_eq!(back.get("read_file"), ToolPolicy::Ask);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件不存在 → 全默认，且**不算错误**（首次运行就是这个状态）。
    #[test]
    fn loading_a_missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("aether-policy-definitely-missing-xyz.json");
        let _ = std::fs::remove_file(&path);
        let (p, warn) = Policy::load(&path);
        assert_eq!(p.customized(), 0);
        assert!(warn.is_none(), "文件不存在是正常情况，不该报警：{warn:?}");
    }

    /// 读不动（这里是目录）→ 退回默认**并给出告警**（不能静默）。
    #[test]
    fn unreadable_file_warns_and_falls_back() {
        let dir = std::env::temp_dir().join(format!("aether-policy-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (p, warn) = Policy::load(&dir);
        assert_eq!(p.customized(), 0, "读失败应退回默认");
        assert!(warn.is_some(), "读失败必须有告警，否则用户以为设置生效了");
        let _ = std::fs::remove_dir_all(&dir);
    }

}
