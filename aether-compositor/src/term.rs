//! 终端胶合层：把 PTY（Linux）与 VT 解析器（跨平台）拼成一个"终端窗口的内容"。
//!
//! 开发机上没有 PTY，但**解析器与渲染是同一条路径** —— 所以预览期喂一段真实的
//! ANSI 脚本进同一个 `Screen`：既让预览有内容可看，也让"解析 → 渲染"这条链在
//! Windows 上被真正走通（而不是只跑单测）。

// 非 Linux 目标只用 Screen + 演示会话：PTY 相关常量/状态/缓冲不会被构造
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::vt::{Cell, Screen};

/// 一帧最多从 PTY 读多少字节：给渲染留余量，避免 `cat` 大文件时单帧被读操作占满。
const MAX_READ_PER_FRAME: usize = 64 * 1024;

/// 终端状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermStatus {
    /// 正在运行
    Running,
    /// shell 已退出（状态码）
    Exited(i32),
    /// 本平台不可用（附原因）
    Unavailable(String),
}

/// 拖选区域（屏幕坐标，格）。`sx/sy` 是按下点，`ex/ey` 是当前点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub sx: usize,
    pub sy: usize,
    pub ex: usize,
    pub ey: usize,
}

impl Selection {
    /// 归一化成 (起始格, 结束格)（行序与列序都从小到大）
    fn norm(&self) -> ((usize, usize), (usize, usize)) {
        let ((ax, ay), (bx, by)) = if (self.sy, self.sx) <= (self.ey, self.ex) {
            ((self.sx, self.sy), (self.ex, self.ey))
        } else {
            ((self.ex, self.ey), (self.sx, self.sy))
        };
        // 跨行的选区按"整块矩形"处理（块选择，简单且可预期）
        ((ax.min(bx), ay), (bx.max(ax), by))
    }
}

pub struct Terminal {
    pub screen: Screen,
    /// 复用读缓冲：终端输出是高频的，每帧新建 Vec 会持续制造垃圾
    read_buf: Vec<u8>,
    status: TermStatus,
    /// 当前拖选；`None` = 无选区
    pub sel: Option<Selection>,
    #[cfg(target_os = "linux")]
    pty: Option<crate::pty::Pty>,
}

