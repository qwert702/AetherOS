# AetherOS UI 设计系统 · 交接文档

日期：2026-09-20
分支：`feat/ui-design-system`（基于 `fix/m3-desktop-render`）
计划依据：`docs/ui-design-plan.md`
当前状态：**Step 1–4 的代码已完成并验证；Step 4 有 3 项收尾、Step 5 未开始**

---

## 0. 一句话交接

设计令牌体系（`draw.rs::theme`）已落地，四个核心组件已按 §3 重绘，**L2+ 权限确认从协议到弹窗全链路打通并实测通过**。接手方需要做的是：3 项 Step 4 收尾（都列在第 4 节）、Step 5 的走查与归档、以及需要实机环境才能做的截图更新。

---

## 1. 分支与提交

```
c645034  Step 4b–4d · 确认弹窗 UI + AI 界面补全
c486d41  Step 4a · L2+ 权限确认的协议与后端
350c26a  Step 3 · 核心组件重绘
2381f99  Step 2 · 字体渲染质量
205a7a3  Step 1 · 设计令牌收口
a0f1268  代码审查 P0–P3 修复（37 项）← 本分支基线的成果，非本次 UI 工作
```

**验证基线（接手前请复跑确认）**：

| 检查 | 命令 | 当前结果 |
|---|---|---|
| 测试 | `cargo test --workspace` | 62 项全绿 |
| 编译 | `cargo check --workspace --all-targets` | 零警告零错误 |
| 像素回归 | 见 §5.3（`scripts/shot-diff.py`） | Step 1 时掩码外差异 0 像素 |
| 权限链路 | `python scripts/e2e-permission-confirm.py`（需先起 aetherd） | 13 项断言全过 |

---

## 2. 已完成（含验证证据）

### Step 1 · 设计令牌收口 ✅
- `draw.rs` 的 `palette` 模块扩为 `theme`，含 6 个子模块：`color` / `font` / `radius` / `elevation` / `state` / `metric`
- 消灭重复定义：`MENUBAR_H`（draw.rs）与 `TOP_BAR`（layout.rs）本是同一个值的两份拷贝，现统一为 `theme::metric::MENUBAR_H` + `layout.rs` 再导出别名
- 消灭魔数：窗口标题栏命中区 `h: 34`（main.rs 两处）等
- **验证方式**：4 张布局截图与改前逐字节比对，差异**只落在两个已知非确定区**（时钟跨分钟、AI 光标闪烁相位），掩码外 0 像素

### Step 2 · 字体渲染质量 ✅
三处修复（都在 `text.rs`）：
1. **光栅化字号取整**——`12.5`/`13.5` 这类非整数 px 的 AA 字形明显发虚；渲染与测量在各自入口做同一取整，布局不漂移
2. **基线取整到像素网格**——`baseline as i32` 截断 → `.round()`（注意：上一提交刚修过基线公式 bug，此处只动取整不改公式）
3. **≤14px 覆盖增益** `sharpen()`——fontdue 无 hinting，小字号细笔画覆盖率偏低；对覆盖率做低段线性增益（14px→1.0，11px→1.45）

**量化结果**：11px 中文行「中间灰/实心笔画比」0.16 → 0.13（模糊感来源下降），4 张布局截图文字无劣化。
**结论**：参数调优已足够，**暂不需要换 CJK 字体**；若接手方要换，`wqy-microhei` 侧同样走增益路径，需实机复验。

新增 `--fonttest` 标本模式（11–15px 中英文/粗细同屏 + 打印真实字体度量）。

