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
    Enter,
    Backspace,
    Escape,
    /// Ctrl + 字母（Ctrl+C/Ctrl+W…）。**独立于 Char**：组合键与字符是两类语义，
    /// 混在一起会让"按住 Ctrl 时字符被吞掉"变成隐式行为。
    Ctrl(char),
    /// 导航键（方向/翻页/Home/End/Tab/Delete）
    Nav(NavKey),
    /// 数字键 1–4：布局快捷键
    LayoutKey(usize),
    /// 鼠标相对位移（evdev REL 事件，按下时主循环据此累计坐标）
    MouseMove { dx: i32, dy: i32 },
    MouseDown,
    MouseUp,
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
#[cfg(target_os = "linux")]
pub fn spawn(tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let mut devs: Vec<File> = Vec::new();
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
                // 数字 1–4 是布局快捷键，但**按住 Shift 时让位给上档符号**
                KEY_1..=KEY_0 if !mods.shift && !mods.ctrl => {
                    if raw.code <= KEY_1 + 3 {
                        return vec![UiEvent::LayoutKey((raw.code - KEY_1 + 1) as usize)];
                    }
                }
                _ => {}
            }
            let Some(base) = key_char(raw.code) else {
                return vec![];
            };
            if mods.ctrl {
                // Ctrl+字母 → 组合键；Ctrl+其他不产生字符（否则会漏出 ';' 之类的散字）
                return if base.is_ascii_alphabetic() {
                    vec![UiEvent::Ctrl(base.to_ascii_lowercase())]
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
    fn digits_one_to_four_are_layout_keys() {
        assert!(matches!(t(2, 1)[0], UiEvent::LayoutKey(1)));
        assert!(matches!(t(5, 1)[0], UiEvent::LayoutKey(4)));
        assert_eq!(t(6, 1), vec![UiEvent::Char('5')]);
        assert_eq!(t(11, 1), vec![UiEvent::Char('0')]);
    }

    #[test]
    fn mouse_rel_and_button() {
        assert_eq!(
            translate(&ev(EV_REL, REL_X, 7), &mut Mods::default()),
            vec![UiEvent::MouseMove { dx: 7, dy: 0 }]
        );
        assert!(matches!(t(BTN_LEFT, 1)[0], UiEvent::MouseDown));
        assert!(matches!(t(BTN_LEFT, 0)[0], UiEvent::MouseUp));
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
            vec![UiEvent::Ctrl('c')] // KEY_C
        );
        // 松开后恢复为普通字符
        assert!(translate(&ev(EV_KEY, KEY_LEFTCTRL, 0), &mut m).is_empty());
        assert!(!m.ctrl);
        assert_eq!(translate(&ev(EV_KEY, 46, 1), &mut m), vec![UiEvent::Char('c')]);
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
}
