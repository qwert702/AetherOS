//! 输入事件层：**跨平台**的事件类型与键位翻译（可在开发机上编译与单测），
//! 加上 Linux 专属的 evdev 采集后端。
//!
//! 之所以拆开：此前整个模块都被 `cfg(target_os = "linux")` 挡在门外，
//! 结果"按键怎么翻译"这段纯逻辑在 Windows 上既不能编译也不能测 —— 而它正是
//! 修饰键、方向键、终端输入的地基。**纯逻辑必须跨平台**，只有设备 I/O 才限定平台。
//!
//! 采集后端：读取 /dev/input/event*，把键盘/鼠标原始事件转为 UI 事件。
//! QEMU 默认是 i8042 PS/2 键盘/鼠标，VBox vboxvga 也走 PS/2 辅助鼠标，
//! 因此只需 EV_KEY / EV_REL；EV_ABS（usb tablet）暂不解析。
//! 后台线程以 O_NONBLOCK 轮询全部设备，主循环每帧 drain 通道，
//! 采集速率与渲染帧率解耦（鼠标位移在通道里累积，不丢步）。

// 非 Linux 目标只用事件类型与键位翻译，采集相关常量/函数不会被引用
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::sync::mpsc::Sender;
#[cfg(target_os = "linux")]
use std::time::Duration;

/// 修饰键状态。
///
/// evdev 只上报"某个物理键按下/抬起"，组合键必须由上层自己算 —— 此前完全没有
/// 这层状态，所以 Ctrl+C / Ctrl+W / Shift+选择这类交互**在任何位置都不可能实现**。
/// 这是终端输入、剪贴板、键位映射三处共同的前置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Super/Win/Command 键（evdev 的 Meta）。锁屏快捷键 `Super+L` 需要它 ——
    /// 在此之前这层状态根本不存在，所以那类快捷键**不可能实现**（不是没写，是没这个信息）。
    pub meta: bool,
}

/// 导航/功能键（不产生字符的那些）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKey {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Delete,
}

/// 渲染主循环消费的 UI 事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    /// 可打印字符（英文/符号直接进 AI 指令条，与预览版一致）
    Char(char),
    /// Super（Win/Cmd）+ 键，已转小写；第二位是**是否按住 Shift**。
    ///
    /// 为什么要带 Shift：虚拟桌面（§5.4）用 `Super+1..4` 切空间、
    /// `Super+Shift+1..4` 把当前窗口**搬到**那个空间 —— 没有这一位就没法区分，
    /// 而"搬窗口"和"切空间"是两件完全不同的事（做错了会以为窗口丢了）。
    Super(char, bool),
    Enter,
    Backspace,
    Escape,
    /// Ctrl + 字母（Ctrl+C/Ctrl+W…），第二项是**是否同时按住 Shift**。
    ///
    /// 独立于 `Char` 是因为组合键与字符是两类语义（混在一起会让"按住 Ctrl 时字符被吞掉"
    /// 变成隐式行为）。带 Shift 是必需的：终端里 `Ctrl+C` 必须归 shell（中断命令），
    /// 复制只能走 `Ctrl+Shift+C` —— 不区分 shift 的话这两件事会抢同一个按键。
    Ctrl(char, bool),
    /// 导航键（方向/翻页/Home/End/Tab/Delete）
    Nav(NavKey),
    /// 数字键 1–4：布局快捷键
    LayoutKey(usize),
    /// 鼠标相对位移（evdev REL 事件，按下时主循环据此累计坐标）
    MouseMove { dx: i32, dy: i32 },
    MouseDown,
    MouseUp,
    /// 滚轮（evdev REL_WHEEL）：**正数向上、负数向下**（linux/input.h 约定）。
    ///
    /// 2026-09-29 P1 新增：此前**完全没有滚轮映射**，任何超出可视区的列表都只能靠
    /// 翻页键。设置中心那种长面板必须有滚轮，否则只能一页页翻。
    Wheel { dy: i32 },
}

/// linux/input.h 的 input_event（x86_64 布局：timeval 16B + type/code/value 8B = 24B）
#[derive(Clone, Copy)]
struct RawEvent {
    type_: u16,
    code: u16,
    value: i32,
}

