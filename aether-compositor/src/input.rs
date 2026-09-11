//! evdev 输入后端（Linux 系统内路径）：读取 /dev/input/event*，
//! 把键盘/鼠标原始事件转为 UI 事件，经通道送到渲染主循环。
//!
//! QEMU 默认是 i8042 PS/2 键盘/鼠标，VBox vboxvga 也走 PS/2 辅助鼠标，
//! 因此只需 EV_KEY / EV_REL；EV_ABS（usb tablet）暂不解析。
//! 后台线程以 O_NONBLOCK 轮询全部设备，主循环每帧 drain 通道，
//! 采集速率与渲染帧率解耦（鼠标位移在通道里累积，不丢步）。

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// 渲染主循环消费的 UI 事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    /// 可打印字符（英文/符号直接进 AI 指令条，与预览版一致）
    Char(char),
    Enter,
    Backspace,
    Escape,
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
const KEY_ENTER: u16 = 28;
/// KEY_1 … KEY_0 = 2 … 11
const KEY_1: u16 = 2;
const KEY_0: u16 = 11;

/// 启动输入采集线程：持续打开 /dev/input/event* 并把事件送上通道。
pub fn spawn(tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let mut devs: Vec<File> = Vec::new();
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
                            for ui in translate(&raw) {
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

fn translate(raw: &RawEvent) -> Vec<UiEvent> {
    match raw.type_ {
        EV_KEY => {
            // 鼠标按键的按下/松开都要上报；键盘按键只取按下/重复
            if raw.code == BTN_LEFT {
                return vec![if raw.value == 1 { UiEvent::MouseDown } else { UiEvent::MouseUp }];
            }
            if raw.value == 0 {
                return vec![];
            }
            match raw.code {
                KEY_ENTER => vec![UiEvent::Enter],
                KEY_BACKSPACE => vec![UiEvent::Backspace],
                KEY_ESC => vec![UiEvent::Escape],
                KEY_1..=KEY_0 => {
                    // 1–4 保留为布局快捷键，其余数字照常进指令条
                    if raw.code <= KEY_1 + 3 {
                        vec![UiEvent::LayoutKey((raw.code - KEY_1 + 1) as usize)]
                    } else {
                        let digit = if raw.code == KEY_0 { b'0' } else { b'1' + (raw.code - KEY_1) as u8 };
                        vec![UiEvent::Char(digit as char)]
                    }
                }
                _ => key_char(raw.code).into_iter().map(UiEvent::Char).collect(),
            }
        }
        EV_REL => match raw.code {
            REL_X => vec![UiEvent::MouseMove { dx: raw.value, dy: 0 }],
            REL_Y => vec![UiEvent::MouseMove { dx: 0, dy: raw.value }],
            _ => vec![],
        },
        _ => vec![],
    }
}

/// 键位 → 字符（US 布局；数字 1–4 已在上面被布局快捷键截走）。
fn key_char(code: u16) -> Option<char> {
    Some(match code {
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

    #[test]
    fn enter_and_backspace() {
        assert!(matches!(translate(&ev(EV_KEY, KEY_ENTER, 1))[0], UiEvent::Enter));
        assert!(matches!(translate(&ev(EV_KEY, KEY_BACKSPACE, 1))[0], UiEvent::Backspace));
        assert!(translate(&ev(EV_KEY, KEY_ENTER, 0)).is_empty()); // 松开忽略
    }

    #[test]
    fn letters_and_symbols() {
        assert_eq!(translate(&ev(EV_KEY, 30, 1)), vec![UiEvent::Char('a')]);
        assert_eq!(translate(&ev(EV_KEY, 36, 1)), vec![UiEvent::Char('j')]);
        assert_eq!(translate(&ev(EV_KEY, 57, 1)), vec![UiEvent::Char(' ')]);
    }

    #[test]
    fn digits_one_to_four_are_layout_keys() {
        assert!(matches!(translate(&ev(EV_KEY, 2, 1))[0], UiEvent::LayoutKey(1)));
        assert!(matches!(translate(&ev(EV_KEY, 5, 1))[0], UiEvent::LayoutKey(4)));
        assert_eq!(translate(&ev(EV_KEY, 6, 1)), vec![UiEvent::Char('5')]);
        assert_eq!(translate(&ev(EV_KEY, 11, 1)), vec![UiEvent::Char('0')]);
    }

    #[test]
    fn mouse_rel_and_button() {
        assert_eq!(
            translate(&ev(EV_REL, REL_X, 7)),
            vec![UiEvent::MouseMove { dx: 7, dy: 0 }]
        );
        assert!(matches!(translate(&ev(EV_KEY, BTN_LEFT, 1))[0], UiEvent::MouseDown));
        assert!(matches!(translate(&ev(EV_KEY, BTN_LEFT, 0))[0], UiEvent::MouseUp));
    }
}
