//! 虚拟桌面（FEATURE-GAPS §5.4 的 "macOS 的 Spaces" 那一半）。
//!
//! 贴边分屏（Aero Snap）本仓库早已实现（`layout::Snap` + 拖拽到边缘的预览），
//! 这里补的是**虚拟桌面**。
//!
//! ## 一条结构性保证
//!
//! **不在当前空间的窗口，根本不在 `Desktop::wins` 里** —— 它们被"停放"（park）在
//! 本模块的 `parked` 里。原因：输入处理、绘制、命中测试都会遍历 `desktop.wins`，
//! 若别的空间的窗口也在里面，就必须在**每一处**加"这个窗口属于当前空间吗"的过滤，
//! 而**漏一处**的后果是"看不见但点得到"——用户在 A 空间点到了 B 空间的窗口。
//! 让它们不在那个向量里，串台在结构上就不可能发生（不是"记得过滤"那种约定）。
//!
//! ## 为什么是泛型
//!
//! 本模块的规则（谁在哪个空间、切走后不丢、焦点怎么恢复）与"窗口是什么类型"无关。
//! 泛型化后这些规则能**脱离 `Win` 的构造**单独测 —— 测试里用 `Vec<u32>` 就够了。
//!
//! ## 停放着的窗口还活着
//!
//! 被停放的窗口只是不在当前空间的向量里，**没有销毁**：终端会话、音乐播放
//! 都继续跑（这是虚拟桌面的意义所在 —— 切走不是关掉）。

// 交互接线在 **Linux 的 fbdev 事件循环**里（`Super+1..4`），
// 因此非目标平台上这些方法只有测试在用 —— 与 `auth`/`lock`/`palette`/`policyview` 同一处理。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// 虚拟桌面集合。
#[derive(Debug, Clone)]
pub struct Spaces<W> {
    /// 下标 = 空间号。**当前空间那一项恒为空**（它的窗口在调用方的向量里）。
    parked: Vec<Vec<W>>,
    /// 每个空间切走前的焦点下标（切回来时恢复，而不是回到第一个）。
    saved_active: Vec<usize>,
    current: usize,
}

impl<W> Default for Spaces<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W> Spaces<W> {
    /// 空间数量。
    ///
    /// 固定 4 个而不是"可增删"：快捷键是 `Super+1..4`（与 Windows/macOS 的习惯一致），
    /// 而"增删空间"要一套管理界面。先把"切"和"搬"做扎实，数量留常量。
    pub const COUNT: usize = 4;

    pub fn new() -> Self {
        Self {
            parked: (0..Self::COUNT).map(|_| Vec::new()).collect(),
            saved_active: vec![0; Self::COUNT],
            current: 0,
        }
    }

    /// 当前空间（0 起）。
    pub fn current(&self) -> usize {
        self.current
    }

    pub fn count(&self) -> usize {
        Self::COUNT
    }

    /// 第 `i` 个空间里的窗口数。当前空间的要看调用方的向量（传进来）。
    pub fn len_of(&self, i: usize, current_len: usize) -> usize {
        if i == self.current {
            current_len
        } else {
            self.parked.get(i).map(|v| v.len()).unwrap_or(0)
        }
    }

    /// 切到第 `i` 个空间。返回是否真的切了。
    ///
    /// 做法是**交换**：当前向量 ⇄ `parked[current]`，再 `parked[i]` ⇄ 当前向量。
    /// 交换完 `parked[current]` 恰好为空（不变量，有测试盯着）。
    pub fn switch(&mut self, i: usize, wins: &mut Vec<W>, active: &mut usize) -> bool {
        if i >= Self::COUNT || i == self.current {
            return false;
        }
        self.saved_active[self.current] = *active;
        std::mem::swap(&mut self.parked[self.current], wins);
        self.current = i;
        std::mem::swap(&mut self.parked[i], wins);
        // 恢复这个空间上次的焦点；窗口少了就夹回去（不能指向不存在的窗口）
        *active = self.saved_active[i].min(wins.len().saturating_sub(1));
        true
    }

