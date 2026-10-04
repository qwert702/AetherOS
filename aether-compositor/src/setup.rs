//! 首次启动向导（FEATURE-GAPS §5.8）：「选语言 / 时区 / 主题 / 连网」。
//!
//! ## 为什么做成一个独立状态机
//!
//! 向导的每一步只是"让用户做一次已有的设置操作" —— 语言、时区、主题、DNS
//! **都已经有各自的实现**（`i18n`/`settings`/`theme`/`set_dns`）。
//! 所以这里**只做两件事**：记住走到第几步、把按键翻译成"要执行哪个已有操作"。
//! 不复制任何设置逻辑 —— 复制出来的那份必然与设置中心里的分叉。
//!
//! ## 按键**按步骤限定**
//!
//! 第 1 步按 `1`/`2` 才是选语言；在第 3 步按 `1` 什么都不该发生。
//! 这不是洁癖：全局按键会让用户在"主题"那一步误按数字而不知道改了什么。
//! 有一条测试专门盯这个（`keys_are_scoped_to_the_current_step`）。
//!
//! ## 界面是键盘驱动的
//!
//! 与命令面板同样的取舍：先把功能打通，鼠标点选留作后续（绘制原语要另做一套）。
//! 每一步都在界面上写清"按哪个键做什么"，所以键盘驱动**不会让人猜**。

// 绘制侧（`draw_setup`）在 Windows 上也会用到本模块的标题/提示；
// 但「按键 → 动作 → 改动设置」这条链在 Linux 的 fbdev 事件循环里，
// 因此非目标平台上这些项目只有测试在用 —— 与 `palette`/`spaces`/`policyview` 同一处理。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// 向导每一步的**按钮**：`(中文标签, English 标签, 按下去要执行的动作)`。
///
/// 键盘与鼠标**共用这张表**：绘制按它画按钮、点击按它取 `Action`。
/// 于是"画了按钮却点了没反应"在结构上不可能 —— 除非表里根本没那一项。
///
/// 为什么现在才有这张表：向导本来是纯键盘驱动的，界面上只有提示文字；
/// 要加鼠标支持就必须先让"可点的东西"真的存在于界面上（本项目零假控件）。
/// 每一步的按钮就把键盘提示（"按 1/2 选语言"）变成了看得见、点得到的东西。
///
/// 未知步骤返回空表（**不 panic**：步骤号来自状态机，但对外接口不该假设它一定合法）。
pub fn options(step: usize) -> Vec<(&'static str, &'static str, Action)> {
    match step {
        // 语言：两个都写"中文 / English"，因为语言名本身就该用它自己的语言写
        0 => vec![
            ("中文", "中文", Action::SetLang(0)),
            ("English", "English", Action::SetLang(1)),
        ],
        // 时区：按小时增减（与键盘 T/Z 同源的动作）
        1 => vec![
            ("时区 −1 小时", "Time −1h", Action::TzShift(-1)),
            ("时区 +1 小时", "Time +1h", Action::TzShift(1)),
        ],
        // 外观：两个开关
        2 => vec![
            ("深色模式", "Dark mode", Action::SetDark),
            ("高对比", "High contrast", Action::SetContrast),
        ],
        // 收尾：一个"开始使用"（等价于回车 = 完成）
        3 => vec![("开始使用", "Get started", Action::Next)],
        _ => Vec::new(),
    }
}

/// 向导步数（语言 / 时区 / 主题 / 连网）。
pub const STEP_COUNT: usize = 4;

/// 默认 DNS（连网那一步用它）。与 `aetherd::netset` 里那份默认值语义一致。
pub const DEFAULT_DNS: &str = "1.1.1.1 8.8.8.8";

/// 时区每档 30 分钟。
pub const TZ_STEP_MIN: i32 = 30;

/// 向导状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Setup {
    pub open: bool,
    /// 当前步骤（0 起）。
    pub step: usize,
    /// 是否已经走过一遍（走完或跳过都算）。
    pub done: bool,
}

/// 是否需要显示向导。
///
/// 判据只有一条：**设置里没记过"已完成"**。不额外看"有没有 DNS""有没有改过语言"——
/// 那些推断会把"用户手动设好了但没走向导"的情况变成再次弹窗。
pub fn needs(settings_done: bool) -> bool {
    !settings_done
}

/// 按键翻译出来的动作。**只描述意图**，执行交给事件循环（那里才知道怎么改设置/发 IPC）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 未识别的键（调用方吞掉 —— 向导是模态的）。
    None,
    /// 前进一步（最后一步 = 完成）。
    Next,
    /// 退回上一步。
    Back,
    /// 跳过全部（Esc）。
    SkipAll,
    /// 选语言（`locale` 下标）。
    SetLang(u8),
    /// 时区平移（分钟，可正可负）。
    TzShift(i32),
    /// 切换深色模式。
    SetDark,
    /// 切换高对比度。
    SetContrast,
    /// 用默认 DNS 值。
    UseDefaultDns,
}