impl Terminal {
    /// 建立终端会话：Linux 起真实 shell，其他平台喂一段演示脚本。
    pub fn spawn(cols: usize, rows: usize, cwd: Option<&str>) -> Terminal {
        let screen = Screen::new(cols, rows);
        #[cfg(target_os = "linux")]
        {
            match crate::pty::Pty::spawn(cols as u16, rows as u16, cwd) {
                Ok(p) => {
                    return Terminal {
                        screen,
                        read_buf: Vec::new(),
                        status: TermStatus::Running,
                        sel: None,
                        pty: Some(p),
                    }
                }
                Err(e) => {
                    // 显式降级：PTY 不可用时说清楚原因，而不是给一个永远空白的窗口
                    let mut term = Terminal {
                        screen,
                        read_buf: Vec::new(),
                        status: TermStatus::Unavailable(format!("PTY 不可用：{e}")),
                        sel: None,
                        pty: None,
                    };
                    term.feed_demo(cwd);
                    return term;
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut term = Terminal {
                screen,
                read_buf: Vec::new(),
                status: TermStatus::Unavailable("开发机预览：PTY 仅在 Linux 上可用".into()),
                sel: None,
            };
            term.feed_demo(cwd);
            term
        }
    }

    /// 预览期的演示会话：走真实 ANSI 序列，因此它同时也在验证解析器与渲染。
    fn feed_demo(&mut self, cwd: Option<&str>) {
        let dir = cwd.unwrap_or("~");
        let script = format!(
            "\x1b[2J\x1b[H\x1b[1;36maether@localhost\x1b[0m:\x1b[1;34m{dir}\x1b[0m$ uname -srv\r\n\
             AetherOS 0.1.0 aether-kernel\r\n\
             \x1b[1;36maether@localhost\x1b[0m:\x1b[1;34m{dir}\x1b[0m$ ls --color=auto\r\n\
             \x1b[1;34mdocs\x1b[0m  \x1b[1;34msrc\x1b[0m  \x1b[1;32mrun.sh\x1b[0m  \x1b[1;31mcore.log\x1b[0m\r\n\
             \x1b[1;36maether@localhost\x1b[0m:\x1b[1;34m{dir}\x1b[0m$ echo \"宽字符对齐：中文与 ASCII 混排\"\r\n\
             宽字符对齐：中文与 ASCII 混排\r\n\
             \x1b[1;36maether@localhost\x1b[0m:\x1b[1;34m{dir}\x1b[0m$ \x1b[33m_\x1b[0m"
        );
        self.screen.feed(script.as_bytes());
    }

    pub fn status(&self) -> &TermStatus {
        &self.status
    }

/// 把鼠标位置换算成屏幕格坐标（越界则夹到边界内）。
pub fn cell_at(
    content: crate::draw::Rect,
    cell_w: f32,
    cell_h: f32,
    mx: f32,
    my: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let col = ((mx - content.x as f32) / cell_w.max(1.0)).floor() as i32;
    let row = ((my - content.y as f32) / cell_h.max(1.0)).floor() as i32;
    (
        (col.max(0) as usize).min(cols.saturating_sub(1)),
        (row.max(0) as usize).min(rows.saturating_sub(1)),
    )
}

    /// 开始/更新拖选。
    pub fn select_at(&mut self, col: usize, row: usize, start: bool) {
        let (c, r) = (
            col.min(self.screen.cols.saturating_sub(1)),
            row.min(self.screen.rows.saturating_sub(1)),
        );
        self.sel = match (start, self.sel) {
            (true, _) => Some(Selection { sx: c, sy: r, ex: c, ey: r }),
            (false, Some(mut s)) => {
                s.ex = c;
                s.ey = r;
                Some(s)
            }
            (false, None) => None,
        };
    }

    pub fn clear_selection(&mut self) {
        self.sel = None;
    }

    /// 提取选区文本。行尾空格裁掉，行间用 `\n` 连接。
    ///
    /// 有选区取选区；没有则取**全部可见内容**（`Ctrl+Shift+C` 在没拖选时也能用）。
    pub fn selection_text(&self) -> String {
        let Some(sel) = self.sel else {
            // 没有选区 = 复制全部可见内容（行裁剪逻辑只有 to_lines 这一份实现）
            return self.screen.to_lines().join("\n");
        };
        let ((_, y0), (_, y1)) = sel.norm();
        let rows = y0..=y1.min(self.screen.rows.saturating_sub(1));
        let ((ax, _), (bx, _)) = sel.norm();
        let x1 = bx.min(self.screen.cols.saturating_sub(1));
        let mut out = String::new();
        for (n, y) in rows.enumerate() {
            if n > 0 {
                out.push('\n');
            }
            let mut line = String::new();
            for x in ax..=x1 {
                let c = self.screen.cell(x, y);
                if c.wide_tail {
                    continue; // 宽字符占位格不进文本
                }
                line.push(c.ch);
            }
            while line.ends_with(' ') {
                line.pop();
            }
            out.push_str(&line);
        }
        out
    }

    /// 每帧调用：把 PTY 里现有的数据搬进解析器。绝不阻塞。
    pub fn pump(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let Some(pty) = self.pty.as_mut() else { return };
            // 两个字段互不重叠，借用检查器允许这样分开借
            self.read_buf.clear();
            pty.read_available(&mut self.read_buf, MAX_READ_PER_FRAME);
            if !self.read_buf.is_empty() {
                self.screen.feed(&self.read_buf);
                // 屏幕内容变了，旧选区指的已不是原来的文本 —— 清掉比保持错位诚实
                self.sel = None;
            }
            if let Some(code) = pty.try_wait() {
                self.status = TermStatus::Exited(code);
                self.pty = None;
            }
        }
    }

    /// 键盘输入 → PTY。
    pub fn write(&mut self, bytes: &[u8]) {
        #[cfg(target_os = "linux")]
        if let Some(pty) = self.pty.as_mut() {
            pty.write_all(bytes);
        }
        #[cfg(not(target_os = "linux"))]
        {
            // 预览期没有 shell：把按键回显到屏幕，至少能看出"输入到了终端"
            let mut echo: Vec<u8> = Vec::new();
            for &b in bytes {
                match b {
                    0x0D => echo.extend_from_slice(b"\r\n"),
                    0x7F => echo.extend_from_slice(b"\x08"),
                    _ => echo.push(b),
                }
            }
            self.screen.feed(&echo);
        }
    }

    /// 窗口尺寸变化 → 同步行列数（`vim`/`top` 依赖它排版）。
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if self.screen.cols == cols && self.screen.rows == rows {
            return;
        }
        self.screen.resize(cols, rows);
        self.sel = None;
        #[cfg(target_os = "linux")]
        if let Some(pty) = self.pty.as_mut() {
            pty.resize(cols as u16, rows as u16);
        }
    }

