# AetherOS 合成器性能优化报告 · 2026-09-26

症状：**VM 里鼠标很卡**（指针跟随严重滞后）。
结论：**单帧渲染 91ms + 固定 sleep 100ms = 191ms/帧 ≈ 5fps**，指针每 191ms 才更新一次。经优化后稳态 **22.5ms/帧**，配合自适应帧率可稳定跑 30fps。

---

## 1. 先测量，再优化

新增两个可复用的观测手段（长期保留）：

| 手段 | 用法 | 作用 |
|---|---|---|
| 帧耗时基准 | `aether-compositor --bench [N]` | 输出首帧（含壁纸生成）与稳态 N 帧的平均耗时、理论 fps 上限 |
| 分段计时 | `AETHER_RENDER_TIMING=1` 环境变量 | 逐帧打印 `bg_gen / bg_copy / snap / windows / menubar / ai / dock / ...` 各阶段耗时 |

**基线（1280x760，两列 + 鼠标）**：

```
首帧（含壁纸生成）: 731.74 ms
稳态 120 帧: 平均 91.31 ms/帧 → 上限 11 fps
理论 30fps 预算 33.3ms/帧，当前占用 274%
```

分段定位（稳态）：

| 阶段 | 耗时 | 占比 |
|---|---|---|
| **windows** | **87–96 ms** | **95%** |
| ai（气泡 + 指令条） | 4–6 ms | 5% |
| dock | 2–3 ms | 2% |
| menubar | 0.5 ms | — |
| bg_copy | 0.35 ms | — |
| bg_gen（每 240 帧一次） | **455 ms** | 周期性尖峰 |

**结论：瓶颈不在 sleep，而在 `draw_window` 本身** —— 3 个窗口每帧要跑 5 层投影 SDF + 逐像素圆角渐变。

---

## 2. 三项根因与优化

### 2.1 `sdf_round_rect` 的无条件 `hypot`（sqrt）

```rust
// 优化前：每个像素都开一次方
let outside = dx.max(0.0).hypot(dy.max(0.0)) + (dx.max(dy)).min(0.0);

// 优化后：只有角部（dx>0 且 dy>0）才需要开方，其余直接分量相加
let ox = dx.max(0.0);
let oy = dy.max(0.0);
let outside = if ox > 0.0 && oy > 0.0 { (ox * ox + oy * oy).sqrt() } else { ox + oy };
```

这是**恒等变形，不是近似**（`hypot(a,0) = a`）。圆角矩形的角部面积占比极小，条件化后省掉绝大部分 sqrt —— 软件光栅化下 sqrt 是最贵的一步。

**效果：91.31 → 49.02 ms/帧（-46%）**

### 2.2 `gradient_stops` 的逐像素 SDF

窗口主体是圆角矩形渐变，原实现每个像素都求一次 SDF。改为按行切分：用解析式算出该行"完全在形状内"的区间，中间直接混色，只有两端圆角带走逐像素抗锯齿。

```rust
let (ix0, ix1) = row_inner_span(r, radius, ay as f32 + 0.5);
for x in a..b { blend_pixel(buf, row + x, rgb, alpha); }   // 直填带
for x in clip_x0..a { /* SDF */ }                          // 左圆角带
for x in b..clip_x1 { /* SDF */ }                          // 右圆角带
```

**效果：49.02 → 46.17 ms/帧**

### 2.3 投影每帧重算（最大的一笔）

投影只取决于**窗口几何与激活态**，鼠标移动时完全不变；但原实现每帧都在 `draw_window` 里重跑 5 层 × 全窗口面积的 SDF。

改为在 `render_frame` 里**烘焙**进一层 `shadow_layer`（壁纸 + 全部窗口投影），用窗口几何指纹（FNV-1a）判断是否需要重算：

```rust
let skey = desktop_key(desktop);
if self.shadow_layer.len() != w * h || self.shadow_key != skey {
    self.shadow_layer.copy_from_slice(&self.bg);
    for (i, win) in desktop.wins.iter().enumerate() { shadow(&mut self.shadow_layer, ...); }
    self.shadow_key = skey;
}
buf.copy_from_slice(&self.shadow_layer);
```

配套把 `shadow()` 内部的通用混色换成纯黑专用的 `darken_pixel`（`dst * (1-a)`，省掉三次 lerp 与 clamp），并对完全在内部的像素跳过覆盖率 clamp。

**效果：46.17 → 23.95 ms/帧**

