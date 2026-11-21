# draw 模块拆分与脏矩形局部重绘（E1 / G3）实现状态报告

- 日期：2026-11-21
- 工作包：`aether-compositor`
- 范围：`aether-compositor/src/draw.rs`、`aether-compositor/src/draw/*.rs`、`aether-compositor/src/fbdev.rs`、`aether-compositor/src/app_main.rs`、`scripts/damage-bench.py`
- 状态口径：本报告严格区分 **代码已实现** / **Linux 副本已编译跑测** / **Windows 已测**，三者不混写；未跑通的门禁如实标注原因。

---

## 0. 交付位置与回灌状态（务必先读）

**改动位于云主机 /tmp/aether/src3，尚未回灌到 QYY 项目工作树；QYY 恢复后需应用 delta 包并重跑 test/shot-diff。**

- 云主机（vm, Linux）工作副本：`/tmp/aether/src3`。
- 真实工作树（QYY Windows，编写本报告时离线）：`D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether`。
- 回灌载体（与代码同目录）：
  - `draw-damage-delta.tar.gz`：仅本包允许清单内的新增 / 修改源码与脚本（相对项目根路径）。
  - `draw-damage-delta.manifest.txt`：逐文件标注新增 / 修改，写明原始行数与拆分后行数。
- 红线：本包**未修改** `aether-compositor/src/wayland/**`（另一工作流在同一副本内做 SCM_RIGHTS fd 传递），也未改 `docs/README.md`、`.github/`、`Cargo.toml`、`Cargo.lock`。
- 本报告中"已实现"指代码已落盘并在 Linux 副本上编译通过、测试通过；**不等于** Windows 已编译通过，也**不等于**真机 fbdev 局部上屏已验证。

### 共享副本上的在途阻断（与本包无关，回灌时不要携带）

- 工作期间，副本内另一工作流的 `wayland/fd.rs` 在途代码曾出现 `E0603`（`Received` 误从 `imp` 再导出），导致整 crate 编译一度被阻断。按红线本包未改 `wayland/**`。
- 最终验证前，该阻断已由另一工作流在 `src3` 内自行解除；本包最终的编译 / 测试 / clippy 直接在 `src3` 副本上完成，未再使用临时修正副本。`src3` 内的 `wayland/**` 不属于本包，不进 delta。

---

## 1. 交付概览

| 工作项 | 目标 | 代码状态 | vm 副本验证 |
|---|---|---|---|
| E1 | 把 `draw.rs` 按绘制域拆成 `draw/` 子模块；纯搬迁、零逻辑改动、外部 `draw::X` 导出不变 | 已实现 | 编译通过；compositor 测试全绿 |
| G3 | fbdev 软件光栅路径上的脏矩形跟踪 + 局部重绘 / 局部上屏原语；像素等价性测试；可复跑基准 | 原语与测试已实现；实时循环仍走既有 `blit_dirty`，局部上屏通路保留待接线 | 4 个单测通过；基准模式实跑取数 |

---

## 2. E1：draw.rs 模块拆分

### 2.1 拆分前后行数

| 文件 | 状态 | 原始行数 | 拆分后行数 | 归属 |
|---|---|---:|---:|---|
| `aether-compositor/src/draw.rs` | 修改 | 5369 | 2498 | 保留 `Renderer` struct + 6 个 impl 块 + 3 个内联测试模块（span / render_equiv / blit）+ 模块声明与再导出 |
| `draw/primitives.rs` | 新增（自 draw.rs 拆出） | 0 | 549 | 图元绘制；`Rect::intersect` / `Rect::contains_rect`；`write_bmp`；`dirty_rows` |
| `draw/wallpaper.rs` | 新增（自 draw.rs 拆出） | 0 | 88 | 壁纸分帧生成 |
| `draw/files.rs` | 新增（自 draw.rs 拆出） | 0 | 265 | `FsEntry` / 路径工具 / `PreviewData` / `read_preview` / 面包屑 / 预览可见行 |
| `draw/desk.rs` | 新增（自 draw.rs 拆出） | 0 | 379 | 桌面 / 窗口 / 浮层状态类型：`FileView`、`Desktop`、`WinKind`、`Win`、`WaylandSurface`、`BubbleKind`、`AiStatus`、`ConfirmUi`、`UiState`、`InstallerPhase`、`InstallerUi`、`ConfirmButton`、`ControlHit`、`PageCtx` 等 |
| `draw/icons.rs` | 新增（自 draw.rs 拆出） | 0 | 279 | 应用图标 / 标题箭头 / 箭头 / 浅色符号 / 文件夹与文件图标 / 光标 |
| `draw/overlays.rs` | 新增（自 draw.rs 拆出） | 0 | 549 | 锁屏 / 命令面板 / 首启向导 / 文本换行 / 输入法候选 / 可见窗口 / 回复气泡 / 工具标签 / toast / 性能 HUD |
| `draw/content.rs` | 新增（自 draw.rs 拆出） | 0 | 879 | 桌面按键 / 窗口绘制 / 终端 / 音乐 / 文件 / 预览 / Wayland 内容面 / 图标网格 / `GridLayout` 与布局几何 |
| `draw/theme.rs` | 修改 | 633 | 645 | 仅扩源码探针：尺寸常量使用断言覆盖 `draw/` 下全部同级文件 |
| `draw/settings.rs` | 未改 | 1817 | 1817 | — |
| `draw/control_layout.rs` | 未改 | 39 | 39 | — |