/// 把按键翻译成动作（**按当前步骤限定**）。
pub fn handle_key(step: usize, ch: char) -> Action {
    // 通用键：任何一步都能前进/后退/跳过
    match ch {
        '\n' | '\r' => return Action::Next,
        '\u{1b}' => return Action::SkipAll,
        'b' => return Action::Back,
        _ => {}
    }
    let c = ch.to_ascii_lowercase();
    match step {
        0 => match c {
            '1' => Action::SetLang(0),
            '2' => Action::SetLang(1),
            _ => Action::None,
        },
        1 => match c {
            '-' => Action::TzShift(-TZ_STEP_MIN),
            '=' | '+' => Action::TzShift(TZ_STEP_MIN),
            _ => Action::None,
        },
        2 => match c {
            'd' => Action::SetDark,
            'c' => Action::SetContrast,
            _ => Action::None,
        },
        3 => match c {
            'd' => Action::UseDefaultDns,
            _ => Action::None,
        },
        _ => Action::None,
    }
}

impl Setup {
    /// 首次运行时构造（`done=false` 时自动打开）。
    pub fn initial(settings_done: bool) -> Self {
        Self {
            open: needs(settings_done),
            step: 0,
            done: settings_done,
        }
    }

    /// 前进一步。返回**是否因此结束**（最后一步前进 = 完成）。
    pub fn advance(&mut self) -> bool {
        if self.step + 1 >= STEP_COUNT {
            self.finish();
            true
        } else {
            self.step += 1;
            false
        }
    }

    /// 退回上一步（**到头就停住**，不回绕）。
    pub fn back(&mut self) {
        self.step = self.step.saturating_sub(1);
    }

    /// 走完（或跳过）：标记完成并关闭。**两条路都记"完成"**——
    /// 否则用户点了跳过，下次开机又弹一遍（那是骚扰，不是帮助）。
    pub fn finish(&mut self) {
        self.done = true;
        self.open = false;
        self.step = 0;
    }

    pub fn is_last_step(&self) -> bool {
        self.step + 1 >= STEP_COUNT
    }
}

/// 第 `step` 步的标题（中文, English）。
pub fn title(step: usize) -> (&'static str, &'static str) {
    match step {
        0 => ("选择界面语言", "Choose your language"),
        1 => ("设置时区", "Set the time zone"),
        2 => ("选择外观", "Pick an appearance"),
        3 => ("连接网络", "Connect to the network"),
        _ => ("", ""),
    }
}