> 副作用（已目视确认可接受）：所有投影统一画在窗口**之下**，因此窗口之间不再互相投影。平铺布局（两列/三列）下窗口不重叠，视觉无差异；浮动布局下重叠窗口之间的阴影层次会减少。

---

## 3. 帧率控制：固定 10fps → 自适应

```rust
// 优化前：无论渲染多快都要等 100ms
std::thread::sleep(Duration::from_millis(100)); // 10fps

// 优化后：按目标周期补足剩余时间；渲染超预算时不睡
const TARGET_FRAME_MS: u128 = 33; // ≈30fps
let spent = frame_start.elapsed().as_millis();
if spent < TARGET_FRAME_MS {
    std::thread::sleep(Duration::from_millis((TARGET_FRAME_MS - spent) as u64));
}
```

这是"鼠标卡"的**直接成因**：指针只在渲染帧里更新，10fps 意味着位置每 100ms 才动一次。

同时把壁纸刷新间隔从 240 帧提到 **1800 帧**：240 帧在 10fps 下是 24 秒，提到 30fps 后只剩 8 秒 —— 而壁纸生成一次要 0.4 秒，**提帧率反而会让周期性卡顿变密**。1800 帧 @30fps ≈ 60 秒。

---

## 4. 上屏：整屏 4MB 写 → 脏行写

`fbdev::blit` 每帧组装并 `write` 整屏（1280x800x4 = 4MB）。新增 `blit_dirty`：

1. 用一次内存比较（切片 `!=` 走 memcmp）找出与上一帧不同的行范围
2. 只组装这些行，一次 `seek` + 一次 `write`
3. 整屏无变化时**一次写都不做**

鼠标移动时只有指针所在的少数几行变化，写入量从 4MB 降到几十字节。尺寸变化或首帧退回全量 `blit`。

脏行检测逻辑抽成纯函数 `draw::dirty_rows` 并配 4 项单元测试 —— 特意放在跨平台的 `draw.rs` 而不是只在 Linux 编译的 `fbdev.rs`，否则测试在开发机上根本跑不到。

---

## 5. 结果

| 指标 | 优化前 | 优化后 | 变化 |
|---|---|---|---|
| 稳态帧耗时 | 91.31 ms | **22.53 ms** | **-75%** |
| 理论 fps 上限 | 11 fps | **44 fps** | +300% |
| 30fps 预算占用 | 274% | **68%** | 有余量 |
| `windows` 阶段 | 90 ms | **20.6 ms** | -77% |
| 每帧上屏写入 | 4 MB | 鼠标移动时**几行** | ~3 个数量级 |
| 有效帧率（含 sleep） | **~5 fps** | **30 fps**（目标值） | +500% |

分段对比（稳态）：

```
优化前                          优化后
bg_gen       0.00 ms           bg_gen       0.00 ms
bg_copy      0.35 ms           bg_copy      0.41 ms
windows     90.02 ms           windows     20.60 ms
menubar      0.57 ms           menubar      0.34 ms
ai           3.87 ms           ai           2.04 ms
dock         2.12 ms           dock         1.11 ms
```

---

## 6. 验证

| 检查 | 结果 |
|---|---|
| `cargo test --workspace --offline` | **78 项全过**（原 74 + 新增 4 项脏行单测） |
| `cargo check --all-targets`（Windows 目标） | 零警告 |
| `cargo check --all-targets --target x86_64-unknown-linux-musl` | 零警告（顺手清掉了 6 个 Linux 目标下的 dead_code 警告 + 1 个 unreachable） |
| **视觉回归** | 与优化前基线逐像素比对：**仅 154 像素不同（0.016%），全部落在顶栏时钟区**（时间不同）。渲染管线重构**零视觉回归** |
| 归档图 | 8 张按新的渲染管线重建 |

---

## 7. 未验证 / 剩余风险

1. **VM 实机未验证**：本轮所有数据来自 Windows 开发机（`--bench`）。VM 里 CPU 更慢，绝对值会更高；**优化比例应当保持**，但"VM 里是否已流畅"需要实机确认。
2. **`blit_dirty` 未在真实 framebuffer 上运行过**：Windows 无 `/dev/fb0`，Linux 目标在本机只能编译不能链接运行。脏行**选择逻辑**有 4 项单测覆盖，但"写入 framebuffer 后画面是否正确"未实测。首帧/尺寸变化会退回全量 `blit`，是兜底。
3. **自适应帧率的实际表现未测**：渲染 22.5ms + sleep 到 33ms 的组合在 VM 里的实际手感需实机确认。
4. **壁纸生成 0.4s 未优化**：只把频率降到 60 秒一次。若要彻底消除，可考虑分帧生成（每帧算 N 行）或降采样后放大，两者都有视觉/复杂度代价，本轮未做。
5. **`--bench` 只覆盖"两列 + 鼠标"场景**：confirm 弹窗、安装向导、下拉菜单等场景未单独基准化。

