//! 文本视图的共享状态：光标与滚动（2.4 统一文本交互）。
//!
//! 起因：三处文本界面此前各写一套 ——
//! - **输入框**只能在末尾追加（没有光标，中间打错只能全删重来）；
//! - **预览窗口**只能看前 N 行（长文件翻不动）；
//! - **终端**的光标与选择由 VT 解析器驱动。
//!
//! 这个模块把**能共享的部分**抽出来：
//!
//! - [`TextCursor`]：可编辑文本的光标（输入框用）。按**字符**索引而非字节 ——
//!   中文是多字节，按字节切会把字符劈成两半。
//! - [`ScrollView`]：只读长文本的滚动偏移（预览用）。夹取逻辑只有一份。
//! - [`nav_action`]：导航键 → 语义动作。**只有这一份映射**，三处各写一套必然漂移。
//!
//! **终端不在此列**：它的光标是"远端程序说什么就是什么"（由 ANSI 序列驱动），
//! 与本地编辑语义不同。强行统一只会两头不讨好 —— 这是有意的范围划定。

use crate::input::NavKey;

/// 可编辑文本的光标。按**字符**索引（不是字节）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextCursor {
    pos: usize,
}

impl TextCursor {
    /// 当前光标位置（第几个字符之前）。
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn left(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    pub fn right(&mut self, len: usize) {
        self.pos = (self.pos + 1).min(len);
    }

    pub fn home(&mut self) {
        self.pos = 0;
    }

    pub fn end(&mut self, len: usize) {
        self.pos = len;
    }

    /// 在光标处插入字符，光标后移一位。
    pub fn insert(&mut self, text: &mut String, ch: char) {
        let at = byte_index(text, self.pos);
        text.insert(at, ch);
        self.pos += 1;
    }

    /// 删除光标**前**一个字符（Backspace）。返回是否有东西被删。
    pub fn backspace(&mut self, text: &mut String) -> bool {
        if self.pos == 0 {
            return false;
        }
        let end = byte_index(text, self.pos);
        let start = byte_index(text, self.pos - 1);
        text.replace_range(start..end, "");
        self.pos -= 1;
        true
    }

    /// 删除光标**处**的字符（Delete）。返回是否有东西被删。
    pub fn delete(&mut self, text: &mut String) -> bool {
        if self.pos >= text.chars().count() {
            return false;
        }
        let start = byte_index(text, self.pos);
        let end = byte_index(text, self.pos + 1);
        text.replace_range(start..end, "");
        true
    }
}

/// 第 `char_idx` 个字符的**字节**偏移（越界则取串长）。
///
/// 中文是多字节字符，`String::insert` / `replace_range` 要的是字节位置；
/// 直接拿字符序号当字节位置会在第一个非 ASCII 字符处 panic。
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// 只读长文本的滚动偏移（以"行"为单位）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollView {
    offset: usize,
}

impl ScrollView {
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// 夹取到 `[0, total - visible]`。`total <= visible` 时归零。
    ///
    /// 独立成方法是因为它**每帧都要跑**：窗口缩放后原来的偏移可能已越界，
    /// 而"越界"在这里不会 panic（渲染时按 `offset..offset+visible` 取，
    /// 取不到就不画），只会表现为空白 —— 但用户看到的是"内容不见了"。
    pub fn clamp(&mut self, total: usize, visible: usize) {
        self.offset = self.offset.min(max_offset(total, visible));
    }

    /// 按行滚动（负数向上）。
    pub fn line(&mut self, delta: isize, total: usize, visible: usize) {
        let max = max_offset(total, visible);
        self.offset = if delta < 0 {
            self.offset.saturating_sub(delta.unsigned_abs())
        } else {
            (self.offset + delta as usize).min(max)
        };
    }

    /// 按页滚动。
    pub fn page(&mut self, down: bool, total: usize, visible: usize) {
        let step = visible.max(1) as isize;
        self.line(if down { step } else { -step }, total, visible);
    }

    pub fn home(&mut self) {
        self.offset = 0;
    }

    pub fn end(&mut self, total: usize, visible: usize) {
        self.offset = max_offset(total, visible);
    }
}

fn max_offset(total: usize, visible: usize) -> usize {
    total.saturating_sub(visible.max(1))
}

/// 文本视图的导航语义。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextNav {
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
}