const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
/// 纵向滚轮（一格 = value 1；正值向上）
const REL_WHEEL: u16 = 0x08;
const BTN_LEFT: u16 = 0x110;
const KEY_ESC: u16 = 1;
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_ENTER: u16 = 28;
/// KEY_1 … KEY_0 = 2 … 11
const KEY_1: u16 = 2;
const KEY_0: u16 = 11;
// 修饰键（linux/input.h）
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTALT: u16 = 56;
/// Super/Win 键（evdev 里叫 Meta）。
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;
const KEY_RIGHTSHIFT: u16 = 54;
// 导航键
const KEY_HOME: u16 = 102;
const KEY_UP: u16 = 103;
const KEY_PAGEUP: u16 = 104;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_END: u16 = 107;
const KEY_DOWN: u16 = 108;
const KEY_PAGEDOWN: u16 = 109;
const KEY_DELETE: u16 = 111;

/// 启动输入采集线程：持续打开 /dev/input/event* 并把事件送上通道。
/// `Super+键` 的意图。**纯数据**，由 [`super_intent`] 产出、由主循环执行。
///
/// 把它独立出来的理由见批次 AQ-1：这个分支历史上出过三次问题
///（守卫顺序、无条件调用、写死常量），而那三次的共同点都是"判断写在执行里"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperIntent {
    /// `Super+L`：锁屏（**最高优先级** —— 见 `super_intent` 的顺序说明）
    Lock,
    /// `Super+数字`：切到第 N 个桌面（0 基）
    SwitchSpace(usize),
    /// `Super+Shift+数字`：把当前窗口搬到第 N 个桌面（0 基）
    MoveToSpace(usize),
    /// 其余 Super 组合键：**明确忽略**（输入层已保证它们不会漏成普通字符）
    Ignore,
}

/// 判断一个 `Super+键` 想做什么。
///
/// `space_count` 是**当前**桌面数（默认 4、可增删到 8）—— 由调用方传入，
/// 而不是在函数里读常量：这正是附录 P 那个 bug 的修法
///（当时的键位判断写死了默认值 4，于是 `Super+5..8` 静默无效）。
///
/// 顺序（**这是本函数存在的意义**）：
/// 1. `Super+L` 且不带 Shift → 锁屏。**必须在数字判断之前**，
///    否则将来的某个改动可能让别的组合键抢走它；
/// 2. `Super+数字` 且 1..=`space_count` → 切换/搬运；
/// 3. 其余一律 `Ignore` —— **不是"忘了处理"，而是明确的兜底**：
///    附录 O 那个 bug 就是"在这个位置之前塞了一句无条件调用"造成的。
pub fn super_intent(c: char, shift: bool, space_count: usize) -> SuperIntent {
    if c == 'l' && !shift {
        return SuperIntent::Lock;
    }
    if let Some(d) = c.to_digit(10) {
        // `to_digit(10)` 给 0..9；0 不是"第 0 个桌面"，忽略
        if d >= 1 && (d as usize) <= space_count {
            let idx = d as usize - 1;
            return if shift {
                SuperIntent::MoveToSpace(idx)
            } else {
                SuperIntent::SwitchSpace(idx)
            };
        }
    }
    SuperIntent::Ignore
}

/// `Enter` 键的意图。**纯数据**，由 [`enter_intent`] 产出、由主循环执行。
///
/// 回车在界面上是"最重"的那个键，它的语义取决于**当时开着什么**：
/// 确认卡片（权限）、安装向导、AI 指令条。三者的优先级写在这里、由测试守着。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterIntent {
    /// 确认卡片：**放行一次**（L3 需要回显匹配，见 `echo_ok`）
    Approve,
    /// 确认卡片：回显不匹配 → 提示用户原样输入（**不放行**）
    EchoMismatch,
    /// 安装向导：开始安装
    StartInstall,
    /// AI 指令条：把输入发给 AI
    SendAi,
    /// 什么都不做（指令条是空的，也没有别的浮层要处理）
    Nothing,
}