    /// 把当前窗口搬到第 `i` 个空间。返回是否真的搬了。
    ///
    /// 搬走的窗口**追加**到目标空间末尾（保持目标空间原有顺序）；
    /// 源空间的焦点随后夹回合法范围。
    pub fn move_active(&mut self, i: usize, wins: &mut Vec<W>, active: &mut usize) -> bool {
        if i >= Self::COUNT || i == self.current || wins.is_empty() || *active >= wins.len() {
            return false;
        }
        let w = wins.remove(*active);
        self.parked[i].push(w);
        // 源空间焦点：原来在中间的窗口被移走后，下标要么不动、要么夹到最后
        *active = (*active).min(wins.len().saturating_sub(1));
        true
    }

    /// 不变量的自检：当前空间那一格必须是空的。
    ///
    /// 提供成方法而不是只在测试里写：这是一条**每次操作后都该成立**的断言，
    /// 集成测试可以随时查（真出问题时能立刻指出是哪一步破坏的）。
    pub fn invariant_holds(&self) -> bool {
        self.parked[self.current].is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(ws: &[&[u32]]) -> (Spaces<u32>, Vec<u32>, usize) {
        // 造一个"当前空间有 ws[0] 个窗口、其余空间各有若干"的局面
        let mut sp: Spaces<u32> = Spaces::new();
        let mut wins: Vec<u32> = ws[0].to_vec();
        let mut active = 0usize;
        for (i, list) in ws.iter().enumerate().skip(1) {
            // 借切换来填充其它空间：先切过去，把窗口塞进去，再切回来
            sp.switch(i, &mut wins, &mut active);
            wins.extend_from_slice(list);
            sp.switch(0, &mut wins, &mut active);
        }
        (sp, wins, active)
    }

    #[test]
    fn starts_on_space_zero_and_keeps_the_invariant() {
        let sp: Spaces<u32> = Spaces::new();
        assert_eq!(sp.current(), 0);
        assert_eq!(sp.count(), 4);
        assert!(sp.invariant_holds());
        assert_eq!(sp.len_of(0, 0), 0);
    }

    #[test]
    fn switching_swaps_windows_and_restores_focus() {
        let (mut sp, mut wins, _) = setup(&[&[1, 2], &[7, 8, 9]]);
        assert_eq!(wins, vec![1, 2]);
        // 把焦点放到第二个窗口再切走 —— 切回来要回到它，而不是第一个
        let mut active = 1usize;
        assert!(sp.switch(1, &mut wins, &mut active));
        assert_eq!(sp.current(), 1);
        assert_eq!(wins, vec![7, 8, 9], "切过去应当看到那个空间的窗口");
        assert!(sp.invariant_holds());
        // 在那个空间里改焦点，再切回来
        active = 2;
        assert!(sp.switch(0, &mut wins, &mut active));
        assert_eq!(wins, vec![1, 2], "切回来应当看到原来的窗口");
        assert_eq!(active, 1, "焦点要恢复成切走前那个窗口");
        assert_eq!(sp.len_of(1, wins.len()), 3, "另一个空间仍有 3 个窗口");
    }

    /// **切来切去不能丢窗口**（这是虚拟桌面最容易出的错：实现成"清空再填充"就会丢）。
    #[test]
    fn windows_are_never_lost_by_switching() {
        let (mut sp, mut wins, mut active) = setup(&[&[1, 2], &[3], &[4, 5], &[6]]);
        let total = (0..sp.count())
            .map(|i| sp.len_of(i, wins.len()))
            .sum::<usize>();
        for round in 0..8 {
            let target = round % 4;
            sp.switch(target, &mut wins, &mut active);
            let now = (0..sp.count()).map(|i| sp.len_of(i, wins.len())).sum::<usize>();
            assert_eq!(now, total, "第 {round} 轮切换后窗口总数变了");
            assert!(sp.invariant_holds(), "第 {round} 轮切换后不变量被破坏");
        }
    }

    #[test]
    fn switching_to_the_same_or_out_of_range_space_is_a_noop() {
        let (mut sp, mut wins, mut active) = setup(&[&[1], &[2]]);
        assert!(!sp.switch(0, &mut wins, &mut active), "切到自己应当是空操作");
        assert!(!sp.switch(9, &mut wins, &mut active), "越界应当是空操作");
        assert_eq!(wins, vec![1]);
        assert_eq!(sp.current(), 0);
    }

    /// 搬窗口：从源空间消失、在目标空间出现（**追加**，不打乱目标原有顺序）。
    #[test]
    fn move_active_moves_the_window_to_another_space() {
        let (mut sp, mut wins, _) = setup(&[&[1, 2, 3], &[8, 9]]);
        let mut active = 1usize; // 搬走 2
        assert!(sp.move_active(1, &mut wins, &mut active));
        assert_eq!(wins, vec![1, 3], "源空间里不该还有它");
        assert_eq!(sp.len_of(1, wins.len()), 3, "目标空间多了一个");
        assert!(sp.invariant_holds());
        // 切过去看：它应当在**末尾**（不打乱目标原有顺序）
        sp.switch(1, &mut wins, &mut active);
        assert_eq!(wins, vec![8, 9, 2]);
    }

    /// 搬走最后一个窗口：源空间变空，切回去是空桌面（不 panic、不复活）。
    #[test]
    fn moving_the_last_window_leaves_an_empty_space() {
        let (mut sp, mut wins, mut active) = setup(&[&[1], &[2]]);
        assert!(sp.move_active(1, &mut wins, &mut active));
        assert!(wins.is_empty(), "源空间应当变空");
        assert_eq!(active, 0, "空空间的焦点下标只能是 0");
        assert!(sp.invariant_holds());
        sp.switch(1, &mut wins, &mut active);
        assert_eq!(wins, vec![2, 1]);
    }

    #[test]
    fn move_is_a_noop_when_it_makes_no_sense() {
        let (mut sp, mut wins, mut active) = setup(&[&[1]]);
        assert!(!sp.move_active(0, &mut wins, &mut active), "搬到自己应当是空操作");
        assert!(!sp.move_active(9, &mut wins, &mut active), "越界");
        // 空空间里没有"当前窗口"可搬
        let mut empty: Spaces<u32> = Spaces::new();
        let mut none: Vec<u32> = Vec::new();
        let mut a = 0usize;
        assert!(!empty.move_active(1, &mut none, &mut a));
        // 焦点下标越界（内部状态不一致）时不搬，也不 panic
        let (mut sp2, mut wins2, _) = setup(&[&[1, 2], &[3]]);
        let mut bad = 99usize;
        assert!(!sp2.move_active(1, &mut wins2, &mut bad));
    }

    /// 焦点恢复要**夹回**合法范围：切回来时那个空间的窗口可能被搬走过了。
    #[test]
    fn restored_focus_is_clamped_to_the_existing_windows() {
        let (mut sp, mut wins, _) = setup(&[&[1, 2, 3], &[9]]);
        let mut active = 2usize; // 焦点在第三个
        sp.switch(1, &mut wins, &mut active);
        // 在空间 1 里把 9 搬到空间 0（此时空间 0 停放 3 个窗口 + 1 个）
        let mut _x = 0;
        sp.move_active(0, &mut wins, &mut active);
        // 回到空间 0：它有 4 个窗口，焦点=2 仍合法
        sp.switch(0, &mut wins, &mut active);
        assert_eq!(active, 2);
        assert!(active < wins.len());
    }

    #[test]
    fn len_of_uses_the_current_vector_for_the_current_space() {
        let (sp, wins, _) = setup(&[&[1, 2], &[3, 4, 5]]);
        assert_eq!(sp.len_of(0, wins.len()), 2, "当前空间要看传进来的向量");
        assert_eq!(sp.len_of(1, wins.len()), 3, "其它空间看 parked");
        assert_eq!(sp.len_of(9, wins.len()), 0, "越界当成空");
    }
}