### Step 3 · 核心组件重绘 ✅
| 组件 | 改动 |
|---|---|
| 层级模型 | 菜单栏/Dock/侧栏/终端归入 `INSET` 深底座层；窗口 `SURFACE_1`；浮层/卡片 `SURFACE_2`；hover/输入框 `SURFACE_3` |
| 窗口标题栏 | 红绿灯 12px、间距 8px、**悬停显示符号**（× − ＋，纯几何绘制不依赖字形）；标题栏内垂直居中 |
| 顶栏 | 品牌三角改青→紫渐变；发丝描边；悬停态；AI 状态点 |
| Dock | **去彩色拟物**（§1：极光只留壁纸/AI/品牌），改中性玻璃片；区分度交给造型 + 状态（hover 提亮、运行中青色微光、运行点用 accent） |
| 按钮/输入框 | 四状态落 token（默认/悬停/按下/禁用）；按下是**叠黑压暗**而非降低底色 |
| 壁纸 | 柔光半径加大、强度降低、漂移 0.05→0.02 放缓 |
| 文本垂直居中 | 新增 `TextRenderer::vcenter(top, height, px)`，替代各处手算偏移——它按字体真实 metrics（ascent + "国"字墨迹中心）计算，**换字体/换字号自动适配** |

**量化验证**：窗口标题墨迹中心 59.5 vs 标题栏中心 60（改前 13px 恰好居中，14px 会偏下 2.5px）。

### Step 4a · L2+ 权限确认的协议与后端 ✅
**修掉的欠账**（roadmap M4 第 59 行）：
- 改前：`Request::ToolCall` 恒 `approved=true`（协议注释写"UI 已确认"，但无从验证）；AI agent 路径遇 L2+ 只能拿到字符串 `"[需用户确认后重试]"`，**用户既看不到确认请求也无法确认**
- 改后：
  - `Request::ToolCall` 增加 `approval: Option<String>`
  - `Response::NeedsConfirmation { tool, level, arguments, consequence, echo_required, token }`
  - **一次性确认令牌**（`perm.rs::Approvals`）：128 位随机、**绑定 (tool, 参数)**、5 分钟时效、用后即废、过期项自动清理
  - 令牌由服务端签发、**只发给 IPC 客户端、绝不进 LLM 上下文** → AI 无法自我授权
  - `agent_run` 遇 L2+ **中断本轮**并上抛结构化 `pending`，不再把错误字符串喂回模型让它编造"已执行"
  - `ToolCall` 默认不带令牌 = 未确认

**实测（真实 IPC，`scripts/e2e-permission-confirm.py`，13 项断言全过）**：

| 路径 | 结果 |
|---|---|
| 无令牌 L3 | 拦下，下发 level=3 / echo_required=`/dev/vda` / 后果说明 / 128 位 token |
| 带令牌重发 | 过闸门进入工具体（报错来自安装器而非权限，证明已过闸门） |
| 重放同一令牌 | 403「确认令牌无效或已被使用」 |
| 令牌 + 篡改参数 | 403「确认令牌与参数不匹配」 |
| 无令牌 L1（desktop） | 照常执行，不弹确认 |

### Step 4b–4d · 确认弹窗 UI + AI 界面补全 ✅
- **模态确认卡片**：L2 黄 / L3 红徽章、参数明文逐项展示、风险色警示三角 + 后果说明、L3 回显输入框（匹配后边框转绿、按钮点亮）、「允许一次」/「拒绝」、**无"永久允许"**（安全决策，计划明确不做）、背景压暗遮罩
- **指令条**：焦点态青紫渐变环（`gradient_outline`）；思考中→三点呼吸动效（明确"在等我"而非"在等你打字"）
- **回复气泡三类**（`BubbleKind`）：用户指令（中性底、偏右）/ AI 回复（青紫环、偏左）/ 工具调用（左侧状态条，成功绿失败红）——**来源方向即身份**
- **AI 状态三态**接线到顶栏（本地青 / 云端紫 / 离线灰）
- **安装向导与 AI 共用同一确认通路**：向导点「开始安装」不再自带 `approved`，同样过闸门弹确认（杜绝"点了按钮即授权"）

**视觉验证**：`--shot 2 --confirm 3`（L3 空回显）、`--confirm 3 --echo`（L3 已匹配）、`--confirm 2`（L2）、`--thinking` 四组截图已走查。

---

## 3. 关键上下文（接手必读）