/// 第 `step` 步的说明（**必须写清按哪个键做什么** —— 界面是键盘驱动的）。
pub fn hint(step: usize) -> (&'static str, &'static str) {
    match step {
        0 => (
            "按 1 用中文，按 2 用 English。回车继续，Esc 跳过向导。",
            "Press 1 for Chinese, 2 for English. Enter to continue, Esc to skip.",
        ),
        1 => (
            "按 - 或 = 按半小时调整。回车继续。",
            "Press - or = to adjust by half an hour. Enter to continue.",
        ),
        2 => (
            "按 D 切换深色模式，按 C 切换高对比度。回车继续。",
            "Press D for dark mode, C for high contrast. Enter to continue.",
        ),
        3 => (
            "按 D 使用默认 DNS（1.1.1.1 / 8.8.8.8），也可以回车先跳过，稍后在设置里填。",
            "Press D to use the default DNS (1.1.1.1 / 8.8.8.8), or Enter to skip and set it later.",
        ),
        _ => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_shows_exactly_when_settings_say_it_is_not_done() {
        assert!(needs(false), "没走过向导就该显示");
        assert!(!needs(true), "走过（或跳过）之后不该再显示");
        let s = Setup::initial(false);
        assert!(s.open && s.step == 0 && !s.done);
        let s2 = Setup::initial(true);
        assert!(!s2.open && s2.done);
    }

    /// **按键按步骤限定**：第 3 步按 `1` 不该改语言。
    ///
    /// 全局按键的后果是"用户在主题那一步误按数字，却不知道改了什么"。
    #[test]
    fn keys_are_scoped_to_the_current_step() {
        // 语言步：1/2 有效
        assert_eq!(handle_key(0, '1'), Action::SetLang(0));
        assert_eq!(handle_key(0, '2'), Action::SetLang(1));
        // 主题步：数字无效
        assert_eq!(handle_key(2, '1'), Action::None);
        assert_eq!(handle_key(2, '2'), Action::None);
        // 时区步：d 无效（那是主题步的键）
        assert_eq!(handle_key(1, 'd'), Action::None);
        // 连网步：d 是"用默认 DNS"（同键在不同步骤含义不同，且都写在了提示里）
        assert_eq!(handle_key(3, 'd'), Action::UseDefaultDns);
        // 主题步的 d 是深色
        assert_eq!(handle_key(2, 'd'), Action::SetDark);
    }

    #[test]
    fn common_keys_work_on_every_step() {
        for step in 0..STEP_COUNT {
            assert_eq!(handle_key(step, '\n'), Action::Next, "第 {step} 步回车");
            assert_eq!(handle_key(step, '\u{1b}'), Action::SkipAll, "第 {step} 步 Esc");
            assert_eq!(handle_key(step, 'b'), Action::Back, "第 {step} 步 b");
        }
    }

    #[test]
    fn case_insensitive_and_unknown_keys_are_ignored() {
        assert_eq!(handle_key(2, 'D'), Action::SetDark, "大写也认");
        assert_eq!(handle_key(2, 'C'), Action::SetContrast);
        assert_eq!(handle_key(2, 'z'), Action::None);
        assert_eq!(handle_key(9, 'd'), Action::None, "越界步骤：什么都不做");
        assert_eq!(handle_key(2, '中'), Action::None, "非 ASCII 也不 panic");
    }

    #[test]
    fn timezone_keys_shift_by_half_an_hour_in_both_directions() {
        assert_eq!(handle_key(1, '-'), Action::TzShift(-30));
        assert_eq!(handle_key(1, '='), Action::TzShift(30));
        assert_eq!(handle_key(1, '+'), Action::TzShift(30));
    }

    #[test]
    fn advancing_walks_every_step_then_finishes() {
        let mut s = Setup::initial(false);
        for expected in 1..STEP_COUNT {
            assert!(!s.advance(), "第 {expected} 步前进不该结束");
            assert_eq!(s.step, expected);
            assert!(s.open, "没走完就必须还开着");
        }
        assert!(s.is_last_step());
        assert!(s.advance(), "最后一步前进应当结束");
        assert!(s.done, "结束后要记「已完成」");
        assert!(!s.open);
        assert_eq!(s.step, 0, "结束后步骤复位");
    }

    #[test]
    fn back_stops_at_the_first_step() {
        let mut s = Setup::initial(false);
        s.advance();
        s.advance();
        assert_eq!(s.step, 2);
        s.back();
        assert_eq!(s.step, 1);
        s.back();
        s.back();
        assert_eq!(s.step, 0, "回到第一步后停住，不回绕到末尾");
        assert!(s.open);
    }

    /// **跳过也要记「已完成」**：否则下次开机又弹一遍 —— 那是骚扰，不是帮助。
    #[test]
    fn skipping_also_marks_it_done() {
        let mut s = Setup::initial(false);
        s.advance();
        s.finish();
        assert!(s.done && !s.open);
        assert!(!needs(s.done), "跳过之后不该再显示");
    }

    #[test]
    fn every_step_has_bilingual_title_and_hint() {
        for step in 0..STEP_COUNT {
            let (zh, en) = title(step);
            assert!(!zh.is_empty() && !en.is_empty(), "第 {step} 步缺标题");
            assert_ne!(zh, en);
            let (hz, he) = hint(step);
            assert!(!hz.is_empty() && !he.is_empty(), "第 {step} 步缺说明");
            // 提示里必须写清按哪个键（界面是键盘驱动的，不写就等于让人猜）
            assert!(
                hz.contains('按') || hz.contains("回车"),
                "第 {step} 步的中文提示没写按哪个键：{hz}"
            );
        }
    }
    /// 每一步都要有按钮，且**每个动作都不是 `Action::None`**。
    ///
    /// 后半句是重点：`Action::None` 是"这个按键/按钮什么都不做"的表示，
    /// 它在按钮表里出现就等于**画了一个按下去没反应的按钮**。
    #[test]
    fn every_step_has_buttons_with_real_actions() {
        for step in 0..STEP_COUNT {
            let opts = options(step);
            assert!(!opts.is_empty(), "第 {} 步没有任何按钮", step + 1);
            for (zh, en, act) in &opts {
                assert!(!zh.is_empty() && !en.is_empty(), "按钮标签不能为空：{zh}/{en}");
                assert_ne!(
                    *act,
                    Action::None,
                    "第 {} 步的按钮「{zh}」没有动作 —— 那是个假按钮",
                    step + 1
                );
            }
        }
        // 语言步：两个选项，且分别对应两个语言（与 handle_key 的 1/2 一致）
        let lang = options(0);
        assert_eq!(lang.len(), 2);
        assert_eq!(lang[0].2, Action::SetLang(0));
        assert_eq!(lang[1].2, Action::SetLang(1));
        // 未知步骤给空表，不 panic
        assert!(options(99).is_empty());
        assert!(options(usize::MAX).is_empty());
    }

}
