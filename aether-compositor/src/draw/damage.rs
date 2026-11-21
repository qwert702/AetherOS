//! 脏矩形跟踪与局部上屏原语（G3）。
//!
//! 现状约束：合成器每帧仍**全量合成**到帧缓冲；本模块只提供两件可单测的原语：
//!
//! 1. [`Damage`]：状态变化（窗口移动/缩放、光标移动、菜单/通知弹出、主题或壁纸切换）
//!    时登记受影响的矩形；上屏路径据此把"整屏写回"降成"只写脏区"。
//! 2. [`present_full`] / [`present_rects`]：两种上屏拷贝路径。二者写入同一目标缓冲，
//!    等价性由 `random_sequence_matches_full` 测试逐字节断言。
//!
//! 不在本批范围：图层级的 retained-layer 局部重绘（脏区之外的老画面保留在离屏层，
//! 只对脏区重跑光栅）。当前光栅路径仍是整屏重画，脏区只作用于**上屏拷贝**这一步。

use super::Rect;

/// 一帧内积累的脏矩形集合。
///
/// 不做复杂的矩形合并/排斥覆盖：交互帧的脏区数量很小（光标 + 一两个窗口 + 一条
/// 菜单栏），列表直存 + 包含关系裁剪足够；重叠矩形在拷贝时重复写同一像素，结果不变。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    rects: Vec<Rect>,
}

impl Damage {
    /// 空脏区（本帧没有需要上屏的变化）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 整屏脏区（首帧、主题/壁纸切换、分辨率变化等退回全量的场景）。
    pub fn full_screen(w: usize, h: usize) -> Self {
        Self {
            rects: vec![Rect { x: 0, y: 0, w: w as i32, h: h as i32 }],
        }
    }

    // G3 保留 API：跨图层汇总 / 上屏路径选全量还是局部时使用，当前循环尚未接线。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// 清空脏区（每帧合成前调用）。
    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// 登记一个脏矩形；自动裁到屏幕、剔除空矩形，并做包含关系裁剪。
    pub fn mark(&mut self, r: Rect, screen: Rect) {
        let r = r.intersect(screen);
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        // 已被某个现存脏矩形完整覆盖 → 不必重复登记
        if self.rects.iter().any(|o| o.contains_rect(r)) {
            return;
        }
        // 新矩形覆盖了旧矩形 → 旧的可以删掉
        self.rects.retain(|o| !r.contains_rect(*o));
        self.rects.push(r);
    }

