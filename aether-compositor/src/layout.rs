//! 窗口布局引擎：平铺/自由两种范式，四种预设布局，
//! 布局切换的目标矩形计算，以及拖拽边缘吸附（左半/右半/最大化）。
//!
//! 这是未来 aetherd "把窗口排成两列" 指令的原生实现——AI 只需
//! 调用 Layout 切换，布局算法本身完全确定、可测试。

use crate::draw::Rect;

// 布局骨架尺寸的单一事实来源在 draw::theme::metric；这里按布局语义再导出
// （TOP_BAR = 菜单栏高度的工作区视角别名）。
pub use crate::draw::theme::metric::{BOTTOM_DOCK, GAP, MENUBAR_H as TOP_BAR};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// 自由布局：窗口保持手动位置
    Float,
    /// 两列平铺
    TwoCol,
    /// 三列平铺（中间主列）
    ThreeCol,
    /// 独占堆叠：全部占满工作区，点击置顶
    Monocle,
}

impl Layout {
    pub const ALL: [Layout; 4] = [Layout::Float, Layout::TwoCol, Layout::ThreeCol, Layout::Monocle];

    pub fn label(self) -> &'static str {
        match self {
            Layout::Float => "自由布局",
            Layout::TwoCol => "两列平铺",
            Layout::ThreeCol => "三列平铺",
            Layout::Monocle => "独占堆叠",
        }
    }
}

/// 工作区：菜单栏以下、Dock 以上。
pub fn work_area(w: usize, h: usize) -> Rect {
    Rect {
        x: 16,
        y: TOP_BAR + 10,
        w: w as i32 - 32,
        h: h as i32 - TOP_BAR - 10 - BOTTOM_DOCK - 12,
    }
}

/// 计算平铺布局下每个窗口的目标矩形（None = 不干预）。
/// 窗口数组顺序即主从优先级。
pub fn tiled_targets(n: usize, lay: Layout, work: Rect) -> Vec<Option<Rect>> {
    if n == 0 {
        return vec![];
    }
    match lay {
        // 独占堆叠：所有窗口占满工作区，靠点击置顶切换可见窗口
        Layout::Monocle => vec![Some(work); n],
        Layout::Float => vec![None; n],
        Layout::TwoCol => {
            let mut t = vec![None; n];
            let left = n.div_ceil(2);
            let cw = (work.w - GAP) / 2;
            stack_into(&mut t, 0..left, Rect { x: work.x, y: work.y, w: cw, h: work.h });
            stack_into(&mut t, left..n, Rect { x: work.x + cw + GAP, y: work.y, w: cw, h: work.h });
            t
        }
        Layout::ThreeCol => {
            let mut t = vec![None; n];
            if n == 1 {
                t[0] = Some(work);
                return t;
            }
            let side = ((work.w - 2 * GAP) as f32 * 0.28) as i32;
            let mid = work.w - 2 * GAP - 2 * side;
            // 第 0 个窗口居中主列，之后左右交替
            t[0] = Some(Rect { x: work.x + side + GAP, y: work.y, w: mid, h: work.h });
            let lefts: Vec<usize> = (1..n).step_by(2).collect();
            let rights: Vec<usize> = (2..n).step_by(2).collect();
            stack_into(&mut t, lefts.into_iter(), Rect { x: work.x, y: work.y, w: side, h: work.h });
            stack_into(&mut t, rights.into_iter(), Rect { x: work.x + side + GAP + mid + GAP, y: work.y, w: side, h: work.h });
            t
        }
    }
}

/// 把一批窗口索引垂直均分进一个区域。
fn stack_into(t: &mut [Option<Rect>], ids: impl Iterator<Item = usize>, area: Rect) {
    let ids: Vec<usize> = ids.collect();
    let k = ids.len() as i32;
    if k == 0 {
        return;
    }
    let each = (area.h - GAP * (k - 1)) / k;
    for (j, &i) in ids.iter().enumerate() {
        t[i] = Some(Rect {
            x: area.x,
            y: area.y + (each + GAP) * j as i32,
            w: area.w,
            h: each,
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Snap {
    None,
    Left,
    Right,
    Top,
}

impl Snap {
    pub fn label(self) -> &'static str {
        match self {
            Snap::None => "",
            Snap::Left => "吸附：左半屏",
            Snap::Right => "吸附：右半屏",
            Snap::Top => "吸附：最大化",
        }
    }
}

/// 根据拖拽中的指针位置判断吸附区。
pub fn detect(mx: f32, my: f32, w: usize, _h: usize) -> Snap {
    if mx < 28.0 {
        Snap::Left
    } else if mx > w as f32 - 28.0 {
        Snap::Right
    } else if my < 70.0 {
        Snap::Top
    } else {
        Snap::None
    }
}

pub fn rect(s: Snap, work: Rect) -> Rect {
    match s {
        Snap::None => work,
        Snap::Left => Rect { x: work.x, y: work.y, w: (work.w - GAP) / 2, h: work.h },
        Snap::Right => Rect { x: work.x + (work.w - GAP) / 2 + GAP, y: work.y, w: (work.w - GAP) / 2, h: work.h },
        Snap::Top => work,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Rect = Rect { x: 16, y: 52, w: 1248, h: 494 };

    #[test]
    fn two_col_covers_all() {
        let t = tiled_targets(5, Layout::TwoCol, W);
        assert_eq!(t.len(), 5);
        assert!(t.iter().all(|r| r.is_some()));
        // 左列 3 个，右列 2 个
        let lefts = t.iter().filter(|r| r.unwrap().x < W.x + W.w / 2).count();
        assert_eq!(lefts, 3);
    }

    #[test]
    fn three_col_master_center() {
        let t = tiled_targets(3, Layout::ThreeCol, W);
        let mid = t[0].unwrap();
        assert!(mid.w > t[1].unwrap().w && mid.w > t[2].unwrap().w);
    }

    #[test]
    fn snap_rects_partition_work() {
        let l = rect(Snap::Left, W);
        let r = rect(Snap::Right, W);
        assert_eq!(l.x + l.w + GAP, r.x);
        assert_eq!(l.h, W.h);
    }

    #[test]
    fn monocle_fills_work_area() {
        // 独占堆叠：每个窗口都占满工作区（P2-2 修复回归）
        let t = tiled_targets(3, Layout::Monocle, W);
        assert_eq!(t.len(), 3);
        for r in t {
            let r = r.unwrap();
            assert_eq!((r.x, r.y, r.w, r.h), (W.x, W.y, W.w, W.h));
        }
    }
}