/// 导航键 → 语义动作。**只有这一份实现** —— 三处各写一套必然漂移。
///
/// 返回 `None` 表示该键不属于文本导航（`Up`/`Down`/`Tab` 各有各的用途：
/// 列表移动、焦点轮换）。
pub fn nav_action(key: NavKey) -> Option<TextNav> {
    use NavKey::*;
    Some(match key {
        Left => TextNav::Left,
        Right => TextNav::Right,
        Home => TextNav::Home,
        End => TextNav::End,
        PageUp => TextNav::PageUp,
        PageDown => TextNav::PageDown,
        Delete => TextNav::Delete,
        Up | Down | Tab => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_moves_are_clamped() {
        let mut c = TextCursor::default();
        c.left(); // 已在开头，不应下溢
        assert_eq!(c.pos(), 0);
        c.right(3);
        c.right(3);
        c.right(3);
        c.right(3);
        assert_eq!(c.pos(), 3, "右移不能超过长度");
        c.end(3);
        assert_eq!(c.pos(), 3);
        c.home();
        assert_eq!(c.pos(), 0);
    }

    #[test]
    fn insert_at_cursor_not_at_end() {
        let mut s = String::from("ac");
        let mut c = TextCursor::default();
        c.right(2); // 光标在 a 之后
        c.insert(&mut s, 'b');
        assert_eq!(s, "abc", "应插在光标处而不是末尾");
        assert_eq!(c.pos(), 2, "光标应随插入后移");
    }

    /// 中文是多字节：按字节索引会切坏字符甚至 panic。
    #[test]
    fn insert_and_delete_handle_multibyte() {
        let mut s = String::from("中文");
        let mut c = TextCursor::default();
        c.end(2);
        c.insert(&mut s, '！');
        assert_eq!(s, "中文！");

        // 删掉"！"，再删"文"
        assert!(c.backspace(&mut s));
        assert_eq!(s, "中文");
        assert!(c.backspace(&mut s));
        assert_eq!(s, "中", "退格应删掉一个完整字符");
        assert_eq!(c.pos(), 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut s = String::from("x");
        let mut c = TextCursor::default();
        assert!(!c.backspace(&mut s));
        assert_eq!(s, "x");
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut s = String::from("abc");
        let mut c = TextCursor::default();
        assert!(c.delete(&mut s));
        assert_eq!(s, "bc");
        assert_eq!(c.pos(), 0, "Delete 不移动光标");
        c.end(2);
        assert!(!c.delete(&mut s), "末尾 Delete 应无事发生");
    }

    #[test]
    fn delete_at_end_is_noop() {
        let mut s = String::from("中文");
        let mut c = TextCursor::default();
        c.end(2);
        assert!(!c.delete(&mut s));
        assert_eq!(s, "中文");
    }

    #[test]
    fn scroll_clamps_to_content() {
        let mut v = ScrollView::default();
        v.line(100, 10, 3);
        assert_eq!(v.offset(), 7, "最大偏移 = total - visible");
        v.line(-100, 10, 3);
        assert_eq!(v.offset(), 0, "不能滚到负数");
    }

    #[test]
    fn scroll_when_content_fits_is_zero() {
        let mut v = ScrollView::default();
        v.line(5, 2, 10);
        assert_eq!(v.offset(), 0, "内容装得下就不该有偏移");
    }

    #[test]
    fn scroll_page_and_home_end() {
        let mut v = ScrollView::default();
        v.page(true, 100, 10);
        assert_eq!(v.offset(), 10, "翻一页 = 一个可视高度");
        v.end(100, 10);
        assert_eq!(v.offset(), 90);
        v.home();
        assert_eq!(v.offset(), 0);
    }

    /// 窗口缩小后原偏移可能越界 —— `clamp` 要把它拉回来。
    #[test]
    fn clamp_after_resize() {
        let mut v = ScrollView::default();
        v.end(100, 10);
        assert_eq!(v.offset(), 90);
        v.clamp(100, 50); // 窗口变高，可见 50 行
        assert_eq!(v.offset(), 50);
        v.clamp(5, 10); // 内容比可视区还少
        assert_eq!(v.offset(), 0);
    }

    #[test]
    fn nav_action_maps_only_text_keys() {
        use NavKey::*;
        assert_eq!(nav_action(Left), Some(TextNav::Left));
        assert_eq!(nav_action(PageDown), Some(TextNav::PageDown));
        assert_eq!(nav_action(Delete), Some(TextNav::Delete));
        assert_eq!(nav_action(Up), None, "上下键归列表移动，不是文本导航");
        assert_eq!(nav_action(Tab), None, "Tab 归焦点轮换");
    }
}