    /// 合并另一份脏区（跨窗口/跨图层汇总时用）。
    #[allow(dead_code)]
    pub fn merge(&mut self, other: &Damage, screen: Rect) {
        for &r in &other.rects {
            self.mark(r, screen);
        }
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// 全部脏矩形的外包矩形；空脏区返回 `None`。
    #[allow(dead_code)]
    pub fn bounds(&self) -> Option<Rect> {
        let mut it = self.rects.iter();
        let first = *it.next()?;
        let mut x0 = first.x;
        let mut y0 = first.y;
        let mut x1 = first.x + first.w;
        let mut y1 = first.y + first.h;
        for &r in it {
            x0 = x0.min(r.x);
            y0 = y0.min(r.y);
            x1 = x1.max(r.x + r.w);
            y1 = y1.max(r.y + r.h);
        }
        Some(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }

    /// 脏区像素数（近似：重叠矩形会重复计入，只用于占比/诊断，不用于精确预算）。
    pub fn dirty_pixels(&self) -> usize {
        self.rects
            .iter()
            .map(|r| (r.w.max(0) * r.h.max(0)) as usize)
            .sum()
    }
}

/// 全量上屏：把 `src` 整屏拷贝到 `dst`。两条缓冲等长（`w*h`）。
pub fn present_full(dst: &mut [u32], src: &[u32], w: usize, h: usize) {
    let n = (w * h).min(dst.len()).min(src.len());
    dst[..n].copy_from_slice(&src[..n]);
}

/// 局部上屏：只把 `src` 中落在脏矩形内的像素拷贝到 `dst`；其余像素保持不变。
///
/// 语义前提：`src` 是本帧完整合成好的画面，`dst` 是保留的上一帧画面；脏区是本帧
/// 发生变化区域的**超集**（旧位置 + 新位置都要登记）。这样没拷到的区域恰好没变过。
pub fn present_rects(dst: &mut [u32], src: &[u32], w: usize, h: usize, rects: &[Rect]) {
    let screen = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
    for &r in rects {
        let r = r.intersect(screen);
        if r.w <= 0 || r.h <= 0 {
            continue;
        }
        let y0 = r.y.max(0) as usize;
        let y1 = (r.y + r.h).min(h as i32).max(y0 as i32) as usize;
        let x0 = r.x.max(0) as usize;
        let x1 = (r.x + r.w).min(w as i32).max(x0 as i32) as usize;
        for y in y0..y1 {
            let row = y * w;
            dst[row + x0..row + x1].copy_from_slice(&src[row + x0..row + x1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
        fn below(&mut self, n: i32) -> i32 {
            if n <= 0 {
                0
            } else {
                (self.next() % n as u64) as i32
            }
        }
    }

    fn random_rect(rng: &mut Rng, screen: Rect, max_w: i32, max_h: i32) -> Rect {
        let w = 10 + rng.below(max_w);
        let h = 8 + rng.below(max_h);
        Rect {
            x: rng.below((screen.w - w).max(1)),
            y: rng.below((screen.h - h).max(1)),
            w,
            h,
        }
    }

    fn fill_scene(buf: &mut [u32], w: usize, widgets: &[Rect], cursor: Rect) {
        // 背景：固定中性色（跨步骤不变，这样"脏区之外"的像素确实没变过）
        let bg = 0x181a1e_u32;
        for p in buf.iter_mut() {
            *p = bg;
        }
        for (i, &r) in widgets.iter().enumerate() {
            let color = 0x2a6fb0_u32.wrapping_add((i as u32).wrapping_mul(0x101010));
            for y in r.y.max(0) as usize..(r.y + r.h).min(w as i32) as usize {
                for x in r.x.max(0) as usize..(r.x + r.w).min(w as i32) as usize {
                    buf[y * w + x] = color;
                }
            }
        }
        for y in cursor.y.max(0) as usize..(cursor.y + cursor.h).min(w as i32) as usize {
            for x in cursor.x.max(0) as usize..(cursor.x + cursor.w).min(w as i32) as usize {
                buf[y * w + x] = 0xffffff;
            }
        }
    }

    #[test]
    fn mark_clips_and_dedupes() {
        let screen = Rect { x: 0, y: 0, w: 100, h: 100 };
        let mut d = Damage::empty();
        // 越界矩形被裁到屏幕
        d.mark(Rect { x: -10, y: -10, w: 30, h: 30 }, screen);
        assert_eq!(d.rects(), &[Rect { x: 0, y: 0, w: 20, h: 20 }]);
        // 被包含的重复登记被剔除
        d.mark(Rect { x: 2, y: 2, w: 5, h: 5 }, screen);
        assert_eq!(d.rects().len(), 1);
        // 包含旧矩形的新登记替换旧的
        d.mark(Rect { x: 0, y: 0, w: 40, h: 40 }, screen);
        assert_eq!(d.rects(), &[Rect { x: 0, y: 0, w: 40, h: 40 }]);
        // 空矩形不登记
        d.mark(Rect { x: 0, y: 0, w: 0, h: 5 }, screen);
        assert_eq!(d.rects().len(), 1);
    }

    #[test]
    fn bounds_unions_all_rects() {
        let screen = Rect { x: 0, y: 0, w: 100, h: 100 };
        let mut d = Damage::empty();
        assert!(d.bounds().is_none());
        d.mark(Rect { x: 10, y: 10, w: 20, h: 20 }, screen);
        d.mark(Rect { x: 80, y: 60, w: 15, h: 15 }, screen);
        assert_eq!(
            d.bounds(),
            Some(Rect { x: 10, y: 10, w: 85, h: 65 })
        );
    }

    #[test]
    fn partial_present_leaves_untouched_pixels_alone() {
        let w = 16;
        let h = 16;
        let mut dst = vec![0x112233_u32; w * h];
        let src = vec![0xaabbcc_u32; w * h];
        let rects = [Rect { x: 4, y: 4, w: 6, h: 6 }];
        present_rects(&mut dst, &src, w, h, &rects);
        // 脏区内被改写，区外保持原值
        for y in 0..h {
            for x in 0..w {
                let inside = (4..10).contains(&x) && (4..10).contains(&y);
                assert_eq!(dst[y * w + x], if inside { 0xaabbcc } else { 0x112233 });
            }
        }
    }

    /// 随机状态序列：移动若干部件 + 光标，旧位/新位都进脏区。
    /// 参考路径整屏重画 + 全量上屏；对照路径只按脏区拷贝。每一步逐字节相等。
    #[test]
    fn random_sequence_matches_full() {
        let w = 96usize;
        let h = 64;
        let screen = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut rng = Rng(42);
        let n = 5;
        let mut pos: Vec<Rect> = (0..n)
            .map(|_| random_rect(&mut rng, screen, 20, 14))
            .collect();
        let mut cursor = Rect { x: 8, y: 8, w: 8, h: 8 };

        let mut scene = vec![0u32; w * h];
        let mut ref_fb = vec![0u32; w * h];
        let mut dam_fb = vec![0u32; w * h];

        for step in 0..200u64 {
            // 首帧保留缓冲是冷的：必须整屏上屏（与真实合成器首帧全量同一语义）。
            let mut damage = if step == 0 {
                Damage::full_screen(w, h)
            } else {
                Damage::empty()
            };
            // 随机移动 1–2 个部件
            let moved = 1 + rng.below(2) as usize;
            for _ in 0..moved {
                let i = rng.below(n) as usize;
                let old = pos[i];
                let new = random_rect(&mut rng, screen, 20, 14);
                damage.mark(old, screen);
                damage.mark(new, screen);
                pos[i] = new;
            }
            // 光标每步移动；旧位 + 新位都登记
            let old_cursor = cursor;
            cursor = Rect {
                x: rng.below(w as i32 - cursor.w),
                y: rng.below(h as i32 - cursor.h),
                w: cursor.w,
                h: cursor.h,
            };
            damage.mark(old_cursor, screen);
            damage.mark(cursor, screen);

            fill_scene(&mut scene, w, &pos, cursor);

            // 参考：全量
            present_full(&mut ref_fb, &scene, w, h);
            // 对照：局部脏区
            present_rects(&mut dam_fb, &scene, w, h, damage.rects());

            assert_eq!(
                ref_fb, dam_fb,
                "step {step}: 局部上屏与全量上屏像素不一致"
            );
        }
    }
}