> 说明：拆出子模块的"原始行数"无法从单文件基线逐块归因，表内记 0 表示该文件为新文件、内容自原 `draw.rs` 搬迁；搬迁不含逻辑改写，仅做可见性提升（被搬条目在子模块内提为 `pub` / `pub(crate)`）与导入继承（子模块顶部 `use super::*;`，沿用既有 `settings.rs` / `control_layout.rs` 模式）。

### 2.2 外部导出不变

- 父 `draw.rs` 对每个拆出模块用 `pub use <mod>::*;` 再导出，外部 `draw::X` 调用点不改。
- 既有声明保留在 `draw.rs` 头部：`pub mod theme;`、`pub mod control_layout;`、`mod settings;` + `pub use settings::draw_settings_content;`。
- 编译与既有测试（含 `render_equiv_tests` 逐像素等价断言、`blit_tests`、`span_tests`）在拆分后全部通过，作为"纯搬迁、零逻辑改动"的回归证据。

### 2.3 拆分过程中处理过的约束

- 子模块不能命名 `state`：与父级 `use self::theme::{..., state}` 重名（E0255），改名为 `desk`。
- `theme.rs` 的 `every_size_constant_is_actually_used` 源码探针原先只扫 `draw.rs` + `settings.rs`；条目搬到新子模块后会漏扫误报，已把探针扩到 `draw/` 全部同级文件。

---

## 3. G3：脏矩形跟踪与局部上屏

### 3.1 模块 `draw/damage.rs`（新增，291 行）

- **`Damage`**：一帧内积累的脏矩形集合。列表直存 + 包含关系裁剪，不做复杂矩形合并；重叠矩形在拷贝时重复写同一像素，结果不变。
  - `empty()` / `full_screen(w, h)`：空脏区 / 整屏脏区（首帧、主题或壁纸切换、分辨率变化退回全量）。
  - `mark(r, screen)`：登记矩形，自动裁到屏幕、剔除空矩形、做包含关系裁剪（已被覆盖则跳过；覆盖旧矩形则替换旧的）。
  - `merge(other, screen)`：跨窗口 / 跨图层汇总。
  - `rects()` / `bounds()` / `dirty_pixels()`：取表 / 外包矩形 / 脏区像素数（近似，重叠重复计入，仅用于占比诊断）。
- **`present_full(dst, src, w, h)`**：全量上屏，整屏拷贝。
- **`present_rects(dst, src, w, h, rects)`**：局部上屏，只把脏矩形内像素从 `src` 拷到 `dst`，其余像素保留。语义前提：`src` 是本帧完整合成画面，`dst` 保留上一帧；脏区是本帧变化区域的**超集**（旧位置 + 新位置都要登记）。
- 两条上屏路径写入同一目标缓冲，等价性由 `random_sequence_matches_full` 测试逐字节断言。

### 3.2 Renderer 接线（`draw.rs`）

- `Renderer` 新增 `damage: Damage` 字段；`mark_damage(r, w, h)` 登记脏区；`take_damage()` 取走本帧脏区并清空。
- `render_frame` 每帧开头清脏区；背景失效（`bg_dirty`）或尺寸变化记整屏。
- 窗口循环登记各窗口矩形；循环之后登记两条持久 UI 条带：顶部菜单栏条带（`y=0, h=MENUBAR_H`）与底部 Dock / AI 条带（`y=h-BOTTOM_DOCK, h=BOTTOM_DOCK`）。
- 模态 / 浮层打开时（`control_open`、`open_menu`、`setup.open`、`palette.open`、`show_installer`、`confirm`、`snap` 任一成立）背景被压暗、整屏构图可能变，直接把脏区置为整屏。

