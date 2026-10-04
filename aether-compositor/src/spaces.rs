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
    /// **默认**空间数量（首次启动就是这么多）。
    pub const COUNT: usize = 4;

    /// 空间数量上限。
    ///
    /// 为什么有上限：切换靠 `Super+1..9`，超过 9 个就没有对应的键了；
    /// 8 是个够用又不会让快捷键与界面都变得拥挤的数字。
    pub const MAX: usize = 8;

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
        self.parked.len()
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
        if i >= self.count() || i == self.current {
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
        if i >= self.count() || i == self.current || wins.is_empty() || *active >= wins.len() {
            return false;
        }
        let w = wins.remove(*active);
        self.parked[i].push(w);
        // 源空间焦点：原来在中间的窗口被移走后，下标要么不动、要么夹到最后
        *active = (*active).min(wins.len().saturating_sub(1));
        true
    }

    /// 追加一个空空间。返回新空间的下标；已达上限则返回 `None`。
    ///
    /// 新空间一律**空**：从别处"复制"一个桌面没有明确的语义
    /// （是复制窗口？还是复制布局？），不做这种含糊的事。
    pub fn add_space(&mut self) -> Option<usize> {
        if self.count() >= Self::MAX {
            return None;
        }
        self.parked.push(Vec::new());
        self.saved_active.push(0);
        Some(self.count() - 1)
    }

    /// 删除第 `i` 个空间。返回是否删掉了。
    ///
    /// 三条规则（**都不是"顺手"能省的**）：
    /// 1. 至少保留一个空间；
    /// 2. **空间里还有窗口就不能删** —— 那会把窗口连同它的终端会话一起丢掉，
    ///    而用户点的是"删桌面"，不是"删掉里面的东西"；
    /// 3. 删**当前**空间时，只有它是空的才允许：然后切到前一个（或第 0 个）。
    pub fn remove_space(&mut self, i: usize, wins: &mut Vec<W>, active: &mut usize) -> bool {
        if self.count() <= 1 || i >= self.count() {
            return false;
        }
        if i == self.current {
            // 当前空间的窗口在这个向量里
            if !wins.is_empty() {
                return false;
            }
            let target = i.saturating_sub(1);
            // 先切过去（此时当前空间是空的，切换不会丢东西），再删
            if target == i || !self.switch(target, wins, active) {
                return false;
            }
            // 切换后，被删的那个空间的下标可能变了（删的是原来那个 i）
            let victim = i;
            self.parked.remove(victim);
            self.saved_active.remove(victim);
            return true;
        }
        // 非当前空间：它的窗口在 parked[i] 里
        if !self.parked[i].is_empty() {
            return false;
        }
        self.parked.remove(i);
        self.saved_active.remove(i);
        // 被删的空间在当前空间之前 → 当前下标要往前挪一格
        if i < self.current {
            self.current -= 1;
        }
        true
    }

    /// 停放中的窗口总数（诊断/测试用：切来切去不该丢窗口）。
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
    /// §5.4：**增删空间的三条规则**（每一条都对应一种"会丢东西"的可能）。
    #[test]
    fn adding_and_removing_spaces_follows_the_rules() {
        // 这个用例只加空间、不动窗口，所以 wins/active 不必可变、也不必命名
        let (mut sp, _, _) = setup(&[&[1]]);
        // 默认 4 个；加到上限
        assert_eq!(sp.count(), 4);
        assert!(sp.add_space().is_some());
        assert_eq!(sp.count(), 5);
        while sp.add_space().is_some() {}
        assert_eq!(sp.count(), Spaces::<u32>::MAX, "不该超过上限");
        assert!(sp.add_space().is_none(), "到上限后返回 None（而不是 panic 或无限加）");

        // **规则 1：不能少于 1 个** —— 逐个删到最后 1 个，再删就不行
        let (mut sp2, mut wins2, mut active2) = setup(&[&[]]);
        while sp2.count() > 1 {
            assert!(sp2.remove_space(sp2.count() - 1, &mut wins2, &mut active2));
        }
        assert_eq!(sp2.count(), 1);
        assert!(!sp2.remove_space(0, &mut wins2, &mut active2), "最后一个不能删");

        // **规则 2：非空空间不能删**（窗口会被连同终端会话一起丢掉）
        // 注意 `setup` 造的是**默认 4 个**空间（不是"数组长度"），所以删不掉时数量仍是 4。
        let (mut sp3, mut wins3, mut active3) = setup(&[&[1], &[7, 8]]);
        assert!(!sp3.remove_space(1, &mut wins3, &mut active3), "里面有窗口，不能删");
        assert_eq!(sp3.count(), 4, "删不掉就数量不变");
        assert_eq!(sp3.len_of(1, wins3.len()), 2, "窗口不该受影响");

        // 空空间可以删；删掉当前空间**之前**的那个，当前下标要跟着挪
        let (mut sp4, mut wins4, mut active4) = setup(&[&[1], &[], &[9]]);
        sp4.switch(2, &mut wins4, &mut active4);
        assert_eq!(sp4.current(), 2);
        assert!(sp4.remove_space(1, &mut wins4, &mut active4), "空空间可以删");
        assert_eq!(sp4.count(), 3, "4 个删掉 1 个 → 3 个");
        assert_eq!(sp4.current(), 1, "删掉前面的空间后，当前下标要往前挪");
        assert_eq!(wins4, vec![9], "当前空间的内容不该变");

        // **规则 3：删当前空间** —— 里面有窗口就不行；空的可以（并自动切走）
        let (mut sp5, mut wins5, mut active5) = setup(&[&[1], &[]]);
        sp5.switch(1, &mut wins5, &mut active5);
        assert!(wins5.is_empty(), "前提：当前空间是空的");
        assert!(sp5.remove_space(1, &mut wins5, &mut active5), "空的当前空间可以删");
        assert_eq!(sp5.count(), 3);
        assert_eq!(sp5.current(), 0, "删掉当前空间后要切到还存在的那个");

        let (mut sp6, mut wins6, mut active6) = setup(&[&[1], &[7]]);
        sp6.switch(1, &mut wins6, &mut active6);
        assert!(!sp6.remove_space(1, &mut wins6, &mut active6), "当前空间有窗口，不能删");
        assert_eq!(wins6, vec![7], "窗口必须还在");

        // 越界与删自己（只有一个空间时）都返回 false 而不是 panic
        let (mut sp7, mut wins7, mut active7) = setup(&[&[1], &[2]]);
        assert!(!sp7.remove_space(9, &mut wins7, &mut active7));
    }

    /// 增删之后，切来切去**仍然不丢窗口**（不变量也要继续成立）。
    #[test]
    fn windows_survive_add_and_remove_cycles() {
        let (mut sp, mut wins, mut active) = setup(&[&[1, 2], &[3], &[4, 5]]);
        let total = (0..sp.count()).map(|i| sp.len_of(i, wins.len())).sum::<usize>();
        assert_eq!(total, 5);
        for _ in 0..3 {
            sp.add_space();
            assert!(sp.invariant_holds());
            sp.switch(sp.count() - 1, &mut wins, &mut active);
            assert!(sp.invariant_holds());
            assert!(sp.remove_space(sp.current(), &mut wins, &mut active));
            assert!(sp.invariant_holds());
            let now = (0..sp.count()).map(|i| sp.len_of(i, wins.len())).sum::<usize>();
            assert_eq!(now, total, "增删空间不该丢窗口");
        }
    }

}