/// 判断一次回车想做什么。
///
/// 优先级（**这是本函数存在的意义**，也是它唯一需要测试的东西）：
/// 1. **确认卡片最高** —— 它是"权限"这一步，绝不能因为后面还开着向导或
///    指令条里有字就被跳过；卡片开着时，回车永远只与卡片有关
///    （放行或提示回显不匹配，**不会**顺手把指令条里的字发出去）；
/// 2. 其次是安装向导 —— 它是一次不可逆操作的入口，回车即"开始"；
/// 3. 最后才是指令条 —— 且必须**有非空内容**（空回车不该发出一个空查询）。
pub fn enter_intent(
    has_confirm: bool,
    echo_ok: bool,
    installer_open: bool,
    ai_empty: bool,
) -> EnterIntent {
    if has_confirm {
        return if echo_ok {
            EnterIntent::Approve
        } else {
            EnterIntent::EchoMismatch
        };
    }
    if installer_open {
        return EnterIntent::StartInstall;
    }
    if !ai_empty {
        return EnterIntent::SendAi;
    }
    EnterIntent::Nothing
}

#[cfg(target_os = "linux")]
pub fn spawn(tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let mut devs: Vec<File> = Vec::new();
        let mut rel_seen: u64 = 0; // 遥测：收到的 REL 事件数
        // 修饰键状态跨事件累积，必须活在循环外
        let mut mods = Mods::default();
        loop {
            if devs.is_empty() {
                devs = open_devices();
                if devs.is_empty() {
                    eprintln!("aether-compositor: 尚无 /dev/input/event*，2s 后重试");
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
                eprintln!(
                    "aether-compositor: evdev 输入已接入（{} 个设备）",
                    devs.len()
                );
            }
            let mut traffic = false;
            for d in &mut devs {
                let mut buf = [0u8; 24 * 32];
                match d.read(&mut buf) {
                    Ok(n) if n >= 24 => {
                        traffic = true;
                        for chunk in buf[..n / 24 * 24].chunks_exact(24) {
                            let raw = RawEvent {
                                type_: u16::from_le_bytes([chunk[16], chunk[17]]),
                                code: u16::from_le_bytes([chunk[18], chunk[19]]),
                                value: i32::from_le_bytes([
                                    chunk[20], chunk[21], chunk[22], chunk[23],
                                ]),
                            };
                            // 遥测：串口能看出"事件到底有没有进来"（2026-10-17 鼠标不动排查）
                            if raw.type_ == EV_REL {
                                rel_seen += 1;
                                if rel_seen <= 3 || rel_seen % 200 == 0 {
                                    eprintln!(
                                        "[input] REL code={} value={}（累计 {rel_seen} 次）",
                                        raw.code, raw.value
                                    );
                                }
                            }
                            for ui in translate(&raw, &mut mods) {
                                let _ = tx.send(ui);
                            }
                        }
                    }
                    // EAGAIN（无数据）与其他读错误（设备消失）都静默跳过
                    _ => {}
                }
            }
            if !traffic {
                std::thread::sleep(Duration::from_millis(8));
            }
        }
    });
}

#[cfg(target_os = "linux")]
fn open_devices() -> Vec<File> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/dev/input") else {
        return out;
    };
    let mut paths: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains("event"))
        .collect();
    paths.sort();
    for p in paths {
        if let Ok(f) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&p)
        {
            out.push(f);
        }
    }
    out
}