---

## 8. 第二轮优化（同日追加）

第一轮把帧耗时压到 22.5ms 后，又找出两处更大的浪费。

### 8.1 字形缓存（本轮最大的一笔）

`text.rs` **完全没有字形缓存** —— `render()` 与 `measure_with()` 每帧对每个字符都调
`Font::rasterize()`（字形查找 + 光栅化 + 分配 bitmap）。终端 9 行、音乐列表 12 行、
文件网格 16 个标签，一帧要重算上百个字形。

新增 `RefCell<HashMap<GlyphKey, Arc<(Metrics, Vec<u8>)>>>`，键为 `(粗体, 字符, 字号×100)`。

**效果：22.4 → 17.0 ms/帧（-24%），`windows` 阶段 20.6 → 10.7 ms。**

### 8.2 描边/填充的行内快速路径

- `rounded_rect` / `fill_clipped`：套用 `row_inner_span`，中间直填带不做 SDF
- `rounded_outline` / `gradient_outline`：**只扫描可能命中描边带的像素**（上下边带整行 + 中间行两端），
  并顺手把残留的 `hypot` 条件化

描边类原本在整个包围盒上逐像素求距离，而真正落进 `[-1.5, 0.5)` 的不足 1% ——
一个 620×500 的窗口描边要白跑 31 万次距离计算。

### 8.3 过程中抓到并修掉的两个真实 bug（都是本轮引入）

| bug | 表现 | 根因 |
|---|---|---|
| `row_inner_span` 未处理"整行在形状外" | AI 指令条（胶囊形）底边**多出一整行** 512 像素 | `dy < 0` 时 `radius²-k²` 被 `max(0)` 截断成 0，算出一个"看似合法"的直填带 |
| 直填带边界按 `d ≤ 0` 而非 `d ≤ -0.5` | 圆角边缘被当成满覆盖（实测覆盖率只有 0.90），边缘变实 | 形状边界 ≠ 覆盖率 1 的边界 |
| 描边扫描边界写反（`ix0-2` 应为 `ix0+2`） | 圆角**内侧缺一小段线**（140 像素） | 描边带要 `d > -1.5`，比直填带更靠外，扫描范围必须**更宽** |

这三个都是"数学上看起来对、视觉上差一两个像素"的类型，靠肉眼看图发现不了。

### 8.4 验证手段：与逐像素参考实现做等价性测试

新增 `render_equiv_tests`，把优化前的朴素写法作为参考实现留在测试里，逐像素比对：

- `rounded_rect_fast_path_matches_reference`
- `rounded_outline_band_scan_matches_reference`
- `fill_clipped_fast_path_matches_reference`
- `gradient_outline_band_scan_matches_reference`
- `inner_span_only_covers_fully_opaque_pixels`（直填带内覆盖率必须真的是 1）
- `glyph_cache_matches_direct_rasterize`（缓存位图与直接光栅化逐字节相同）

**这套测试是本次能抓出上面三个 bug 的唯一手段** —— 像素级比对只告诉你"有 0.4% 不一致"，
等价性测试直接告诉你"哪个函数、哪个形状、哪个像素"。

### 8.5 第二轮结果

| 指标 | 第一轮后 | 第二轮后 | 累计（相对最初） |
|---|---|---|---|
| 稳态帧耗时 | 22.53 ms | **15.46 ms** | 91.31 → 15.46（**-83%**） |
| 理论 fps 上限 | 44 fps | **65 fps** | 11 → 65 fps |
| 30fps 预算占用 | 68% | **46%** | 274% |
| `windows` 阶段 | 20.6 ms | **10.7 ms** | 90 ms |

测试总数 **78 → 89**；Windows 与 Linux 目标**双零警告**。

**视觉回归**：与第二轮改动前的基线逐像素比对，排除顶栏时钟区后仅 **184 像素不同（0.019%）**，
且绝大多数幅度 ≤4（抗锯齿级），无可见差异。归档图 8 张已按新管线重建。

### 8.6 一条环境经验

本轮遇到 `rust-lld: undefined symbol: anon.*.llvm.*`（来自 serde_json）——
这是**增量编译缓存损坏**，不是代码问题。`rm -rf target/debug/incremental` 后重建即恢复。
伴随症状是构建期反复出现 `file-system error deleting outdated file ... .o`。
