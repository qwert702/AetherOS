//! VT/ANSI 终端解析器：**纯逻辑，跨平台**。
//!
//! 为什么单独成文件并且不碰任何 I/O：终端的正确性几乎全在解析器上，而它无法在开发机
//! （Windows）上手工验证 —— PTY 是 Linux 专属。把解析器做成纯函数式状态机后，
//! 全部行为都能用单元测试钉住，这也是"纯逻辑必须跨平台"这条约定的直接收益。
//!
//! 支持范围（有意划定，见 docs/PRODUCTION-PLAN-2026-09-27.md §1.5）：
//! 够 `ls`/`vim`/`top`/`less`/`git log` 用 —— 光标定位、清屏擦行、SGR 8/16 色、
//! 滚屏、自动折行、CJK 宽字符。**不做**：鼠标上报、完整 DA/DSR 应答、滚动区
//! （DECSTBM）、字符集切换（G0/G1）。

/// 一个字符格。
///
/// 宽字符（CJK）占两格：第一格存字符，第二格 `wide_tail = true` 作为占位，
/// 否则整行会错位（这是终端里最常见的错位来源）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    /// 前景/背景色索引：0–7 基本色，8–15 亮色
    pub fg: u8,
    pub bg: u8,
    pub bold: bool,
    /// 宽字符的第二格占位
    pub wide_tail: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: 7, bg: 0, bold: false, wide_tail: false }
    }
}

impl Cell {
    fn blank(fg: u8, bg: u8, bold: bool) -> Self {
        Cell { ch: ' ', fg, bg, bold, wide_tail: false }
    }
}

/// 解析器状态机。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    /// 刚收到 ESC
    Escape,
    /// ESC [（CSI）
    Csi,
    /// ESC ]（OSC，标题设置等）—— 吞到 BEL 或 ST
    Osc,
    /// ESC ] ... ESC（等 ST 的第二个字节）
    OscEsc,
    /// ESC ( ) * + 等字符集选择：吞掉一个字节
    Charset,
}

/// 终端的可打印宽度判定（East Asian Wide/Fullwidth 的常用区间）。
///
/// 用区间表而不是查字体度量：解析器必须与渲染解耦，而且这个判定要能被测试。
pub fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F        // Hangul Jamo
        | 0x2E80..=0x303E      // CJK 部首、标点
        | 0x3041..=0x33FF      // 假名、注音、CJK 兼容
        | 0x3400..=0x4DBF      // CJK 扩展 A
        | 0x4E00..=0x9FFF      // CJK 统一表意
        | 0xA000..=0xA4CF      // 彝文
        | 0xAC00..=0xD7A3      // Hangul 音节
        | 0xF900..=0xFAFF      // CJK 兼容表意
        | 0xFE30..=0xFE6F      // CJK 兼容形式
        | 0xFF00..=0xFF60      // 全角形式
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F    // emoji（按宽处理，避免错位）
        | 0x20000..=0x3FFFD)
}