### 3.3 fbdev 后端（`fbdev.rs`，405 → 494 行）

- 新增 `blit_rects(buf, w, h, rects)`：行粒度局部上屏。与任一脏矩形相交的行才重新组装并写回；x 方向精裁剪在跨平台原语 `present_rects` 内，本后端通路与之像素等价。
- **实时循环保持既有 `blit_dirty` 不变**（行脏区 memcmp + `prev` 基线），`blit_rects` 作为已编译、已配等价测试的上屏原语保留，**未接入默认循环**。
  - 依据：菜单栏 / Dock / AI 条 / 光标的逐帧脏区登记若不完整，真机上会留残影；本机无法验证真实 `fb0`，贸然切换通路风险不可控。
  - 注意：`blit_dirty` 维护 `prev` 基线，`blit_rects` 不更新 `prev`，两条通路不能混用。

### 3.4 基准模式（`app_main.rs`，2696 → 2745 行）

- `--damage-bench [N]`（N 默认 200）：在 1920×1080、3840×2160 × 100%、150% 四格上，对代表性脏区（顶部菜单栏条带 h=28 + 一个 620×500×k 窗口 + 24×24 光标）对比 `present_full` 与 `present_rects` 的软件缓冲拷贝耗时，输出脏区像素数、占比与局部 / 全量倍率。
- 包装脚本：`scripts/damage-bench.py`（`python scripts/damage-bench.py [N]`）。
- 基准只测内存帧缓冲拷贝，**不含**真实 fbdev 写回带宽。

---

## 4. 取舍与已知限制（代码现状，不是缺陷掩盖）

1. **未做 retained-layer 局部重绘**：当前光栅路径仍是每帧整屏重画到帧缓冲；脏区只作用于**上屏拷贝**这一步。图层级脏区之外保留离屏层、只对脏区重跑光栅，不在本批范围。
2. **局部上屏未接入实时循环**：`blit_rects` / `take_damage` 已落盘但未被默认 `fbdev_app` 循环消费；接入前需补齐菜单栏 / Dock / AI 条 / 光标 / 浮层的逐帧脏区登记，并在真实 fb0 硬件验证。
3. **脏区不做矩形合并**：交互帧脏区数量很小，列表直存足够；重叠矩形重复写，结果不变。
4. **基准为 debug 轮廓、软件缓冲口径**：真机帧缓冲写入带宽、跨平台后端上屏成本均未测。

---

## 5. 测试清单

### 5.1 G3 新增单测（`draw/damage.rs`，共 4 个）

| # | 测试名 | 覆盖点 | vm 副本执行 |
|---|---|---|---|
| 1 | `mark_clips_and_dedupes` | 越界裁剪 / 重复登记剔除 / 新矩形覆盖旧矩形 / 空矩形不登记 | 已通过 |
| 2 | `bounds_unions_all_rects` | 多矩形外包矩形合并；空脏区返回 `None` | 已通过 |
| 3 | `partial_present_leaves_untouched_pixels_alone` | 脏区内改写、区外保留原值 | 已通过 |
| 4 | `random_sequence_matches_full` | 200 步随机移动部件 + 光标，旧位 / 新位都进脏区；参考路径全量上屏、对照路径局部上屏，每一步逐字节相等 | 已通过 |

### 5.2 既有测试

- 拆分后 `draw` 既有断言（`span_tests` / `render_equiv_tests` / `blit_tests`，含快速路径与参考路径逐像素等价、壁纸分帧与全量生成一致、脏行检测等）继续通过。

---

## 6. 验证结果（真实命令与口径）

### 6.1 Linux 云主机（vm，已实机执行）

- 环境：vm Linux 副本 `/tmp/aether/src3`；最终编译 / 测试 / clippy 直接在该副本上完成（期间 wayland 在途阻断已由另一工作流解除，未对 `wayland/**` 做任何本包改动）。
- 命令：

  ```bash
  export PATH="$HOME/.cargo/bin:$PATH"
  cd /tmp/aether/src3
  cargo build -p aether-compositor --locked
  cargo test -p aether-compositor --locked
  cargo clippy -p aether-compositor --all-targets --locked
  ```

