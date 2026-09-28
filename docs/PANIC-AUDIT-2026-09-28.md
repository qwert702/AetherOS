# AetherOS 关键路径 panic 审计 · 2026-09-28

- **对应计划**：`docs/PRODUCTION-PLAN-2026-09-28.md` §3 的 **0.8**（本轮新增项）
- **范围**：`aether-compositor` 的 `draw.rs` / `input.rs` / `main.rs` / `layout.rs` / `term.rs`
- **方法**：静态审读（逐处索引运算的边界推导）+ 局部验证 + 补充回归测试
- **结果**：**发现并修复 1 个 P2（真实可触发的越界 panic）**，另有 2 处 P3 观察项

---

## 0. 为什么这个审计值得做

`platform/overlay/etc/aether/services/compositor.json`：

```json
{"name": "compositor", "deps": ["network", "aetherd"], "essential": true, "restart": false}
```

而 `aether-init/src/unit.rs:122` 的 `default_restart()` 返回 `true` —— **只有 compositor 显式关掉了重启**。

所以在当前配置下：**compositor 的任何一次 panic = 桌面永久死掉，需要手动重启整机。**

这改变了 panic 的严重级别：在普通程序里"偶发崩溃"是 P3，在这里是**服务不可用**。审计的目标不是"证明不可能崩"，而是**找出那些依赖隐式约定、没有显式夹取的索引**。

---

## 1. 逐点结论

### 1.1 已显式防护的（读代码即可确认）

