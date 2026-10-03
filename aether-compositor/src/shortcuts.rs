//! 快捷键总览（FEATURE-GAPS §5.9）。
//!
//! 需求写的是"设置里一页列全，且**逐条真实生效**"。后半句决定了设计：
//! 光有一张表没有意义 —— 界面上的快捷键说明最容易变成**过期的谎言**
//! （改了实现忘了改文档，而用户照着按没反应，只能自己怀疑人生）。
//!
//! 所以每条快捷键都带一个 **`probe`**：它必须能在源码里被搜到（测试会扫）。
//! 于是新增/修改快捷键时，**要么同步更新这张表、要么测试变红**，
//! 不存在"总览页说了但代码里没有"的状态。
//!
//! 为什么用探针而不是"让测试真的按一遍键"：按键要走完整的 evdev → UI 事件 →
//! 事件循环三段，而第三段在 `#[cfg(target_os = "linux")]` 的 fbdev 循环里
//! （开发机跑不到）。探针是**能在任意平台执行**的等价证据 —— 它证明"落点还在"，
//! 不证明"行为正确"，这条边界写在下面，不含糊。

use crate::i18n::{pick, Lang};

/// 一条快捷键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// 键位（给人看的写法，如 `Ctrl+Z`）。
    pub keys: &'static str,
    /// 中文说明。
    pub desc: &'static str,
    /// English description.
    pub desc_en: &'static str,
    /// **真实生效的依据**：处理这条快捷键的源码里必须出现的片段。
    pub probe: &'static str,
    /// 探针位于 `input.rs`（键位翻译）而不是 `main.rs`（事件循环）。
    pub in_input: bool,
}

impl Shortcut {
    pub fn desc_in(&self, lang: Lang) -> &'static str {
        pick(lang, self.desc, self.desc_en)
    }
}

/// 全部快捷键。**只列真实存在的** —— 测试会逐条核对探针。
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        keys: "Alt+1…4",
        desc: "切换窗口布局（四档）",
        desc_en: "Switch window layout (4 presets)",
        probe: "input::UiEvent::LayoutKey(n) => {",
        in_input: false,
    },
    Shortcut {
        keys: "Super+L",
        desc: "锁定屏幕（解锁需重新验证口令）",
        desc_en: "Lock the screen (unlocking requires the password)",
        probe: "input::UiEvent::Super('l') => {",
        in_input: false,
    },
    Shortcut {
        keys: "Ctrl+Z",
        desc: "撤销上一步 AI 的桌面改动",
        desc_en: "Undo the last desktop change made by AI",
        probe: "if c == 'z' && !shift {",
        in_input: false,
    },
    Shortcut {
        keys: "Ctrl+W",
        desc: "关闭当前窗口",
        desc_en: "Close the active window",
        probe: "let closed = !shift && c == 'w';",
        in_input: false,
    },
    Shortcut {
        keys: "Ctrl+Space",
        desc: "切换中文输入法",
        desc_en: "Toggle the Chinese input method",
        probe: r"if c == '\0' {",
        in_input: false,
    },
    Shortcut {
        keys: "Esc",
        desc: "关闭浮层（控制中心）；输入法拼字中先取消拼字",
        desc_en: "Close overlays; cancels IME composition first",
        probe: "input::UiEvent::Escape => {",
        in_input: false,
    },
    Shortcut {
        keys: "方向键 / Tab / Delete",
        desc: "窗口与文件列表导航",
        desc_en: "Navigate windows and the file list",
        probe: "input::UiEvent::Nav(n) => {",
        in_input: false,
    },
    Shortcut {
        keys: "Enter",
        desc: "在权限确认卡片里放行一次（L3 需原样回显）",
        desc_en: "Approve once in the permission card (L3 needs an exact echo)",
        probe: "req.echo_ok_input(echo)",
        in_input: false,
    },
    Shortcut {
        keys: "Alt+1…4（键位翻译）",
        desc: "布局快捷键在输入层从裸数字改为 Alt 组合（2026-09-29 修复）",
        desc_en: "Layout keys moved from bare digits to Alt combos (fixed 2026-09-29)",
        probe: "mods.alt && !mods.shift && !mods.ctrl =>",
        in_input: true,
    },
    Shortcut {
        keys: "Super（元键）",
        desc: "输入层跟踪 Meta 键 —— 没有它，Super 组合键不可能实现",
        desc_en: "The input layer tracks Meta; without it Super combos are impossible",
        probe: "KEY_LEFTMETA | KEY_RIGHTMETA => {",
        in_input: true,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// §5.9 的核心断言：**总览页不许撒谎**。
    ///
    /// 每条快捷键的探针都必须在真实源码里找到。改了实现（例如把 Ctrl+Z 挪走）
    /// 而没改这张表，这条测试就会变红 —— 这正是它存在的意义。
    #[test]
    fn every_listed_shortcut_is_actually_wired_in_the_source() {
        let main_src = include_str!("main.rs");
        let input_src = include_str!("input.rs");
        for s in SHORTCUTS {
            let hay = if s.in_input { input_src } else { main_src };
            assert!(
                hay.contains(s.probe),
                "快捷键 {}（{}）声称的落点「{}」在{}里找不到 —— 总览页会撒谎",
                s.keys,
                s.desc,
                s.probe,
                if s.in_input { "input.rs" } else { "main.rs" }
            );
        }
    }

    /// 两种语言都要有说明（英文用户看到空白等于没做）。
    #[test]
    fn every_shortcut_is_described_in_both_languages() {
        for s in SHORTCUTS {
            assert!(!s.keys.trim().is_empty(), "键位不能为空");
            assert!(!s.desc.trim().is_empty(), "{} 缺中文说明", s.keys);
            assert!(!s.desc_en.trim().is_empty(), "{} 缺英文说明", s.keys);
            assert_ne!(s.desc, s.desc_en, "{} 的两种说明不该一模一样", s.keys);
            assert_eq!(s.desc_in(Lang::Zh), s.desc);
            assert_eq!(s.desc_in(Lang::En), s.desc_en);
        }
    }

    /// 键位不能重复（同一键位列两条会让人以为有两个功能，或其中一条是错的）。
    #[test]
    fn shortcut_keys_are_unique() {
        for (i, a) in SHORTCUTS.iter().enumerate() {
            for b in &SHORTCUTS[i + 1..] {
                assert_ne!(a.keys, b.keys, "键位重复：{}", a.keys);
            }
        }
        assert!(SHORTCUTS.len() >= 6, "总览页太短就失去意义");
    }

    /// **反例测试**：探针机制本身得有效 —— 编造的探针必须找不到。
    ///
    /// 没有这条，上面那些断言可能只是"恰好都过"（比如探针写成空串就永远成立，
    /// 所以顺带断言探针不能太短）。
    #[test]
    fn the_probe_mechanism_itself_rejects_fabricated_entries() {
        let main_src = include_str!("main.rs");
        let fabricated = "input::UiEvent::CtrlAltDel('x') => {";
        assert!(
            !main_src.contains(fabricated),
            "反例探针竟然存在？说明探针写法有问题"
        );
        for s in SHORTCUTS {
            assert!(
                s.probe.len() >= 12,
                "{} 的探针太短（{}），容易误命中：{}",
                s.keys,
                s.probe.len(),
                s.probe
            );
        }
    }
}