### 3.1 截图/调试参数（`aether-compositor`，非 Linux 主机）
```
--shot 1|2|3|4     渲染指定布局单帧 → preview.bmp（Float/TwoCol/ThreeCol/Monocle）
--menu             带下拉菜单展开态
--mouse X Y        指针定位到 (X,Y)，用于拍悬停态（红绿灯符号、Dock hover、卡片 hover）
--installer        渲染安装向导单帧（demo 磁盘列表，非真实会话）
--confirm 2|3      渲染 L2/L3 权限确认弹窗
--echo             配合 --confirm，预填回显（走查"已匹配"态）
--thinking         指令条思考动效态
--fonttest         11–15px 字体标本 + 打印真实字体度量 → fonttest.bmp
```
`--shot` 渲染固定 `t = 1.2`，但**时钟用真实时间、AI 光标用亚秒时钟**——这是像素比对的天然非确定区（见 §5.3）。

### 3.2 层级与令牌（`draw.rs::theme`）
从底到顶：壁纸 → `INSET`（深底座）→ `SURFACE_1`（窗口）→ `SURFACE_2`（卡片/浮层）→ `SURFACE_3`（hover/输入框）

字阶 5 档：`LABEL 11` / `CAPTION 12` / `MONO 13` / `BODY 14` / `TITLE 20`，另有 `GLYPH 16`（图标内字形，不占文字层级）。
圆角 3 档：`SM 8` / `MD 12` / `LG 16`。阴影 3 档：`ELEV_1 0.25` / `ELEV_2 0.4` / `ELEV_3 0.55`。

### 3.3 文字垂直居中必须用 `vcenter`
```rust
let y = tr.vcenter(bar.y as f32, bar.h as f32, font::BODY);
```
不要手写 `(h - px) / 2.0`——那忽略字体的 ascent/descent 不对称，换字体必然错位。

### 3.4 平台条件
`fbdev`/`input` 模块与 `run_fbdev`/`Installer`/`run_install` 都是 `#[cfg(target_os = "linux")]`。**在 Windows 上编译时，这些路径的代码不参与，因此相关字段会报"never used"**——这类警告已用 `#[cfg_attr(not(target_os = "linux"), allow(dead_code))]` 标注，属预期。

---

## 4. 未完成清单（按优先级）

### P1 · Step 4 收尾（小改动，建议先做）

#### 4.1 `AiStatus::Cloud` 没有真实来源
- **现状**：三态渲染齐备，但 `Local`/`Offline` 有来源（收到回复=Local，出错=Offline），**`Cloud` 永远构造不出来**；`draw.rs` 里标了 `#[allow(dead_code)]`
- **原因**：aetherd 知道本轮走了哪个通道（`router::Channel`），但没告诉客户端
- **要做**：协议扩展——`Response::ChatChunk` 加 `channel: Option<String>`（值 `"local"`/`"cloud"`），aetherd 在 `handle_chat` 下发时填入，合成器收到后设 `ai_status`
- **改动点**：`aether-ipc/src/lib.rs`（加字段，注意 `#[serde(default)]` 保持向后兼容）、`aetherd/src/server.rs::handle_chat`、`aetherd/src/main.rs::AgentOutcome`（带上 channel）、`aether-compositor/src/main.rs`（`AiEvent::Reply` 带 channel）
- **验收**：配 `AETHER_API_KEY` 走云端时顶栏显示紫色「云端模型」；仅本地时青色；aetherd 停掉后显示灰色「AI 离线」

#### 4.2 气泡三类只截图验证了 AI 类
- **现状**：`--shot` 固定画 `BubbleKind::Ai`。用户类（中性底偏右）与工具类（左状态条）只经过代码走查，**没有视觉确认**
- **要做**：给 `--shot` 加 `--bubble user|ai|tool` 参数（或直接在 `--shot` 里画三条并排），出图逐一确认对齐/配色/左右偏移是否如预期
- **注意**：`draw_reply` 里用户类靠右（`w/2 + 60 - bar_w`）、AI/工具靠左（`w/2 - 60`），偏移量需目视确认