- `cargo test`：

  ```text
  test result: ok. 488 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s
  ```

  - 改动前 vm 基线：compositor 473 passed / 0 failed。本包新增 4 个 damage 测试，其余增量来自另一工作流的 wayland fd 测试；测试数未减少。
- `cargo clippy`：退出码 **0**，**0 error**。本包新增代码无死代码 / 冗余转换告警（保留待接线的 API 已显式 `#[allow(dead_code)]` 并注明用途）。剩余告警均为改动前基线既有的风格类提示（函数参数过多、冗余 cast 等，跨 `draw/` 历史代码与 wayland / fbdev_app 等文件分布），以及两条既有 manifest 警告：workspace `repository` 未用字段、compositor 未用依赖 `log`；未在本包范围内处理。

### 6.2 基准实测（vm，debug 轮廓，软件缓冲拷贝口径，每格 200 次，三轮取数）

| 分辨率 | 缩放 | 全量 ms/帧（三轮） | 局部 ms/帧（三轮） | 脏区像素 | 脏区占比 | 局部 / 全量 |
|---|---|---:|---:|---:|---:|---:|
| 1920×1080 | 100% | 0.628 / 0.622 / 0.606 | 0.136 / 0.140 / 0.127 | 364,336 | 17.6% | 0.21–0.22× |
| 1920×1080 | 150% | 0.621 / 0.621 / 0.597 | 0.290 / 0.282 / 0.258 | 751,260 | 36.2% | 0.43–0.47× |
| 3840×2160 | 100% | 2.657 / 2.732 / 2.510 | 0.157 / 0.159 / 0.143 | 418,096 | 5.0% | 0.06× |
| 3840×2160 | 150% | 2.787 / 2.700 / 2.581 | 0.297 / 0.297 / 0.271 | 805,596 | 9.7% | 0.10–0.11× |

- 数值为 debug 构建、内存缓冲 `copy_from_slice` 口径；发布轮廓与真机 fbdev 写回带宽会不同，不能据此承诺上屏帧率。
- 1080p @150% 脏区占比升至 36.2%（窗口按 1.5 倍放大），局部相对收益随之降到约 0.43–0.47×，符合脏区越大拷贝越多的预期。

### 6.3 未执行的门禁（如实标注原因）

- **`python scripts/shot-diff.py verify`（57 档逐字节回归）：未执行。**
  原因：vm 副本中不存在 `scripts/` 目录，也缺少基线 BMP 构建产物；该门禁须在 QYY Windows 在线工作树运行。
- **`python scripts/bench-gate.py`（P99 ≤ 33ms 性能门禁）：未执行。**
  原因：同上，vm 副本缺 `scripts/` 与对应基线；本包基准数字为软件缓冲口径，不替代该门禁。
- **Windows 编译 / 测试 / clippy：未复跑。** 编写本报告时 QYY 离线；改动前 Windows 基线历史记录为 479 passed / 4 failed（4 个失败全在 `wayland::session::tests`，SCM_RIGHTS 未实现，属另一工作流范围的预先存在失败）。代码层审计不能替代 Windows 编译结果。

---

## 7. QYY 恢复后的回灌与复跑步骤

1. 解压 `draw-damage-delta.tar.gz`，按清单覆盖对应项目根路径；`settings.rs`、`control_layout.rs` 未改，不需覆盖。
2. 先核对共享副本里另一工作流的 `wayland/fd.rs` 已自行落定（E0603 解除），再编译。
3. Windows GNU 工具链复跑：

   ```powershell
   & "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu test -p aether-compositor --locked
   & "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu clippy -p aether-compositor --all-targets --locked
   ```

4. 像素回归：`python scripts/shot-diff.py verify`；性能门禁：`python scripts/bench-gate.py`；基准复跑：`python scripts/damage-bench.py 200`。
5. 若日后要把实时循环从 `blit_dirty` 切到 `blit_rects`：先补齐全部持久 UI / 光标 / 浮层逐帧脏区登记，在真实 fb0 验证；注意两条通路的 `prev` 基线不可混用。

---

## 8. 增量包清单（见 `draw-damage-delta.manifest.txt`）

仅含本包允许清单：`draw.rs`、`draw/{primitives,wallpaper,files,desk,icons,overlays,content,damage,theme}.rs`、`fbdev.rs`、`app_main.rs`、`scripts/damage-bench.py`。排除：`wayland/**`、`target/`、`Cargo.toml`、`Cargo.lock`、`docs/README.md`、`.github/`。