fn translate(raw: &RawEvent, mods: &mut Mods) -> Vec<UiEvent> {
    match raw.type_ {
        EV_KEY => {
            // 鼠标按键的按下/松开都要上报；键盘按键只取按下/重复
            if raw.code == BTN_LEFT {
                return vec![if raw.value == 1 { UiEvent::MouseDown } else { UiEvent::MouseUp }];
            }
            // 修饰键：只更新状态，本身不产生 UI 事件（value==2 是自动重复，忽略）
            if raw.value != 2 {
                let down = raw.value == 1;
                let touched = match raw.code {
                    KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                        mods.ctrl = down;
                        true
                    }
                    KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                        mods.shift = down;
                        true
                    }
                    KEY_LEFTMETA | KEY_RIGHTMETA => {
                        mods.meta = down;
                        return vec![];
                    }
                    KEY_LEFTALT | KEY_RIGHTALT => {
                        mods.alt = down;
                        true
                    }
                    _ => false,
                };
                if touched {
                    return vec![];
                }
            }
            if raw.value == 0 {
                return vec![];
            }
            // 导航键：与字符无关，先判掉
            if let Some(nav) = nav_key(raw.code) {
                return vec![UiEvent::Nav(nav)];
            }
            match raw.code {
                KEY_ENTER => return vec![UiEvent::Enter],
                KEY_BACKSPACE => return vec![UiEvent::Backspace],
                KEY_ESC => return vec![UiEvent::Escape],
                // 布局快捷键 = **Alt + 1..4**。
                // 2026-09-29 实测修复：原来用**裸数字 1–4**，于是这几个数字在**任何输入框里都
                // 打不出来** —— 终端里连 `wget http://10.0.2.2/...` 都敲不了（`2` 被当成
                // "切两列布局"直接吞掉），AI 输入框同理。全局吞键绝不能占用裸字母数字。
                KEY_1..=KEY_0 if mods.alt && !mods.shift && !mods.ctrl => {
                    if raw.code <= KEY_1 + 3 {
                        return vec![UiEvent::LayoutKey((raw.code - KEY_1 + 1) as usize)];
                    }
                }
                _ => {}
            }
            let Some(base) = key_char(raw.code) else {
                return vec![];
            };
            if mods.meta {
                // Super+字母/数字 → 组合键（与 Ctrl 同样只收这两种，避免漏出散字）。
                // 数字是虚拟桌面的需要（`Super+1..4`）；只认字母的话那几个键收不到。
                return if base.is_ascii_alphanumeric() {
                    vec![UiEvent::Super(base.to_ascii_lowercase(), mods.shift)]
                } else {
                    vec![]
                };
            }
            if mods.ctrl {
                // Ctrl+字母 → 组合键；Ctrl+其他不产生字符（否则会漏出 ';' 之类的散字）
                return if base.is_ascii_alphabetic() {
                    vec![UiEvent::Ctrl(base.to_ascii_lowercase(), mods.shift)]
                } else {
                    vec![]
                };
            }
            if mods.alt {
                return vec![]; // Alt 组合暂不映射，但**不能退化成普通字符**
            }
            let c = if mods.shift { shift_of(base) } else { base };
            vec![UiEvent::Char(c)]
        }
        EV_REL => match raw.code {
            REL_X => vec![UiEvent::MouseMove { dx: raw.value, dy: 0 }],
            REL_Y => vec![UiEvent::MouseMove { dx: 0, dy: raw.value }],
            REL_WHEEL => vec![UiEvent::Wheel { dy: raw.value }],
            _ => vec![],
        },
        _ => vec![],
    }
}

fn nav_key(code: u16) -> Option<NavKey> {
    Some(match code {
        KEY_LEFT => NavKey::Left,
        KEY_RIGHT => NavKey::Right,
        KEY_UP => NavKey::Up,
        KEY_DOWN => NavKey::Down,
        KEY_HOME => NavKey::Home,
        KEY_END => NavKey::End,
        KEY_PAGEUP => NavKey::PageUp,
        KEY_PAGEDOWN => NavKey::PageDown,
        KEY_TAB => NavKey::Tab,
        KEY_DELETE => NavKey::Delete,
        _ => return None,
    })
}

/// Shift 上档字符。字母转大写，其余查表（表外的按原样返回，例如空格）。
fn shift_of(c: char) -> char {
    match c {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        ';' => ':',
        '\'' => '"',
        '\\' => '|',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        c => c.to_ascii_uppercase(),
    }
}