/// 终端屏幕模型 + ANSI 状态机。
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    cells: Vec<Cell>,
    pub cur_x: usize,
    pub cur_y: usize,
    fg: u8,
    bg: u8,
    bold: bool,
    state: State,
    /// CSI 参数字节（用 `;` 分隔）
    params: Vec<u16>,
    cur_param: Option<u16>,
    /// CSI 的私有前缀（如 `?25`）
    private: bool,
    utf8: [u8; 4],
    utf8_have: usize,
    utf8_need: usize,
    saved: (usize, usize),
    /// 上一字符刚好写到行尾：下一个可打印字符要先换行（延迟折行，符合 xterm 行为）
    wrap_pending: bool,
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut s = Screen {
            cols: cols.max(1),
            rows: rows.max(1),
            cells: Vec::new(),
            cur_x: 0,
            cur_y: 0,
            fg: 7,
            bg: 0,
            bold: false,
            state: State::Ground,
            params: Vec::new(),
            cur_param: None,
            private: false,
            utf8: [0; 4],
            utf8_have: 0,
            utf8_need: 0,
            saved: (0, 0),
            wrap_pending: false,
        };
        s.cells = vec![Cell::default(); s.cols * s.rows];
        s
    }

    pub fn cell(&self, x: usize, y: usize) -> Cell {
        self.cells[y * self.cols + x]
    }

    /// 改变尺寸：内容按原样保留能保留的部分（不重排，够用即可）。
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let mut next = vec![Cell::default(); cols * rows];
        for y in 0..rows.min(self.rows) {
            for x in 0..cols.min(self.cols) {
                next[y * cols + x] = self.cells[y * self.cols + x];
            }
        }
        self.cells = next;
        self.cols = cols;
        self.rows = rows;
        self.cur_x = self.cur_x.min(cols - 1);
        self.cur_y = self.cur_y.min(rows - 1);
        self.wrap_pending = false;
    }

    /// 喂入原始字节流（可能在任何位置截断，包括 UTF-8 序列中间）。
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.feed_byte(b);
        }
    }

    fn feed_byte(&mut self, b: u8) {
        // UTF-8 续字节优先处理：控制字符不会出现在多字节序列里
        if self.utf8_need > 0 {
            if b & 0xC0 == 0x80 {
                self.utf8[self.utf8_have] = b;
                self.utf8_have += 1;
                if self.utf8_have == self.utf8_need {
                    let n = self.utf8_need;
                    self.utf8_need = 0;
                    self.utf8_have = 0;
                    if let Ok(s) = std::str::from_utf8(&self.utf8[..n]) {
                        if let Some(ch) = s.chars().next() {
                            self.put_char(ch);
                        }
                    }
                }
                return;
            }
            // 序列被打断：丢弃半截，按普通字节重新处理
            self.utf8_need = 0;
            self.utf8_have = 0;
        }

        match self.state {
            State::Ground => match b {
                0x1B => self.state = State::Escape,
                0x0D => {
                    self.cur_x = 0;
                    self.wrap_pending = false;
                }
                0x0A | 0x0B | 0x0C => self.line_feed(),
                0x08 => {
                    // 退格：不跨行（xterm 行为）
                    self.cur_x = self.cur_x.saturating_sub(1);
                    self.wrap_pending = false;
                }
                0x09 => {
                    let next = (self.cur_x / 8 + 1) * 8;
                    self.cur_x = next.min(self.cols - 1);
                    self.wrap_pending = false;
                }
                0x07 => {} // BEL：不响铃（无音频后端）
                0x00..=0x1F => {}
                _ => self.byte_or_utf8(b),
            },
            State::Escape => {
                self.state = State::Ground;
                match b {
                    b'[' => {
                        self.params.clear();
                        self.cur_param = None;
                        self.private = false;
                        self.state = State::Csi;
                    }
                    b']' => self.state = State::Osc,
                    b'(' | b')' | b'*' | b'+' => self.state = State::Charset,
                    b'=' | b'>' => {} // 键盘模式（DECKPAM/DECKPNM）：忽略
                    b'7' => self.saved = (self.cur_x, self.cur_y),
                    b'8' => {
                        self.cur_x = self.saved.0.min(self.cols - 1);
                        self.cur_y = self.saved.1.min(self.rows - 1);
                    }
                    b'c' => self.reset(),
                    b'M' => self.reverse_index(),
                    b'D' => self.line_feed(),
                    b'E' => {
                        self.cur_x = 0;
                        self.line_feed();
                    }
                    _ => {}
                }
            }
            State::Csi => match b {
                b'?' | b'>' | b'<' | b'=' => self.private = true,
                b'0'..=b'9' => {
                    let v = self.cur_param.unwrap_or(0) as u32 * 10 + (b - b'0') as u32;
                    self.cur_param = Some(v.min(65535) as u16);
                }
                b';' => {
                    self.params.push(self.cur_param.unwrap_or(0));
                    self.cur_param = None;
                }
                0x40..=0x7E => {
                    if let Some(p) = self.cur_param.take() {
                        self.params.push(p);
                    }
                    self.state = State::Ground;
                    self.dispatch_csi(b);
                }
                _ => {} // 中间字节（空格、$ 等）：忽略
            },
            State::Osc => match b {
                0x07 => self.state = State::Ground,
                0x1B => self.state = State::OscEsc,
                _ => {}
            },
            State::OscEsc => {
                self.state = if b == b'\\' { State::Ground } else { State::Osc };
            }
            State::Charset => self.state = State::Ground,
        }
    }

    fn byte_or_utf8(&mut self, b: u8) {
        if b < 0x80 {
            self.put_char(b as char);
        } else if b & 0xE0 == 0xC0 {
            self.utf8 = [b, 0, 0, 0];
            self.utf8_have = 1;
            self.utf8_need = 2;
        } else if b & 0xF0 == 0xE0 {
            self.utf8 = [b, 0, 0, 0];
            self.utf8_have = 1;
            self.utf8_need = 3;
        } else if b & 0xF8 == 0xF0 {
            self.utf8 = [b, 0, 0, 0];
            self.utf8_have = 1;
            self.utf8_need = 4;
        }
        // 0x80..0xC0 的孤立续字节：丢弃
    }

    /// 写入一个字符并推进光标（含宽字符与自动折行）。
    fn put_char(&mut self, ch: char) {
        let width = if is_wide(ch) { 2 } else { 1 };
        if self.wrap_pending {
            self.cur_x = 0;
            self.line_feed();
            self.wrap_pending = false;
        }
        if self.cur_x + width > self.cols {
            // 行尾放不下：先折行（宽字符不跨行拆开）
            self.cur_x = 0;
            self.line_feed();
        }
        let idx = self.cur_y * self.cols + self.cur_x;
        self.cells[idx] = Cell { ch, fg: self.fg, bg: self.bg, bold: self.bold, wide_tail: false };
        if width == 2 {
            // 第二格占位（清掉它原有的内容，否则旧字符会从宽字右边露出来）
            self.cells[idx + 1] = Cell {
                ch: ' ',
                fg: self.fg,
                bg: self.bg,
                bold: self.bold,
                wide_tail: true,
            };
        }
        self.cur_x += width;
        if self.cur_x >= self.cols {
            self.cur_x = self.cols - 1;
            self.wrap_pending = true;
        }
    }

    /// 换行：在底部时整屏上滚。
    fn line_feed(&mut self) {
        if self.cur_y + 1 >= self.rows {
            self.scroll_up(1);
        } else {
            self.cur_y += 1;
        }
        self.wrap_pending = false;
    }

    fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.rows);
        if n == 0 {
            return;
        }
        let keep = (self.rows - n) * self.cols;
        self.cells.copy_within(n * self.cols.., 0);
        for c in &mut self.cells[keep..] {
            *c = Cell::blank(self.fg, self.bg, false);
        }
    }

    fn reverse_index(&mut self) {
        if self.cur_y == 0 {
            let n = self.rows - 1;
            self.cells.copy_within(0..n * self.cols, self.cols);
            for c in &mut self.cells[..self.cols] {
                *c = Cell::blank(self.fg, self.bg, false);
            }
        } else {
            self.cur_y -= 1;
        }
    }

    fn param(&self, i: usize, default: usize) -> usize {
        match self.params.get(i) {
            Some(&0) | None => default,
            Some(&v) => v as usize,
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        // 私有序列（`?`）：只认"显示/隐藏光标"，其余忽略（不做鼠标上报与完整 DSR）
        if self.private {
            self.params.clear();
            return;
        }
        match final_byte {
            b'A' => self.cur_y = self.cur_y.saturating_sub(self.param(0, 1)),
            b'B' => {
                let n = self.param(0, 1);
                self.cur_y = (self.cur_y + n).min(self.rows - 1);
            }
            b'C' => {
                let n = self.param(0, 1);
                self.cur_x = (self.cur_x + n).min(self.cols - 1);
                self.wrap_pending = false;
            }
            b'D' => {
                self.cur_x = self.cur_x.saturating_sub(self.param(0, 1));
                self.wrap_pending = false;
            }
            b'E' => {
                let n = self.param(0, 1);
                self.cur_y = (self.cur_y + n).min(self.rows - 1);
                self.cur_x = 0;
            }
            b'F' => {
                self.cur_y = self.cur_y.saturating_sub(self.param(0, 1));
                self.cur_x = 0;
            }
            b'G' | b'`' => {
                self.cur_x = (self.param(0, 1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            b'd' => self.cur_y = (self.param(0, 1) - 1).min(self.rows - 1),
            b'H' | b'f' => {
                self.cur_y = (self.param(0, 1) - 1).min(self.rows - 1);
                self.cur_x = (self.param(1, 1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            b'J' => self.erase_display(self.param(0, 0)),
            b'K' => self.erase_line(self.param(0, 0)),
            b'm' => self.sgr(),
            b'L' => {
                // 在光标行插入 n 行
                let n = self.param(0, 1).min(self.rows - self.cur_y);
                let start = self.cur_y * self.cols;
                self.cells.copy_within(start..(self.rows - n) * self.cols, start + n * self.cols);
                for c in &mut self.cells[start..start + n * self.cols] {
                    *c = Cell::blank(self.fg, self.bg, false);
                }
            }
            b'M' => {
                // 删除光标行起的 n 行
                let n = self.param(0, 1).min(self.rows - self.cur_y);
                let start = self.cur_y * self.cols;
                self.cells.copy_within((start + n * self.cols).., start);
                let tail = self.cells.len() - n * self.cols;
                for c in &mut self.cells[tail..] {
                    *c = Cell::blank(self.fg, self.bg, false);
                }
            }
            b'P' => {
                // 删除光标处 n 个字符，行内左移
                let n = self.param(0, 1).min(self.cols - self.cur_x);
                let row = self.cur_y * self.cols;
                self.cells.copy_within(row + self.cur_x + n..row + self.cols, row + self.cur_x);
                for x in (self.cols - n)..self.cols {
                    self.cells[row + x] = Cell::blank(self.fg, self.bg, false);
                }
            }
            b'X' => {
                // 擦除光标处 n 个字符（不移动）
                let n = self.param(0, 1).min(self.cols - self.cur_x);
                let row = self.cur_y * self.cols;
                for x in self.cur_x..self.cur_x + n {
                    self.cells[row + x] = Cell::blank(self.fg, self.bg, false);
                }
            }
            b'r' | b'h' | b'l' | b's' | b'u' | b't' | b'n' => {} // 滚动区/模式/应答：不支持
            _ => {}
        }
        self.params.clear();
    }

    fn erase_display(&mut self, mode: usize) {
        let n = self.cells.len();
        match mode {
            0 => {
                // 光标到末尾
                let start = self.cur_y * self.cols + self.cur_x;
                for c in &mut self.cells[start..n] {
                    *c = Cell::blank(self.fg, self.bg, false);
                }
            }
            1 => {
                let end = (self.cur_y * self.cols + self.cur_x + 1).min(n);
                for c in &mut self.cells[..end] {
                    *c = Cell::blank(self.fg, self.bg, false);
                }
            }
            _ => {
                for c in &mut self.cells {
                    *c = Cell::blank(self.fg, self.bg, false);
                }
            }
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let row = self.cur_y * self.cols;
        let (a, b) = match mode {
            0 => (self.cur_x, self.cols),
            1 => (0, (self.cur_x + 1).min(self.cols)),
            _ => (0, self.cols),
        };
        for x in a..b {
            self.cells[row + x] = Cell::blank(self.fg, self.bg, false);
        }
    }

    fn sgr(&mut self) {
        if self.params.is_empty() {
            self.sgr_reset();
            return;
        }
        // 取出参数（避免借用冲突），逐项应用后丢弃
        let params = std::mem::take(&mut self.params);
        for p in params {
            match p {
                0 => self.sgr_reset(),
                1 => self.bold = true,
                22 => self.bold = false,
                7 => std::mem::swap(&mut self.fg, &mut self.bg), // 反显：交换即可
                27 => {}
                30..=37 => self.fg = (p - 30) as u8,
                39 => self.fg = 7,
                40..=47 => self.bg = (p - 40) as u8,
                49 => self.bg = 0,
                90..=97 => self.fg = (p - 90 + 8) as u8,
                100..=107 => self.bg = (p - 100 + 8) as u8,
                _ => {}
            }
        }
        self.params.clear();
    }

    fn sgr_reset(&mut self) {
        self.fg = 7;
        self.bg = 0;
        self.bold = false;
    }

    pub fn reset(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
        self.cur_x = 0;
        self.cur_y = 0;
        self.sgr_reset();
        self.state = State::Ground;
        self.wrap_pending = false;
    }

    /// 整屏文本（测试与"复制全部"用；行尾空格已裁剪）。
    ///
    /// **跳过宽字符的占位格** —— 否则"中文"会被提取成"中 文"。
    pub fn to_lines(&self) -> Vec<String> {
        (0..self.rows)
            .map(|y| {
                let mut s = String::new();
                for x in 0..self.cols {
                    let c = self.cell(x, y);
                    if c.wide_tail {
                        continue;
                    }
                    s.push(c.ch);
                }
                while s.ends_with(' ') {
                    s.pop();
                }
                s
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(cols: usize, rows: usize, input: &str) -> Screen {
        let mut s = Screen::new(cols, rows);
        s.feed(input.as_bytes());
        s
    }

    #[test]
    fn plain_text_and_cursor() {
        let s = screen(10, 3, "abc");
        assert_eq!(s.to_lines()[0], "abc");
        assert_eq!((s.cur_x, s.cur_y), (3, 0));
    }

    #[test]
    fn crlf_moves_to_next_line() {
        let s = screen(10, 3, "ab\r\ncd");
        assert_eq!(s.to_lines()[..2], ["ab".to_string(), "cd".to_string()]);
        assert_eq!((s.cur_x, s.cur_y), (2, 1));
    }

    #[test]
    fn backspace_does_not_cross_lines() {
        let s = screen(10, 3, "ab\r\n\x08\x08x");
        // 退格在行首停下（不回到上一行）
        assert_eq!(s.to_lines()[1], "x");
    }

    #[test]
    fn newline_at_bottom_scrolls() {
        let s = screen(6, 2, "one\r\ntwo\r\nthree");
        // 第一行被滚出，剩下 two / three
        assert_eq!(s.to_lines(), vec!["two".to_string(), "three".to_string()]);
    }

    #[test]
    fn autowrap_moves_to_next_line() {
        let s = screen(4, 3, "abcdef");
        assert_eq!(s.to_lines()[..2], ["abcd".to_string(), "ef".to_string()]);
    }

    #[test]
    fn wide_char_occupies_two_cells() {
        let s = screen(10, 2, "中文");
        assert_eq!(s.cell(0, 0).ch, '中');
        assert!(s.cell(1, 0).wide_tail, "宽字符第二格必须是占位");
        assert_eq!(s.cell(2, 0).ch, '文');
        assert!(s.cell(3, 0).wide_tail);
        assert_eq!(s.to_lines()[0], "中文");
    }

    #[test]
    fn wide_char_wraps_instead_of_splitting() {
        // 3 列宽：一个宽字符占 2 格，剩下 1 格放不下第二个 → 折行
        let s = screen(3, 3, "中中");
        assert_eq!(s.cell(0, 0).ch, '中');
        assert_eq!(s.cell(0, 1).ch, '中', "第二个宽字符应整体换行");
    }

    #[test]
    fn utf8_split_across_feeds_is_reassembled() {
        let mut s = Screen::new(10, 2);
        let bytes = "中".as_bytes();
        s.feed(&bytes[..1]); // 半截
        s.feed(&bytes[1..]);
        assert_eq!(s.cell(0, 0).ch, '中');
    }

    #[test]
    fn control_char_interrupts_utf8_gracefully() {
        let mut s = Screen::new(10, 2);
        let bytes = "中".as_bytes();
        s.feed(&bytes[..1]);
        s.feed(b"\r\n"); // 打断序列
        s.feed(b"ok");
        assert_eq!(s.to_lines()[1], "ok");
    }

    #[test]
    fn sgr_sets_colors_and_bold() {
        let s = screen(10, 2, "\x1b[31;1mA");
        let c = s.cell(0, 0);
        assert_eq!(c.ch, 'A');
        assert_eq!(c.fg, 1);
        assert!(c.bold);
    }

    #[test]
    fn sgr_reset_restores_default() {
        let s = screen(10, 2, "\x1b[31;1mA\x1b[0mB");
        assert!(s.cell(0, 0).bold);
        let b = s.cell(1, 0);
        assert!(!b.bold);
        assert_eq!(b.fg, 7);
    }

    #[test]
    fn bright_colors_map_to_8_to_15() {
        let s = screen(10, 2, "\x1b[91mA\x1b[102mB");
        assert_eq!(s.cell(0, 0).fg, 9);
        assert_eq!(s.cell(1, 0).bg, 10);
    }

    #[test]
    fn clear_screen_and_home() {
        let s = screen(6, 3, "abc\x1b[2J\x1b[H");
        assert_eq!(s.to_lines(), vec!["".to_string(), "".to_string(), "".to_string()]);
        assert_eq!((s.cur_x, s.cur_y), (0, 0));
    }

    #[test]
    fn erase_to_end_of_line() {
        let s = screen(6, 2, "abcdef\x1b[1;3H\x1b[K");
        assert_eq!(s.to_lines()[0], "ab", "K 应擦掉光标起至行尾");
    }

    #[test]
    fn cursor_position_is_1_based() {
        let s = screen(10, 5, "\x1b[3;4HX");
        assert_eq!(s.cell(3, 2).ch, 'X');
    }

    #[test]
    fn cursor_movement_relative() {
        let s = screen(10, 5, "\x1b[3;3H\x1b[2A\x1b[1DX");
        // 从 (2,2) 上移 2 → 行 0；左移 1 → 列 1
        assert_eq!(s.cell(1, 0).ch, 'X');
    }

    #[test]
    fn cursor_movement_clamps_at_edges() {
        let s = screen(5, 3, "\x1b[99;99H\x1b[99BX");
        assert_eq!(s.cell(4, 2).ch, 'X', "越界的光标定位应夹到边界而不是 panic");
    }

    #[test]
    fn delete_and_erase_chars() {
        let s = screen(6, 2, "abcdef\x1b[1;2H\x1b[2P");
        assert_eq!(s.to_lines()[0], "adef", "P 应删掉光标处 2 个字符");
    }

    #[test]
    fn insert_lines_shifts_down() {
        let s = screen(4, 3, "aaa\r\nbbb\r\nccc\x1b[1;1H\x1b[LX");
        assert_eq!(s.to_lines(), vec!["X".to_string(), "aaa".to_string(), "bbb".to_string()]);
    }

    #[test]
    fn osc_title_is_swallowed() {
        let s = screen(10, 2, "\x1b]0;my title\x07ok");
        assert_eq!(s.to_lines()[0], "ok", "OSC 序列内容不应上屏");
    }

    #[test]
    fn osc_terminated_by_st() {
        let s = screen(10, 2, "\x1b]2;title\x1b\\ok");
        assert_eq!(s.to_lines()[0], "ok");
    }

    #[test]
    fn private_sequences_are_ignored_safely() {
        let s = screen(10, 2, "\x1b[?25lok");
        assert_eq!(s.to_lines()[0], "ok");
    }

    #[test]
    fn unknown_sequences_do_not_corrupt_state() {
        let s = screen(10, 2, "\x1b[999;888;777Zok");
        assert_eq!(s.to_lines()[0], "ok", "未知 CSI 应被完整吞掉");
    }

    #[test]
    fn charset_selection_is_swallowed() {
        let s = screen(10, 2, "\x1b(0ok");
        assert_eq!(s.to_lines()[0], "ok");
    }

    #[test]
    fn save_and_restore_cursor() {
        let s = screen(10, 3, "\x1b[2;3H\x1b7\x1b[1;1HX\x1b8Y");
        assert_eq!(s.cell(0, 0).ch, 'X');
        assert_eq!(s.cell(2, 1).ch, 'Y', "restore 后应回到保存的位置");
    }

    #[test]
    fn resize_preserves_overlap_and_clamps_cursor() {
        // 内容在左上角：缩到 4x2 后应保留
        let mut s = screen(10, 3, "ab\r\ncd\x1b[3;10HZ");
        // 光标此时在 (9,2)，缩到 4x2 后必须被夹住而不是越界
        s.resize(4, 2);
        assert_eq!((s.cols, s.rows), (4, 2));
        assert!(s.cur_x < s.cols && s.cur_y < s.rows);
        assert_eq!(s.to_lines(), vec!["ab".to_string(), "cd".to_string()]);
        // 夹住之后继续写入不能 panic、不能越过新边界
        s.feed(b"xy");
        assert!(
            s.cur_x < s.cols && s.cur_y < s.rows,
            "写入后光标仍在界内（cur=({}, {})，尺寸 {}x{}）",
            s.cur_x,
            s.cur_y,
            s.cols,
            s.rows
        );
    }

    #[test]
    fn resize_drops_content_outside_new_bounds() {
        // 右下角的内容在缩小后应当丢弃（不做重排），且不留残影到错误位置
        let mut s = screen(10, 3, "\x1b[3;9HZ");
        s.resize(4, 2);
        assert_eq!(s.to_lines(), vec!["".to_string(), "".to_string()]);
    }

    #[test]
    fn tab_advances_to_next_stop() {
        let s = screen(20, 2, "a\tb");
        assert_eq!(s.cell(8, 0).ch, 'b');
    }

    #[test]
    fn reverse_index_at_top_scrolls_down() {
        let s = screen(4, 3, "aaa\r\nbbb\r\nccc\x1b[1;1H\x1bMX");
        assert_eq!(s.to_lines()[0], "X");
        assert_eq!(s.to_lines()[1], "aaa");
    }
}