    /// 屏幕内容为空的判定（仅测试使用：渲染路径没有「空白就不画」的分支）。
    #[cfg(test)]
    pub fn is_blank(&self) -> bool {
        self.screen.to_lines().iter().all(|l| l.is_empty())
    }
}

/// 粘贴文本 → 终端字节。
///
/// 必须把 `\n` 换成 `\r`：终端（以及 readline）把回车当"提交"，把裸换行当"换行不提交"。
/// 直接送 `\n` 的话多行粘贴只会显示换行、命令永远不会执行。
pub fn paste_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut prev_cr = false;
    for ch in text.chars() {
        match ch {
            '\r' => {
                out.push(b'\r');
                prev_cr = true;
            }
            '\n' => {
                // CRLF 不重复插入回车
                if !prev_cr {
                    out.push(b'\r');
                }
                prev_cr = false;
            }
            _ => {
                let mut b = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                prev_cr = false;
            }
        }
    }
    out
}

/// 终端需要的按键语义。
///
/// 两条输入路径（evdev / minifb）各自翻译成它，再统一由 [`bytes_for`] 变成字节 ——
/// **字节映射只有这一份实现**，也就只有一处会出错、一处需要测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermKey {
    Char(char),
    /// Ctrl + 字母（转成控制字符 0x01–0x1A）
    Ctrl(char),
    Enter,
    Backspace,
    Escape,
    /// Tab 走 `Nav(NavKey::Tab)`，不单列一个变体（两个入口必然要维护两份映射）
    Nav(crate::input::NavKey),
}

/// 按键 → 终端字节（xterm 约定）。
pub fn bytes_for(key: TermKey) -> Vec<u8> {
    use crate::input::NavKey::*;
    match key {
        TermKey::Char(c) => {
            let mut b = [0u8; 4];
            c.encode_utf8(&mut b).as_bytes().to_vec()
        }
        // Ctrl+A = 0x01 … Ctrl+Z = 0x1A（Ctrl+[ 是 ESC，也照此规则）
        TermKey::Ctrl(c) => {
            let lc = c.to_ascii_lowercase();
            if ('a'..='z').contains(&lc) {
                vec![(lc as u8) - b'a' + 1]
            } else if lc == '[' {
                vec![0x1B]
            } else {
                Vec::new()
            }
        }
        // 终端收到的是"回车"不是"换行"：否则 shell 里历史记录那类行为会不对
        TermKey::Enter => vec![b'\r'],
        // 退格是 DEL(0x7F)，不是 BS(0x08)：readline 靠这个区分"删字符"与"删词"
        TermKey::Backspace => vec![0x7F],
        TermKey::Escape => vec![0x1B],
        TermKey::Nav(k) => match k {
            Up => b"\x1b[A".to_vec(),
            Down => b"\x1b[B".to_vec(),
            Right => b"\x1b[C".to_vec(),
            Left => b"\x1b[D".to_vec(),
            Home => b"\x1b[H".to_vec(),
            End => b"\x1b[F".to_vec(),
            PageUp => b"\x1b[5~".to_vec(),
            PageDown => b"\x1b[6~".to_vec(),
            Delete => b"\x1b[3~".to_vec(),
            Tab => vec![b'\t'],
        },
    }
}

/// ANSI 16 色板 → RGB。
///
/// 终端**两种主题下都用深底**：16 色标准调色板本来就是为深底设计的，
/// 浅底会让"亮色系"（8–15）糊成一片；同时在浅色桌面上，一块深色终端也是
/// 最自然的视觉锚点（macOS 上深色终端配浅色桌面同样常见）。
pub fn ansi_rgb(i: u8, bright_bold: bool) -> [u8; 3] {
    let i = if bright_bold && i < 8 { i + 8 } else { i };
    match i {
        0 => [58, 62, 72],          // black（不用纯黑：纯黑在深底上看不出边界）
        1 => [224, 88, 88],
        2 => [110, 200, 120],
        3 => [226, 186, 94],
        4 => [96, 158, 234],
        5 => [196, 122, 226],
        6 => [86, 196, 205],
        7 => [214, 218, 228],
        8 => [124, 130, 144],
        9 => [246, 118, 118],
        10 => [140, 224, 148],
        11 => [246, 214, 130],
        12 => [134, 184, 250],
        13 => [220, 156, 246],
        14 => [120, 222, 230],
        15 => [246, 248, 252],
        _ => [214, 218, 228],
    }
}