#### 4.3 确认弹窗的交互没有真机点过
- **现状**：协议层 13 项断言全过（含 403 三条路径）；UI 层只截了图，**"点允许一次→真的执行"和"点拒绝→状态回退"没有端到端走过**
- **要做**：
  - Windows 预览：`aetherd serve` + `cargo run -p aether-compositor`（去掉 `--shot`），在 AI 指令条输入能触发 L2+ 的话（例如让模型调 `install_disk`），点一次「允许一次」看是否真执行、点「拒绝」看气泡/toast 是否正确
  - Linux 实机（更关键）：触发 L3 → 回显输入 → 允许/拒绝双路径
- **已知限制**：同一时刻只保存**一个** `confirm`（`Option`），若两个 L2+ 请求并发到达会互相覆盖。当前合成器只允许单条 AI 查询在飞，但**安装向导 + AI 可并发**——若要彻底解决需改成队列

### P2 · Step 5（未开始）

#### 4.4 全界面走查（对照 §3 token 表逐项核对）
- 计划要求：圆角/间距/字阶一致性逐项核对
- **已知待查项**：
  - 圆角档位是否全落到 `radius::` 令牌（`draw.rs` 里仍有 `26.0`/`20.0`/`11.0`/`7.0` 等局部几何值——按 Step 1 的约定它们属"组件局部几何"可不入令牌，但需确认观感一致）
  - 间距是否遵循 4 的倍数网格（现有 `GAP = 14` 不在此网格上，是计划 §3.3 明确保留的例外）
  - 字阶是否还有漏网的裸字号（`div_ceil`/`radius` 等非文本参数不算）

#### 4.5 `docs/screenshot-*.png` 更新
- **重要约束**：现有 `docs/screenshot-desktop.png`、`screenshot-m4-interactive.png`、`screenshot-vbox.png`、`screenshot-vmware.png`、`screenshot-installer-wizard.png`、`screenshot-installer-done.png` 是**实机截图**（QEMU/VBox/VMware），**不是** `--shot` 生成的
- **要做**：在真实虚拟机里跑起新版本后重新拍摄替换。环境要求见 `docs/HANDOVER.md` 第 61–63 行（QEMU `-vga std`、VBox `--graphicscontroller vboxvga`、VMware 默认 SVGA）
- **无法在 Windows 开发机上完成**

#### 4.6 文档更新
- `docs/roadmap.md`：
  - 第 59 行 `- [ ] L2+ 确认卡片 UI（NEEDS_CONFIRMATION → 确认弹窗 → 带 approval 重试）` → **应勾选**（已完成并实测）
  - 第 60 行「中文输入支持」仍未做（预览期 minifb 限制）
  - 该文件整体落后实际进度约 3 个里程碑（M5/M6 已实测闭环但未反映）
- `INDEX.md`：计划要求同步更新
- `docs/ui-design-plan.md`：可在文件头标注各 Step 的完成状态与提交号

### P3 · 继承自代码审查的待办（`docs/CODE-REVIEW-2026-09-19.md` 第 6 节，全部仍有效）
1. **Linux VM 实机回归**：fbdev stride/modeset、init 救援模式、Unix socket 权限、安装全链路
2. `platform/br2-external/*` 与 ISO 引导链实测
3. 审计"显式上报"在合成器侧的 UI 呈现（当前只回到 ToolResult 文本；现在的工具气泡是天然落点——`BubbleKind::Tool` 可直接承载审计失败警告）

### P4 · 技术债
1. **`vcenter` 依赖字体度量**：它读 `font.horizontal_line_metrics` + 「国」字墨迹中心。换字体后须 `--fonttest` 复跑 `dump_metrics` 确认（`text.rs::dump_metrics` 已就绪）
2. **阴影性能未掐帧**：`shadow()` 每层是一次全区域 SDF 扫描，共 5 层。计划 §6 要求"新效果先 `--shot` 静态验证，再交互模式掐帧"。**10fps 下是否掉帧没有实测**（QEMU TCG 环境渲染本身就要数秒，需在实机上看）
3. **亚像素定位未做**：Step 2 选择了整像素取整（清晰优先），未做子像素水平定位。观感已达标，如未来要更精细可再评估
4. **`aether-install` 自身输出界面**未纳入本轮 token 统一（合成器里的安装向导已统一；纯 CLI 的 `aether-install` 输出是文本）