| 位置 | 运算 | 防护 |
|---|---|---|
| `draw.rs:381` `blend_pixel` | `buf[idx]` | 首行 `if alpha <= 0.0 \|\| idx >= buf.len() { return; }` |
| `draw.rs:400` `darken_pixel` | `buf[idx]` | 同上 |
| `draw.rs:429` `fill_rect` | `y*w + x` | `rect.w <= 0 \|\| rect.h <= 0` 早退 + 两端 `.max(0)` + `.min(w/h)` |
| `draw.rs:904` `parent_of` | `t[..1]` / `t[..i]` / `t[..=i]` | `rfind` 返回的是 `/` 或 `\` 的字节索引（ASCII ⇒ 必为 UTF-8 边界） |
| `draw.rs:2472` 文件网格 | `entries[i]` | `i ∈ start..end`，而 `end = (start+per_page).min(entries.len())` |
| `draw.rs:2540` 预览 | `d.lines[i]` | `n = d.lines.len().min(max_lines)`，循环 `0..n` |
| `draw.rs:3231` BMP 输出 | `buf[y*w + x]` | `y < h`、`x < w`，且 `buf.len() == w*h`（同函数内分配） |
| `layout.rs:50` `tiled_targets` | `t[i]` | 全部 `i` 来自 `0..left` / `left..n` / `(1..n).step_by(2)` ⇒ 恒 `< n = t.len()` |
| `layout.rs:92` `stack_into` | `(area.h - GAP*(k-1)) / k` | `k == 0` 时提前 return ⇒ 不除零 |
| `main.rs:480` 点击置顶 | `wins[i]` / `wins[wi]` | `i ∈ 0..len`；`wi = len-1` 在 `push` 之后 ⇒ `len ≥ 1` |
| `main.rs:602/623/2332` | `tg[i]` | `tg.len() == wins.len()`（`tiled_targets` 对 `n>0` 返回长度 `n`） |
| `main.rs:1989` `apply_resize` | 窗口尺寸 | `MIN_W = 320` / `MIN_H = 200` 双端夹取，且末尾再 `.max(MIN_*)` |
| `main.rs:2150` `close_window` | `wins.remove(idx)` | 首行 `if idx >= desktop.wins.len() { return None; }` |
| `main.rs:2168` `toggle_zoom` | `wins.get_mut(idx)?` | 用 `get_mut` 而非索引 |
| `main.rs:2204` Tab 切焦点 | `wins[active]` | `len < 2` 时提前 return + `(active+1) % len` |
| `input.rs:143` evdev 解析 | `buf[..n/24*24]` | `n` 来自 `read(&mut buf)` ⇒ `n ≤ buf.len()` |
| `term.rs:163` 选区提取 | `screen.cell(x, y)` | 调用前对 `x`/`y` 做 `.min(cols-1)` / `.min(rows-1)` |
| `vt.rs`（上轮已验） | 多处 | 第四轮已逐条验算，全部有界 |

### 1.2 依赖"约定"但可接受的

| 位置 | 约定 | 说明 |
|---|---|---|
| `draw.rs:1624` 极光 | `bands[k]` / `centers[k*w+x]` / `envelopes[k*w+x]` | `nb = bands.len()`，且 `centers` / `envelopes` 按 `vec![0f32; w*nb]` 分配 —— 三者长度同源，安全 |
| `draw.rs:790` `sample_stops` | `stops[0]` / `stops[stops.len()-1]` | **函数本身不防御空数组**，但全部 2 个调用点都传非空（`gradient_tile` 传 2 元字面量；`draw_window` 传局部 `stops` 固定数组）。见 §2.2 |

### 1.3 发现的问题

见下节。

---

## 2. 发现

### 2.1 【P2 · 已修复】拖拽中的窗口索引会在其它路径关闭窗口后失效

**位置**：`main.rs:1643-1652`（拖拽分支）+ 两个"裸 pop"的关闭路径

**缺陷链**：

1. 拖拽分支直接用索引取窗口：

```rust
} else if let Some((i, lx, ly)) = drag {
    let w = &mut desktop.wins[i].rect;   // ← 无边界检查
```

2. 而 `close_active`（`apply_action`，AI 触发）与菜单「关闭窗口」是**裸 pop**：

```rust
// main.rs:1024（apply_action 的 close_active）
if desktop.wins.len() > 1 {
    desktop.wins.pop();
    desktop.active = desktop.wins.len() - 1;   // ← 只改 active，不清 drag
```

```rust
// main.rs:1190（run_menu_item 的「关闭窗口」）
if desktop.wins.len() > 1 {
    desktop.wins.pop();
    desktop.active = desktop.wins.len() - 1;   // ← 同样不清 drag
```

**触发条件**：用户按住鼠标拖拽**最后一个窗口**时，经上述任一路径关掉一个窗口。

- AI 路径（`close_active`）尤其现实：它是**异步**的 —— 用户在拖拽，AI 恰好回复并执行关闭动作；
- 菜单路径：拖拽中另一只手点菜单（或按 `Ctrl+W`）。

**后果**：下一帧 `desktop.wins[i]` 中 `i == 原 len - 1`，而 `wins.len()` 已减 1 ⇒ **越界 panic** ⇒ 因 `restart: false`，**桌面永久死掉**。

**定 P2 的理由**：触发需要"拖拽中"+"另一路径关窗"的时序巧合，日常使用不易撞上（**不是 P1**）；但一旦触发就是服务永久不可用，且用户完全无法自行恢复（**不是 P3**）。

**修复（3 处）**：

1. **拖拽分支加守卫**（纵深防御 —— 一处修所有关闭路径，将来新增 pop 也不会重演）：

```rust
if i >= desktop.wins.len() {
    drag = None;
    snap_zone = layout::Snap::None;
} else {
    let w = &mut desktop.wins[i].rect;
    ...
}
```

2. **两处裸 pop 统一改用 `close_window`** —— 它已有索引边界检查与更细致的 `active` 夹取（`if active > idx { active -= 1 }` + `.min(len-1)`），语义上也更一致（返回"已关闭「标题」"而不是笼统的"已关闭活动窗口"）。

**回归测试**（`main.rs` 的 `window_mgmt_tests`）：

```rust
#[test]
fn closing_last_window_invalidates_stale_drag_index() {
    let mut d = desk(3);
    d.active = 2;
    let stale = 2;                 // 拖拽开始时记下的索引
    let idx = d.active;
    close_window(&mut d, idx);
    assert_eq!(d.wins.len(), 2);
    assert!(d.active < d.wins.len(), "active 必须仍在界内");
    assert!(stale >= d.wins.len(), "旧拖拽索引应已失效，这正是要防的越界条件");
}
```

> 守卫条件本身在拖拽分支里（依赖主循环局部变量 `drag`），无法直接单测；这条测试锁住它的**前提**：关掉最后一个窗口后，旧索引确实不再小于 `len`。

**验证**：`cargo test --workspace --offline` **193 项全绿**（192 → 193）。

### 2.2 【P3 · 记录不修】`sample_stops` 不防御空数组

`draw.rs:790`：

```rust
fn sample_stops(stops: &[(f32, [u8; 3])], t: f32) -> [u8; 3] {
    if t <= stops[0].0 {          // 空数组 ⇒ 越界
        return stops[0].1;
    }
    ...
    stops[stops.len() - 1].1      // 空数组 ⇒ 下溢
}
```

**当前安全**：两个调用点都传非空（一个是 2 元字面量，一个是固定长度局部数组）。

**为什么不修**：这是私有函数（`fn` 而非 `pub fn`），调用点只有 2 个且都在同一文件；加防御分支会让"渐变必须至少有两个色标"这个契约变得隐晦。

**建议**（留给将来）：若调用点增至 3 个以上，加一行 `debug_assert!(!stops.is_empty())` —— 零运行时成本，且能在开发期抓住误用。

### 2.3 【P3 · 记录不修】极小窗口下会产生负尺寸的 `Rect`

`layout.rs:39`：

```rust
pub fn work_area(w: usize, h: usize) -> Rect {
    Rect { x: 16, y: TOP_BAR + 10, w: w as i32 - 32, h: h as i32 - TOP_BAR - 10 - BOTTOM_DOCK - 12 }
}
```

当 `h` 小于 `TOP_BAR + BOTTOM_DOCK + 22`（约 130px）时，`work.h` 为负。下游 `tiled_targets` 的 `cw = (work.w - GAP) / 2` 也可能为负。

**为什么不是 panic**：`fill_rect` 有 `rect.w <= 0 || rect.h <= 0` 早退，`blend_pixel` / `darken_pixel` 也有 `idx >= buf.len()` 检查。所以负尺寸只会**什么都不画**，不会崩。

**实际影响**：屏幕高度小于约 130px 时桌面内容区不渲染。这在真实显示器上不会发生（`WIDTH`/`HEIGHT` 是 1280×720），但如果将来支持窗口内嵌渲染或极小分辨率，会表现为"一片空白"。

**建议**：给 `work_area` 的返回值加 `.max(0)`，把"负尺寸"这个中间状态消灭在源头。

---

## 3. 方法上的诚实说明

1. **这是静态审读，不是形式化证明。** 每处结论都是"按当前代码推导其边界"，不是穷尽所有执行路径。
2. **部分结论依赖跨函数的长度约定**（例如"`buf.len() == w*h`"、"`tg.len() == wins.len()`"）。这些约定在代码里没有类型层面的强制 —— 本审计记录了它们，但没有把它们变成不可违反的形式。
3. **审计只覆盖了"索引越界 / 除零 / 下溢"三类 panic 源。** 未覆盖：`unwrap` 之外的 panic 宏、`Vec` 容量相关的 OOM、以及外部库内部的 panic。
4. `main.rs` 里剩余的 `unwrap()`（`tg[i].unwrap()` 三处）都在 §1.1 已确认有长度约定保护。

---

## 4. 未覆盖

| 项 | 原因 |
|---|---|
| `fbdev.rs`（fbdev 上屏、`blit_dirty`） | Linux-only，本机无法编译（缺 musl 交叉工具链），**仅能人工审读** |
| 真实拖拽/缩放时序 | 需要实机（`--shot` 无法模拟鼠标按住 + 并发的 AI 动作） |
| `text.rs` 的字形缓存并发 | 本轮未展开；它用 `RefCell` 而非 `Mutex`，单线程假设成立 |
| 崩溃恢复机制本身 | 属于门禁 1b 的另一条路径（给 compositor 开 `restart`），本轮只做了"排除已知 panic 源"这一半 |

---

## 5. 结论

- **compositor 的关键渲染路径（`draw.rs` 的像素/裁剪/渐变、`vt.rs` 的解析、`layout.rs` 的布局）在索引层面是干净的** —— 要么有显式夹取，要么有可推导的长度约定。
- **真正的风险不在"算错了"，而在"状态被另一条路径改了"** —— 本轮唯一的 P2 就是这个形态：拖拽持有索引，而关闭窗口的路径只维护 `active`、不维护 `drag`。**新增状态时必须同时检查所有会破坏它的路径**，这是这类缺陷的通用教训。
- 两处 P3 都不是缺陷，是"可以更防御"的地方，已记录理由与建议。

**验证基线**：`cargo test --workspace --offline` **193 项全绿**，代码零警告。