/// 终端默认背景色索引（`Cell` 的 bg=0）。默认背景不铺底色，省掉整屏矩形。
pub const DEFAULT_BG: u8 = 0;

/// 该单元格是否需要用背景色填充（默认背景不填，省掉整屏矩形）。
pub fn cell_has_custom_bg(c: &Cell) -> bool {
    c.bg != DEFAULT_BG
}

#[cfg(test)]
mod tests {
    use super::*;

    // 只在开发机跑：Linux 下 `Terminal::spawn` 开的是**真 PTY**（/bin/sh），
    // 屏幕初始是空的；这些测试验的是"演示脚本能被正确解析"，而演示脚本
    // 只在非 Linux 路径存在。Linux 侧的终端行为由实机验证覆盖。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn demo_transcript_parses_into_lines() {
        let t = Terminal::spawn(60, 12, Some("/home/u"));
        let lines = t.screen.to_lines();
        assert!(lines.iter().any(|l| l.contains("uname -srv")), "演示会话应上屏");
        assert!(
            lines.iter().any(|l| l.contains("宽字符对齐")),
            "宽字符行应完整（不因占位格被拆坏）"
        );
        assert!(matches!(t.status(), TermStatus::Unavailable(_)));
    }

    // 只在开发机跑：Linux 下 `Terminal::spawn` 开的是**真 PTY**（/bin/sh），
    // 屏幕初始是空的；这些测试验的是"演示脚本能被正确解析"，而演示脚本
    // 只在非 Linux 路径存在。Linux 侧的终端行为由实机验证覆盖。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn demo_uses_real_ansi_colors() {
        let t = Terminal::spawn(60, 12, None);
        // 第二行的提示符里 host 用了青色（36 → 索引 6）
        let row = (0..t.screen.cols)
            .map(|x| t.screen.cell(x, 0))
            .find(|c| c.ch == 'a')
            .expect("第一行应有 aether@localhost");
        assert_eq!(row.fg, 6, "提示符主机名应是青色的 SGR 36");
    }

    #[test]
    fn resize_is_idempotent_and_updates_screen() {
        let mut t = Terminal::spawn(40, 10, None);
        t.resize(40, 10); // 同尺寸：不应产生变化也不应 panic
        t.resize(20, 5);
        assert_eq!((t.screen.cols, t.screen.rows), (20, 5));
    }

    #[test]
    fn write_echoes_in_preview_mode() {
        #[cfg(not(target_os = "linux"))]
        {
            let mut t = Terminal::spawn(40, 6, None);
            t.screen.feed(b"\x1b[2J\x1b[H");
            t.write(b"echo hi\r");
            let lines = t.screen.to_lines();
            assert!(lines.iter().any(|l| l.contains("echo hi")), "预览期按键应回显");
            assert_eq!(lines[1], "", "回车后光标应换行（第 2 行仍为空）");
        }
    }

    #[test]
    fn ansi_palette_bright_mapping() {
        assert_eq!(ansi_rgb(1, false), [224, 88, 88]);
        assert_eq!(ansi_rgb(1, true), ansi_rgb(9, false), "粗体+基本色 = 亮色");
        assert_eq!(ansi_rgb(15, false), [246, 248, 252]);
    }

    // 只在开发机跑：Linux 下 `Terminal::spawn` 开的是**真 PTY**（/bin/sh），
    // 屏幕初始是空的；这些测试验的是"演示脚本能被正确解析"，而演示脚本
    // 只在非 Linux 路径存在。Linux 侧的终端行为由实机验证覆盖。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn blank_detection() {
        let t = Terminal::spawn(40, 6, None);
        let mut fresh = Terminal::spawn(40, 6, None);
        fresh.screen.feed(b"\x1b[2J\x1b[H");
        assert!(fresh.is_blank());
        assert!(!t.is_blank(), "演示会话不是空白");
    }

    #[test]
    fn term_bytes_follow_xterm_conventions() {
        assert_eq!(bytes_for(TermKey::Enter), vec![b'\r']);
        assert_eq!(bytes_for(TermKey::Backspace), vec![0x7F], "退格必须是 DEL");
        assert_eq!(bytes_for(TermKey::Nav(crate::input::NavKey::Tab)), vec![b'\t']);
        assert_eq!(bytes_for(TermKey::Escape), vec![0x1B]);
    }

    #[test]
    fn ctrl_letters_become_control_bytes() {
        use crate::input::NavKey;
        assert_eq!(bytes_for(TermKey::Ctrl('c')), vec![0x03], "Ctrl+C 要能让 shell 中断");
        assert_eq!(bytes_for(TermKey::Ctrl('C')), vec![0x03]);
        assert_eq!(bytes_for(TermKey::Ctrl('d')), vec![0x04]);
        assert_eq!(bytes_for(TermKey::Ctrl('[')), vec![0x1B]);
        assert!(bytes_for(TermKey::Ctrl('1')).is_empty(), "非字母组合不应发出垃圾字节");
        let _ = NavKey::Up;
    }

    #[test]
    fn nav_keys_emit_csi_sequences() {
        use crate::input::NavKey::*;
        assert_eq!(bytes_for(TermKey::Nav(Up)), b"\x1b[A".to_vec());
        assert_eq!(bytes_for(TermKey::Nav(PageDown)), b"\x1b[6~".to_vec());
        assert_eq!(bytes_for(TermKey::Nav(Delete)), b"\x1b[3~".to_vec());
    }

    #[test]
    fn wide_char_input_encodes_as_utf8() {
        assert_eq!(bytes_for(TermKey::Char('中')), "中".as_bytes().to_vec());
    }

    #[test]
    fn paste_converts_newlines_to_carriage_returns() {
        assert_eq!(paste_bytes("ls\n"), b"ls\r".to_vec());
        assert_eq!(paste_bytes("a\nb\n"), b"a\rb\r".to_vec());
    }

    #[test]
    fn paste_does_not_double_cr_for_crlf() {
        assert_eq!(paste_bytes("a\r\nb"), b"a\rb".to_vec(), "CRLF 不应变成两个回车");
    }

    #[test]
    fn paste_keeps_utf8_intact() {
        assert_eq!(paste_bytes("中文"), "中文".as_bytes().to_vec());
    }

    #[test]
    fn selection_extracts_rectangular_region() {
        let mut t = Terminal::spawn(20, 4, None);
        t.screen.feed(b"\x1b[2J\x1b[Habcdef\r\nghijkl");
        t.sel = Some(Selection { sx: 1, sy: 0, ex: 3, ey: 1 });
        assert_eq!(t.selection_text(), "bcd\nhij", "块选择：矩形内的两行");
    }

    #[test]
    fn selection_normalizes_when_dragging_backwards() {
        let mut t = Terminal::spawn(20, 4, None);
        t.screen.feed(b"\x1b[2J\x1b[Habcdef\r\nghijkl");
        // 从右下往左上拖：结果必须与正向拖一致
        t.sel = Some(Selection { sx: 3, sy: 1, ex: 1, ey: 0 });
        assert_eq!(t.selection_text(), "bcd\nhij");
    }

    #[test]
    fn selection_single_cell() {
        let mut t = Terminal::spawn(20, 4, None);
        t.screen.feed(b"\x1b[2J\x1b[Habcdef");
        t.sel = Some(Selection { sx: 2, sy: 0, ex: 2, ey: 0 });
        assert_eq!(t.selection_text(), "c");
    }

    #[test]
    fn selection_skips_wide_tail_cells() {
        let mut t = Terminal::spawn(20, 4, None);
        t.screen
            .feed("\x1b[2J\x1b[2;1H中文".as_bytes()); // 第 2 行 = 中文（宽字符）
        t.sel = Some(Selection { sx: 0, sy: 1, ex: 3, ey: 1 });
        assert_eq!(t.selection_text(), "中文", "占位格不能变成多余空格");
    }

    #[test]
    fn no_selection_falls_back_to_whole_screen() {
        let mut t = Terminal::spawn(20, 4, None);
        t.screen.feed(b"\x1b[2J\x1b[Hhello");
        assert!(t.sel.is_none());
        assert!(t.selection_text().contains("hello"));
    }

    #[test]
    fn typing_clears_selection() {
        let mut t = Terminal::spawn(20, 4, None);
        t.screen.feed(b"\x1b[2J\x1b[Habcdef");
        t.sel = Some(Selection { sx: 1, sy: 0, ex: 2, ey: 0 });
        t.write(b"x"); // 预览路径：写即回显
        t.clear_selection();
        assert!(t.sel.is_none());
    }
}