---

## 5. 环境与命令手册

### 5.1 构建与测试
```bash
cargo test --workspace                    # 62 项
cargo check --workspace --all-targets     # 应零警告
cargo run -q -p aether-compositor -- --shot 2 --mouse 30 64   # 出 preview.bmp
```

### 5.2 权限链路端到端（Windows 可用）
```bash
cargo build -p aetherd
./target/debug/aetherd.exe serve &        # 监听 127.0.0.1:7311
python scripts/e2e-permission-confirm.py   # 13 项断言
```

### 5.3 截图像素回归（Step 1 的验证方法，改视觉层后慎用）
`scripts/shot-diff.py`（本次从 target/ 迁入，见 §6.7）：
- `python shot-diff.py mask` — 输出两轮基线之间的差异区（非确定区）
- `python shot-diff.py check` — 要求 `after` 与 `before` 的差异**只落在掩码内**

掩码 = 四布局两轮差异并集 ∪ 时钟框 `x[1215,1280] y[0,32]` ∪ AI 光标框 `x[515,530] y[595,630]`。

> ⚠️ 这套方法只适用于"纯重构、不该有任何视觉变化"的改动（Step 1/2）。Step 3 之后视觉已大幅变化，**不要**再用它当验收门槛。

---

## 6. 陷阱与注意事项

1. **`fill_rect` 已防负宽高**（P2-15）：`rect.w <= 0 || rect.h <= 0` 直接返回。写新绘制代码时不要依赖旧的"负值会被钳成整屏"行为
2. **红绿灯符号是几何绘制**，不是字体 glyph——`wqy-microhei`/`msyh` 的符号字形覆盖不一致，用字体画会缺字
3. **`sharpen()` 只作用于 ≤14px**：14px 以上返回原值，所以调大字号不会有意外增益
4. **确认令牌绑定参数**：UI 侧 `dispatch_approved` 必须原样回传 `raw_arguments`（不是展示用的 `arguments` 字符串对），否则服务端判"参数不匹配"
5. **`echo_required` 是目标值不是字段名**：`install_disk` 的 `echo_field = Some("disk")` 会被解析成 `args["disk"]` 的实际值（`/dev/vda`）。曾误传字段名，已在 4a 修正（`tools.rs::execute`）
6. **测试断言位置**：`tools.rs` 里 `install_disk_needs_confirmation_without_approval` 现在断言的是 `ExecOutcome::NeedsConfirmation`（结构化），不是字符串 `NEEDS_CONFIRMATION`
7. **辅助脚本位置**：`scripts/shot-diff.py`（截图像素回归）与 `scripts/e2e-permission-confirm.py`（权限链路端到端）已从 `target/` 迁入仓库并随本文档提交。其余一次性脚本（`insert_confirm.py`/`wire_main.py`/`prep_s4.py`）留在 `target/` 未提交
8. **Mimosa hook 会拦 Bash 写文件**：本会话中 heredoc 写长脚本被拦过，改用 Write 工具写脚本文件再执行即可

---

## 7. 建议的接手顺序

1. 复跑 §1 的验证基线（确认起点干净）
2. P1 的 4.1（`AiStatus::Cloud` 接线）——协议小扩展，收益明确
3. P1 的 4.2、4.3（气泡截图 + 交互实测）——补齐 Step 4 的验收证据
4. P2 的 4.6（roadmap 勾选）——成本极低，避免文档继续落后
5. P2 的 4.4（走查）——纯核对，无风险
6. P2 的 4.5 + P3（实机）——需要 Linux 虚拟机环境，建议单独一轮做