/// 键位 → **未按 Shift 的**字符（US 布局）。
/// Shift 的效果统一由 [`shift_of`] 施加，避免两处各写一套大小写规则。
fn key_char(code: u16) -> Option<char> {
    Some(match code {
        2 => '1',
        3 => '2',
        4 => '3',
        5 => '4',
        6 => '5',
        7 => '6',
        8 => '7',
        9 => '8',
        10 => '9',
        11 => '0',
        16 => 'q',
        17 => 'w',
        18 => 'e',
        19 => 'r',
        20 => 't',
        21 => 'y',
        22 => 'u',
        23 => 'i',
        24 => 'o',
        25 => 'p',
        26 => '[',
        27 => ']',
        30 => 'a',
        31 => 's',
        32 => 'd',
        33 => 'f',
        34 => 'g',
        35 => 'h',
        36 => 'j',
        37 => 'k',
        38 => 'l',
        39 => ';',
        40 => '\'',
        43 => '\\',
        44 => 'z',
        45 => 'x',
        46 => 'c',
        47 => 'v',
        48 => 'b',
        49 => 'n',
        50 => 'm',
        51 => ',',
        52 => '.',
        53 => '/',
        12 => '-',
        13 => '=',
        57 => ' ',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(type_: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { type_, code, value }
    }

    /// 用默认（无修饰键）状态翻译单个事件
    fn t(code: u16, value: i32) -> Vec<UiEvent> {
        let mut m = Mods::default();
        translate(&ev(EV_KEY, code, value), &mut m)
    }

    /// 指定修饰键状态翻译单个事件
    fn tm(code: u16, value: i32, mods: Mods) -> Vec<UiEvent> {
        let mut m = mods;
        translate(&ev(EV_KEY, code, value), &mut m)
    }

    #[test]
    fn enter_and_backspace() {
        assert!(matches!(t(KEY_ENTER, 1)[0], UiEvent::Enter));
        assert!(matches!(t(KEY_BACKSPACE, 1)[0], UiEvent::Backspace));
        assert!(t(KEY_ENTER, 0).is_empty()); // 松开忽略
    }

    #[test]
    fn letters_and_symbols() {
        assert_eq!(t(30, 1), vec![UiEvent::Char('a')]);
        assert_eq!(t(36, 1), vec![UiEvent::Char('j')]);
        assert_eq!(t(57, 1), vec![UiEvent::Char(' ')]);
    }

    #[test]
    fn plain_digits_type_characters() {
        // 2026-09-29 回归：裸数字曾经被布局快捷键吞掉，1–4 在任何输入框里都打不出来。
        // 证据：终端里 `wget http://10.0.2.2/i` 被敲成 `10.0.../i`（两个 `2` 都丢了）。
        assert_eq!(t(2, 1), vec![UiEvent::Char('1')]);
        assert_eq!(t(3, 1), vec![UiEvent::Char('2')]);
        assert_eq!(t(4, 1), vec![UiEvent::Char('3')]);
        assert_eq!(t(5, 1), vec![UiEvent::Char('4')]);
        assert_eq!(t(6, 1), vec![UiEvent::Char('5')]);
        assert_eq!(t(11, 1), vec![UiEvent::Char('0')]);
        // 一条真实命令要能敲出来（曾经 `2` 打不出来）
        for (code, ch) in [(3u16, '2'), (4, '3'), (5, '4'), (2, '1')] {
            assert_eq!(t(code, 1), vec![UiEvent::Char(ch)], "键码 {code} 应打出 {ch}");
        }
    }

    #[test]
    fn shift_digits_still_produce_symbols() {
        let sh = Mods { shift: true, ..Default::default() };
        assert_eq!(tm(3, 1, sh.clone()), vec![UiEvent::Char('@')]);
        assert_eq!(tm(2, 1, sh), vec![UiEvent::Char('!')]);
    }

    #[test]
    fn alt_digits_switch_layout() {
        let alt = Mods { alt: true, ..Default::default() };
        assert!(matches!(tm(2, 1, alt.clone())[0], UiEvent::LayoutKey(1)));
        assert!(matches!(tm(5, 1, alt.clone())[0], UiEvent::LayoutKey(4)));
        // Alt+5 不是布局键，也不能漏成普通字符（否则 Alt 组合会往终端灌字）
        assert!(tm(6, 1, alt).is_empty(), "Alt+5 不该产生字符");
    }

    #[test]
    fn mouse_rel_and_button() {
        assert_eq!(
            translate(&ev(EV_REL, REL_X, 7), &mut Mods::default()),
            vec![UiEvent::MouseMove { dx: 7, dy: 0 }]
        );
        assert_eq!(
            translate(&ev(EV_REL, REL_Y, -3), &mut Mods::default()),
            vec![UiEvent::MouseMove { dx: 0, dy: -3 }]
        );
        assert!(matches!(t(BTN_LEFT, 1)[0], UiEvent::MouseDown));
        assert!(matches!(t(BTN_LEFT, 0)[0], UiEvent::MouseUp));
    }

    #[test]
    fn wheel_is_translated_with_sign() {
        // P1：滚轮此前**完全没有映射**，长列表/设置面板只能翻页
        assert_eq!(
            translate(&ev(EV_REL, REL_WHEEL, 1), &mut Mods::default()),
            vec![UiEvent::Wheel { dy: 1 }]
        );
        assert_eq!(
            translate(&ev(EV_REL, REL_WHEEL, -1), &mut Mods::default()),
            vec![UiEvent::Wheel { dy: -1 }]
        );
        assert_eq!(
            translate(&ev(EV_REL, REL_WHEEL, 3), &mut Mods::default()),
            vec![UiEvent::Wheel { dy: 3 }]
        );
    }

    #[test]
    fn ctrl_produces_combo_not_char() {
        let mut m = Mods::default();
        // 修饰键本身不产生事件，只改状态
        assert!(translate(&ev(EV_KEY, KEY_LEFTCTRL, 1), &mut m).is_empty());
        assert!(m.ctrl);
        // 按住 Ctrl 时字母变成组合键，而不是普通字符
        assert_eq!(
            translate(&ev(EV_KEY, 46, 1), &mut m),
            vec![UiEvent::Ctrl('c', false)] // KEY_C
        );
        // 松开后恢复为普通字符
        assert!(translate(&ev(EV_KEY, KEY_LEFTCTRL, 0), &mut m).is_empty());
        assert!(!m.ctrl);
        assert_eq!(translate(&ev(EV_KEY, 46, 1), &mut m), vec![UiEvent::Char('c')]);
    }

    #[test]
    fn ctrl_shift_is_reported_separately() {
        // Ctrl+Shift+C 与 Ctrl+C 必须是**不同的**事件：前者是"复制"，后者是"中断"
        let mut m = Mods { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(
            translate(&ev(EV_KEY, 46, 1), &mut m),
            vec![UiEvent::Ctrl('c', true)]
        );
    }

    #[test]
    fn ctrl_with_symbol_is_swallowed() {
        // Ctrl+; 不应漏出 ';'（否则按住 Ctrl 打字会出现散字）
        let mut m = Mods { ctrl: true, ..Default::default() };
        assert!(translate(&ev(EV_KEY, 39, 1), &mut m).is_empty());
    }

    #[test]
    fn shift_uppercases_and_shifts_digits() {
        let mut m = Mods::default();
        translate(&ev(EV_KEY, KEY_LEFTSHIFT, 1), &mut m);
        assert_eq!(translate(&ev(EV_KEY, 30, 1), &mut m), vec![UiEvent::Char('A')]);
        // 按住 Shift 时数字 1–4 让位给上档符号，而不是触发布局切换
        assert_eq!(translate(&ev(EV_KEY, 2, 1), &mut m), vec![UiEvent::Char('!')]);
        assert_eq!(translate(&ev(EV_KEY, 3, 1), &mut m), vec![UiEvent::Char('@')]);
    }

    #[test]
    fn shift_maps_punctuation() {
        assert_eq!(shift_of('-'), '_');
        assert_eq!(shift_of('='), '+');
        assert_eq!(shift_of('['), '{');
        assert_eq!(shift_of('/'), '?');
        assert_eq!(shift_of(' '), ' '); // 表外原样返回
    }

    #[test]
    fn navigation_keys_are_reported() {
        assert_eq!(t(KEY_UP, 1), vec![UiEvent::Nav(NavKey::Up)]);
        assert_eq!(t(KEY_DOWN, 1), vec![UiEvent::Nav(NavKey::Down)]);
        assert_eq!(t(KEY_LEFT, 1), vec![UiEvent::Nav(NavKey::Left)]);
        assert_eq!(t(KEY_RIGHT, 1), vec![UiEvent::Nav(NavKey::Right)]);
        assert_eq!(t(KEY_PAGEUP, 1), vec![UiEvent::Nav(NavKey::PageUp)]);
        assert_eq!(t(KEY_PAGEDOWN, 1), vec![UiEvent::Nav(NavKey::PageDown)]);
        assert_eq!(t(KEY_HOME, 1), vec![UiEvent::Nav(NavKey::Home)]);
        assert_eq!(t(KEY_END, 1), vec![UiEvent::Nav(NavKey::End)]);
        assert_eq!(t(KEY_TAB, 1), vec![UiEvent::Nav(NavKey::Tab)]);
        assert_eq!(t(KEY_DELETE, 1), vec![UiEvent::Nav(NavKey::Delete)]);
    }

    #[test]
    fn modifier_key_release_is_not_a_char() {
        // 松开修饰键不能退化成字符（曾是把 key_char 当兜底会踩的坑）
        assert!(t(KEY_LEFTSHIFT, 0).is_empty());
        assert!(t(KEY_LEFTCTRL, 0).is_empty());
    }

    #[test]
    fn alt_combo_does_not_leak_char() {
        let mut m = Mods::default();
        translate(&ev(EV_KEY, KEY_LEFTALT, 1), &mut m);
        assert!(translate(&ev(EV_KEY, 30, 1), &mut m).is_empty());
    }
    /// evdev 的 L 键码。模块顶层只定义用得到的几个键 —— 这里自带一个，
    /// 避免为了测试在生产代码里加一个恒不被使用的常量（那会触发死代码警告）。
    const KEY_L: u16 = 38;

    /// Super+L 的前置：Meta 状态必须被跟踪，且 Super+字母产出 `Super` 事件。
    ///
    /// 在这条之前，输入层**完全不认识 Meta 键** —— 于是"锁屏快捷键"这类需求
    /// 在实现层面就是不可能的（不是没写，是没有这个信息）。
    #[test]
    fn super_key_is_tracked_and_produces_events() {
        let mut m = Mods::default();
        assert!(translate(&ev(EV_KEY, KEY_LEFTMETA, 1), &mut m).is_empty());
        assert!(m.meta, "Meta 状态应被记住");
        // Super+L → Super('l')，且**不能**退化成一个 'l' 字符
        assert_eq!(translate(&ev(EV_KEY, KEY_L, 1), &mut m), vec![UiEvent::Super('l', false)]);
        // 松开 Meta 后恢复普通字符
        assert!(translate(&ev(EV_KEY, KEY_LEFTMETA, 0), &mut m).is_empty());
        assert!(!m.meta);
        assert_eq!(translate(&ev(EV_KEY, KEY_L, 1), &mut m), vec![UiEvent::Char('l')]);
    }

    /// Super 组合键**不能退化成普通字符**（与 Alt 一致）；右 Meta 同理。
    ///
    /// 注意 `Super+1` 的期望在 §5.4 之后变了：它现在是**有意义的**
    /// （虚拟桌面要 `Super+1..4`，所以数字键位必须收），
    /// 但本条测试要守的性质**没变** —— 它绝不能变成一个裸 `'1'` 字符。
    #[test]
    fn super_with_non_alnum_does_not_leak_a_char() {
        let mut m = Mods::default();
        translate(&ev(EV_KEY, KEY_LEFTMETA, 1), &mut m);
        // 数字键位：收成组合键（虚拟桌面需要），**不是**普通字符
        let got = translate(&ev(EV_KEY, KEY_1, 1), &mut m);
        assert_eq!(got, vec![UiEvent::Super('1', false)]);
        assert!(
            !got.iter().any(|e| matches!(e, UiEvent::Char(_))),
            "Super 组合键绝不能漏成普通字符"
        );
        // 带 Shift 时要区分开（Super+Shift+1 = 把窗口搬过去）
        translate(&ev(EV_KEY, KEY_LEFTSHIFT, 1), &mut m);
        let got2 = translate(&ev(EV_KEY, KEY_1, 1), &mut m);
        assert_eq!(got2, vec![UiEvent::Super('1', true)], "Shift 必须被带上");
        translate(&ev(EV_KEY, KEY_LEFTSHIFT, 0), &mut m);
        // 标点键位仍然什么都不产生（避免漏出 `,` 这种散字）。
        // 51 是 `key_char` 里映射成逗号的那个键位（见该函数）。
        assert!(
            translate(&ev(EV_KEY, 51, 1), &mut m).is_empty(),
            "Super+标点不该产生任何事件"
        );
        let mut m2 = Mods::default();
        translate(&ev(EV_KEY, KEY_RIGHTMETA, 1), &mut m2);
        assert!(m2.meta, "右 Meta 也应算 Meta");
        assert_eq!(translate(&ev(EV_KEY, KEY_L, 1), &mut m2), vec![UiEvent::Super('l', false)]);
    }

    /// **回归：三个历史 bug 的状态组合都被钉住**（批次 AQ-1）。
    ///
    /// 这三条对应的正是附录 K / O / P —— 它们当年都藏在这个分支里。
    #[test]
    fn super_intent_covers_the_three_historical_bugs() {
        // ① 附录 O：**其它 Super 组合键不许锁屏**（当年分支开头无条件锁屏）
        for c in ['q', 'w', 'e', 'z', '0', '9', ' '] {
            assert_ne!(
                super_intent(c, false, 4),
                SuperIntent::Lock,
                "Super+{c} 不该锁屏"
            );
        }
        // ② 附录 P：**数量是参数，不是常量** —— 5 个桌面时 Super+5 应当可用
        assert_eq!(super_intent('5', false, 4), SuperIntent::Ignore, "4 个桌面时 Super+5 无效");
        assert_eq!(
            super_intent('5', false, 5),
            SuperIntent::SwitchSpace(4),
            "5 个桌面时 Super+5 必须能用（写死常量就错了）"
        );
        assert_eq!(super_intent('8', false, 8), SuperIntent::SwitchSpace(7));
        assert_eq!(super_intent('9', false, 8), SuperIntent::Ignore, "上限之外忽略");
        // ③ 附录 K：**锁屏的判断在最前** —— 数字判断不能抢走它
        assert_eq!(super_intent('l', false, 4), SuperIntent::Lock);
        assert_eq!(super_intent('l', true, 4), SuperIntent::Ignore, "Super+Shift+L 不是锁屏");
        // 0 不是"第 0 个桌面"
        assert_eq!(super_intent('0', false, 8), SuperIntent::Ignore);
    }

    /// 数字键的两种模式：`Super+N` 切、`Super+Shift+N` 搬。
    #[test]
    fn super_digits_switch_or_move() {
        assert_eq!(super_intent('1', false, 4), SuperIntent::SwitchSpace(0));
        assert_eq!(super_intent('4', false, 4), SuperIntent::SwitchSpace(3));
        assert_eq!(super_intent('1', true, 4), SuperIntent::MoveToSpace(0));
        assert_eq!(super_intent('4', true, 4), SuperIntent::MoveToSpace(3));
        // 只有 1 个桌面时，Super+2 无效（而不是越界）
        assert_eq!(super_intent('2', false, 1), SuperIntent::Ignore);
    }

    /// 回车的**优先级**：确认卡片 > 安装向导 > AI 指令条。
    ///
    /// 这三条断言对应三种"同时开着"的情形 —— 它们正是这类逻辑最容易写错的地方
    ///（把 `else if` 的顺序颠倒一行，行为就变了，而界面看不出任何异常）。
    #[test]
    fn enter_priority_is_confirm_then_installer_then_ai_bar() {
        // ① 确认卡片开着：即使向导也开着、指令条里还有字，回车也只与卡片有关
        assert_eq!(enter_intent(true, true, true, false), EnterIntent::Approve);
        assert_eq!(
            enter_intent(true, false, true, false),
            EnterIntent::EchoMismatch,
            "回显不匹配时**不能**放行，也不能顺手把指令条的字发出去"
        );
        // ② 没有卡片：向导优先于指令条
        assert_eq!(enter_intent(false, false, true, false), EnterIntent::StartInstall);
        // ③ 都没有：指令条有非空内容才发
        assert_eq!(enter_intent(false, false, false, false), EnterIntent::SendAi);
        assert_eq!(
            enter_intent(false, false, false, true),
            EnterIntent::Nothing,
            "空回车不该发出一个空查询"
        );
        // ④ 边界：什么都没有（三个都假）→ Nothing，不 panic
        assert_eq!(enter_intent(false, false, false, true), EnterIntent::Nothing);
    }

}
