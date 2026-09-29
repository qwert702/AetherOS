# AetherOS UI 设计系统 · 交接文档

日期：2026-09-20
分支：`feat/ui-design-system`（基于 `fix/m3-desktop-render`）
计划依据：`docs/archive/ui-design-plan.md`
当前状态：**Step 1–4 的代码已完成并验证；Step 4 收尾项 4.1/4.2/4.3（协议+UI 视觉）已于 2026-09-20 完成（见 §8），4.3 真机点击与 Step 5 未开始**

---

## 0. 一句话交接

设计令牌体系（`draw.rs::theme`）已落地，四个核心组件已按 §3 重绘，**L2+ 权限确认从协议到弹窗全链路打通并实测通过**。接手方需要做的是：4.3 的真机点击端到端（需要交互式桌面/实机）、Step 5 的走查与归档、以及需要实机环境才能做的截图更新（§8.5 已推进协议侧与文档侧）。

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
| 测试 | `cargo test --workspace` | 65 项全绿（Windows 宿主；含 Linux 专属共 82 项定义）|
| 编译 | `cargo check --workspace --all-targets` | 零警告零错误 |
| 像素回归 | 见 §5.3（`scripts/shot-diff.py`） | Step 1 时掩码外差异 0 像素 |
| 权限链路 | `python scripts/e2e-permission-confirm.py`（需先起 aetherd） | 13 项断言全过（2026-09-20 代码变更后重跑仍全过） |

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
--bubble user|ai|tool  回复气泡走查（4.2 新增）：默认 ai
--ai-status local|cloud|offline  顶栏 AI 三态走查（4.1 新增）：默认 local
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
- `docs/archive/ui-design-plan.md`：可在文件头标注各 Step 的完成状态与提交号

### P3 · 继承自代码审查的待办（`docs/archive/CODE-REVIEW-2026-09.md` §3，全部仍有效）
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

---

## 8. 实施推进记录（2026-09-20，按 §7 顺序执行）

### 8.1 4.1 `AiStatus::Cloud` 接线 ✅

| 改动点 | 变更 |
|---|---|
| `aether-ipc/src/lib.rs` | `Response::ChatChunk` 新增 `channel: Option<String>`（`#[serde(default)]` 向后兼容）；新增往返 + 旧包兼容 2 项单测 |
| `aetherd/src/router.rs` | `Channel::label()`（"local"/"cloud"）+ 稳定标识单测 |
| `aetherd/src/main.rs` | `AgentOutcome.channel`；`agent_run` 记录每轮实际路由通道并随四个返回点带上 |
| `aetherd/src/server.rs` | 快速意图 → `channel:"local"`；LLM 路径 → `outcome.channel` |
| `aether-compositor/src/main.rs` | `AiEvent::Reply(text, channel)`；`query_aether` 从 ChatChunk 捕获；fbdev 与 preview 两条路径按 channel 设置 `AiStatus::{Local,Cloud}`；`--shot` 新增 `--ai-status local\|cloud\|offline` 走查参数 |

**验证（像素断言，`analyze.ps1` 18 项全过）**：顶栏状态点 本地=24 个青色像素、云端=24 个紫色像素、离线=0 彩色像素 + 灰色实心块。即「走云端=紫、仅本地=青、aetherd 停=灰」三态均已落地。
**接受的验收标准**：云端配 `AETHER_API_KEY` 时顶栏紫色「云端模型」——已由 `--ai-status cloud` 走查截图覆盖渲染侧；真机配 Key 后由 §8.3 通路自然生效。

### 8.2 4.2 气泡三类视觉验证 ✅

- `aether-compositor/src/main.rs`：`--shot` 新增 `--bubble user|ai|tool`（默认 ai）
- 生成三张走查图并做像素断言：三类气泡在气泡带内彼此差异显著（16k/5k/18k 像素），带外（掩码时钟/光标/气泡+阴影包络）差异 0/166 像素 → 确认参数只影响气泡本身；工具气泡含绿色状态条（68px）；用户气泡为 SURFACE_3 底色（6738 vs 362px）
- 对齐目视结论：用户类靠右、AI/工具类靠左的偏移符合 `draw_reply` 公式

### 8.3 4.3 确认弹窗交互 ✅（协议层 + UI 视觉；真机点击待实机）

- **协议层**：重建 `aetherd` 后 `aetherd serve` + `python scripts/e2e-permission-confirm.py` → **13 项断言全过**（无令牌拦下/令牌放行/重放 403/篡改参数 403/L1 不受影响）
- **UI 视觉**：`--confirm 2` / `--confirm 3` / `--confirm 3 --echo` 像素断言全过：L3 红徽章（1142px）、L2 黄徽章（988px）、回显匹配后描边转绿（未匹配 0 → 匹配 1128px）、遮罩压暗（38.1 → 29.0 平均亮度）
- **未覆盖**：真实鼠标"点允许一次/点拒绝"的端到端点击（需交互式桌面或实机；Windows 预览点击无法脚本化）。单 `Option<confirm>` 并发覆盖的已知限制未变（合成器同一时刻仅一条 AI 查询在飞）

### 8.4 4.4 全界面走查 ✅（核对结论，未改视觉）

对照 §3 token 表 + 计划 §3 逐项核对 `draw.rs`/`layout.rs`：

| 核对项 | 结论 |
|---|---|
| 圆角档位 | 全部落到 `radius::{SM,MD,LG}` 或标注为组件局部几何（26/20/11/7 等），观感一致，按 Step 1 约定可接受 |
| 间距网格 | `GAP=14` 是计划 §3.3 明确保留的例外；其余间距遵循 4px 网格 |
| 裸字号 | 文本绘制全部走 `font::` 令牌或 `--fonttest` 标的局部值；`div_ceil`/radius 非文本参数不含字号 |

未发现需要修改的漏网令牌；P4-1（vcenter 依赖字体度量）与 P4-2（阴影性能未掐帧）维持原状。

### 8.5 4.6 文档更新 ✅

- `docs/roadmap.md`：按 git 历史与实测复核，M0–M6 全部勾选至完成态并补事实备注（smithay 未接入→fbdev 路径、Shell 职责由 compositor 承担、L2+ 确认链路、M5/M6 实测记录）；新增「UI 设计系统」章节
- `docs/INDEX.md`：行数/组件表/测试分布/走查参数同步（7,500 行、65+17=82 项测试）

### 8.6 遗留（需实机环境，未在本轮完成）

- §4.5 `docs/screenshot-*.png` 实机重拍（需 QEMU/VBox/VMware）
- §4.3 真实点击端到端（交互桌面）；P3 全部项（fbdev stride/modeset、init 救援、Unix socket 权限、ISO 引导链）
- §P1 之外的已知限制：中文输入、接入真实 LLM 验证、`AiStatus::Cloud` 的真实云端触发（结构已通，等 API Key）

---

## 9. 视觉重构推进记录（2026-09-24，诊断驱动四步走）

按用户诊断（5 项）→ 修复顺序（4 步）执行，全部集中在 `aether-compositor/src/draw.rs`（+main.rs 一处底色），
**布局几何/协议/行为/字体管线零改动**。每步单独提交 + 像素断言 + 65 项测试回归。

| 提交 | 对应诊断 | 内容与验证 |
|---|---|---|
| `c4e53c2` Step1 | 1 死黑 / 2 灰泥 | 色板 7 档重排（级差 8-11→15-18，新增 SURFACE_4）、壁纸极光强化（青/紫 wash 强度 0.10-0.28→0.15-0.45 + 暗角 0.30→0.14）；像素断言 6 项：青 29→1956、紫 28→1861、壁纸/菜单栏/窗口亮度 18/29/47→35/42/64 |
| `cdf9796` Step2 | 3 图标廉价 | 每应用专属低饱和双色渐变底座 + 白色几何符号（纯 rect，脱离字体字形）；安装器琥珀警示色；卡片缩略图同语言重绘；运行点 4→3px；断言 10 项全过 |
| `192b2dd` Step3 | 4 文字层级 | 激活标题纯白 1.0 / 非激活 TEXT_DIM（弃 0.4 灰）、侧栏 0.75→0.9、终端/菜单/向导文字提亮；断言 4 项全过 |
| `0fc1dd6` Step4 | 5 细节糙 | 红绿灯 12→10px、间距 8→7、符号收细、非激活窗控 0.45→0.6、发丝描边统一提亮、侧栏 hover 0.7→0.65；断言 6 项全过 |

**部署**：构建机重建 ISO（musl 五二进制 + 内核 initramfs 重嵌 + isohybrid）→ 替换 VMware 引导盘 →
全新开机验证：init→aether-init→持久化挂载→网络 DHCP→compositor fbdev 1280×800 渲染 1000+ 帧，
五服务全 ✓（`D:\aether-vm\AetherOS-VMware\serial.txt`）。

**回归基线**：`cargo test --workspace` 65 项全绿；`cargo check --workspace --all-targets` 零警告；
`git diff --check` 干净。主机走查图集：`%TEMP%\aether-ui-verify\final\*.png`（11 张）；
VM 实拍：`C:\Users\cbn\Pictures\aether-vm-final.png`。

**未覆盖**：真实鼠标点击的端到端（需交互桌面）；`docs/screenshot-*.png` 实机重拍（主机 --shot 图
已可用，实机图待 VM 会话内操作后补）；中文输入、真实 LLM Key 验证仍旧。

---

## 10. 视觉质量冲刺（2026-09-24 晚，用户复核后二轮）

用户复核 §9 的成果后结论是"还是不行"。复核截图确认：令牌体系落地了，但**感知质量没跟上**——
壁纸读起来是"脏渐变"不是极光、明暗层级太平没有纵深、图标与内容区仍是占位感。
本轮不做令牌层工作，只做**感知层**：改完每轮 `--shot` 出图目视，共 9 轮迭代。

### 10.1 壁纸：从"平摊柔光"到"结构化极光带"

`draw_background` 重写。原实现是 3 个大半径 wash 叠加 → 出来是一块发灰的脏渐变。
新实现是**有走向的光带**：

| 要素 | 做法 |
|---|---|
| 光带 | `AuroraBand`：翘曲中心线（主波 + 0.34 幅次谐波，避免标准正弦的机械感）+ 窄横截面 `1/(1+d²)²`（Lorentzian 平方，廉价且尾部像极光的自然消散）+ 沿 x 的余弦包络 |
| 构图 | 重心放在"真正露出来的地方"——窗口覆盖中区，所以可见区是上缘与下缘，第三条带特意压到 y≈0.78；**不在屏幕边缘放窄带**（只露一窄条时窄带会读成色块） |
| 颗粒 | 确定性 hash 噪点 ±2/255：软件光栅的深色渐变必然出等高线，没有颗粒质感立刻崩 |
| 性能 | 逐 x 预计算中心线与包络（`w` 长度数组），每像素只剩一次除法的横截面求值；背景仍是 240 帧刷新一次 |

### 10.2 纵深：深色主题下投影不可见，只能靠明度阶梯

- 色板整体压深一档（`INSET 38→30 / SURFACE_1 54→46 / SURFACE_2 66→64`），相邻级差稳定在 12-16
- 窗口用 `gradient_stops` 一次成型：标题栏亮一档（硬变色标）→ 主体平色 → 底部微暗；
  **一遍 SDF，无二次叠加接缝**（这是"窗口必须有头"的关键，否则整窗是一块灰纸板）
- 新增 `fill_clipped`：内容区（侧栏/内容面/状态条）按窗口自身的圆角 SDF 裁切，
  否则方角会把窗口底角切成直角
- `sdf_round_rect` 抽出，填充/描边/裁切共用同一距离场，形状严格一致

### 10.3 内容形态：从"占位"到"像真的"

| 窗口 | 改动 |
|---|---|
| 文件 | 三层结构（深色侧栏 / 亮一档"纸面" / 深色状态条）+ 图标网格（**无卡片外框**——"框里再放块"是廉价感来源之一）+ 文件夹图标（图形即图标：页签 + 渐变主体 + 顶部高光，饱和度刻意压低）+ 名称标签 + 状态条（数量取实际画出值，不写死） |
| 终端 | 提示符分色 + 9 行命令/输出（含 `cat services/aetherd.json`、`aether-ipc --probe`）+ 逐段横向裁切（不裁会画出窗口外）+ 闪烁光标 |
| 音乐 | 左列表（封面块 + 曲名/艺人/时长，窄窗口自动截断）+ 右"正在播放"面板（大封面 + 曲名 + 进度 + 传输控件）；窗口 < 540px 时退化为纯列表 |
| Dock | 图标底座改"上亮下暗"（自上方受光）——上暗下亮会读成"压扁的按钮" |

### 10.4 三个必须记住的坑

1. **没有模糊就不能半透明**：窗口主体 alpha 从 0.86 提到 0.955（激活）。合成器没有
   `backdrop-filter`，半透明窗口会让后面窗口的文字"透"上来变鬼影，比没有玻璃感难看得多。
2. **文本绘制没有横向裁剪**：长行会直接画到窗口外。终端与列表都已按剩余宽度 `ellipsize`。
3. **`ellipsize` / `fill_clipped` / `arrow_glyph` 是新增的公共设施**，新写绘制代码优先复用；
   图标内的符号一律纯几何绘制（不依赖字体字形覆盖）。

### 10.5 验证与产物

- `cargo check --workspace --all-targets` 零警告；`cargo test --workspace` **65 项全绿**
  （上次遗留的 `supervision_restarts_and_backs_off` 也已通过）
- 主机走查图归档（新命名前缀 `host-ui-`，与实机 `screenshot-*.png` 区分）：
  `docs/host-ui-desktop.png`（两列）、`host-ui-threecol.png`（三列）、`host-ui-confirm.png`（L3 确认）、`host-ui-menu.png`（下拉菜单）
- 截图工具：`scripts/bmp2png.py`（BMP→PNG，仅标准库）。
  ⚠️ 不要把它包成 `.sh` 再调用——本机 bash 嵌套调用时 PATH 被污染，`cargo` 起不来（见 §11）

**仍未覆盖**：实机（VM）复看本轮视觉；拖拽/悬停等交互态的目视；`shadow()` 在多窗口下的性能实测。

---

## 11. 本机环境陷阱（Windows + w64devkit bash）

| 现象 | 原因 | 对策 |
|---|---|---|
| 脚本里调 `cargo` 静默失败（exit 1，无输出） | 嵌套 `bash script.sh` 时 PATH 变成分号分隔的 Windows 形式，`cargo.exe` 起不来 | 不要写 `.sh` 包装脚本；直接用链式命令 `cargo run ... && python scripts/bmp2png.py ...` |
| `/tmp` 时有时无 | w64devkit 的 `/tmp` 映射不稳定 | 临时产物放 `target/uishot/`（已在 `.gitignore` 覆盖范围内） |
| `.git/refs/heads/<分支>` 被外部删除，HEAD 变"未出生分支" | 未查明（沙箱/外部进程）；`git update-ref` 在沙箱内不生效 | 提交对象通常完好：先查 reflog 确认 commit 存在，再直写 ref 文件（必要时关沙箱），**不要重做工作** |

---

## 12. 双模主题：明亮（默认）+ 深空（2026-09-24 夜）

产品方向调整：用户要求"苹果那种高级感、明亮通透、简洁精致、留白充足"的界面。
做法是**双模**而不是推翻——深色那版已验证的成果全部保留，可随时 A/B。

### 12.1 架构：色板常量 → 模式函数

`theme::color` 的常量全部改为函数（`color::surface_1()`），模式存在 `AtomicU8`，
启动时定一次（`--theme light|dark`，**默认 light**）。172 个调用点靠一次性正则改写，
之后新增视觉代码不必区分模式。

关键的**语义 token**（这是双模能低成本成立的原因）：

| token | 深空 | 明亮 | 作用 |
|---|---|---|---|
| `hairline()` | 白 | 黑 | 描边/分隔/悬停叠层。**调用点 alpha 一行不改**——颜色随模式反转 |
| `HIGHLIGHT`（常量） | 白 | 白 | 顶部内高光/玻璃上缘，两模式都是白 |
| `glass()` + `glass_alpha()` | 底座色 | **白** | AI 指令条、Dock 托盘等悬浮层 |
| `scrim()` + `scrim_alpha()` | 黑 0.35 | 深蓝灰 0.24 | 模态遮罩 |
| `aurora_cyan/violet/blue/pink()` | 发光带色 | 低饱和粉彩 | 壁纸配色 |
| `elevation::elev_*()` | 0.25/0.40/0.55 | **0.17/0.21/0.27** | 阴影强度 |

**明度方向两模式一致**（内容面比窗口体亮＝抬起，底座比窗口体暗＝沉下），
所以绝大多数调用点不需要任何模式判断。

### 12.2 明亮模式的四个专属陷阱（踩过的）

1. **明度阶梯必须重排，不能照搬深色数值**。首版 `inset`(233) 与 `surface_1`(233) 几乎相等
   → 侧栏/状态条/音乐面板/菜单栏全部与窗口同色，整屏糊成一片。最终阶梯：
   **底座 226 < 窗口体 242 < 纸面 255**，相邻差 ~14。
2. **悬浮层必须是白玻璃**。AI 指令条/Dock 托盘若沿用底座色，在浅色下就是"一块灰板"，
   界面立刻廉价。另：白玻璃盖在已接近纯白的窗口上区分度不足 → 不透明度提到 0.95。
3. **壁纸不能沿用极光带**。光带结构只在深色底读得出；浅色底上宽光带糊成"奶雾"、
   窄光带变成脏色块。浅色改走 **近白底 + 四角粉彩柔光团**（Apple 浅色壁纸的语言）。
4. **阴影要更轻但更明确**。`shadow()` 是 5 层叠加，边缘处合成接近强度的 2 倍——
   白底上给 0.25 会压出脏黑边；给 0.11 又完全找不到窗口边界。落在 0.17 附近最像 macOS。

### 12.3 验证

| 项 | 结果 |
|---|---|
| 深色回归 | **像素级零回归**：与上一版 `docs/host-ui-desktop.png` 逐像素比对，壁纸区平均差 0.0、窗口区平均差 0.06（仅抗锯齿/时钟相位） |
| 浅色走查 | 桌面/三列/确认弹窗/下拉菜单/安装向导/工具气泡/AI 思考态 全部目视通过 |
| 编译 | `cargo check --workspace --all-targets` 零警告 |
| 测试 | `cargo test --workspace` **65 项全绿** |
| 归档 | `docs/host-ui-light-{desktop,confirm,installer,menu}.png`（深色图 `host-ui-*.png` 仍有效） |

### 12.4 新增工具

`scripts/png-crop.py`：裁剪 + 整数倍放大走查图，用于 1:1 检查边框/字重/图标比例。
（放大实现踩过一次坑：行字节乘了两次，行宽变 scale² 倍导致 PNG 损坏。）

---

## 13. 第三轮代码审查的 UI 修复（2026-09-26）

来源：`docs/archive/CODE-REVIEW-2026-09.md`（第三轮 P2-19 / P2-20 / P3-27 / P3-28）。全部集中在 `draw.rs`，**布局几何/协议/行为零改动**。

### 13.1 修复项

| 项 | 位置 | 改动 | 实测（修复前 → 修复后） |
|---|---|---|---|
| P2-20 主按钮对比度 | `draw.rs` 确认弹窗按钮 | 新增 `accent_strong()` 做底 + 纯白字；新增 `danger_text()` 做拒绝按钮字色 | 「允许一次」深色 **2.07 → 5.31:1**、明亮 **4.11 → 5.59:1**；「拒绝」深色 **3.22 → 5.02:1**、明亮 → **6.56:1** |
| P2-20 同类：L3 徽章 | 同上 | 底色 0.22 → 0.12；文字改用 `badge_text_color()`（新增 `warning_text()`） | 深色 **2.56 → 5.17:1**、明亮 → **6.71:1** |
| P2-20 同类：后果说明 | 同上 | 文字改主文字色（彩色文字在淡底上达不到 AA），警示语义交给左侧三角 | 深色 → **8.4:1** |
| P2-18 参数截断 | 确认弹窗参数行 | 只留头部 → **头 20 + 尾 10**，截断处用 `…` 明确标出 | "良性前缀 + 第 25 字符起的恶意值"不再能躲过视线 |
| P3-23 气泡截断 | `draw_reply` | 从头部删字符 → `ellipsize`（保留开头 + 省略号） | 消息开头（"要干什么"）不再被吃掉 |
| P3-23 同类：后果说明 | 确认弹窗 | `shown.pop()` 尾部静默截断 → `ellipsize` | 关键半句不再被砍 |
| P3-27 Dock 托盘 | `draw_dock` | 硬编码 alpha 0.68 → `glass_alpha(hover)`（明亮 0.95 / 深色 0.75） | 与注释和 §12.2 陷阱 2 的声称一致；托盘不再透出壁纸色调 |
| P3-28 `fill_rect` | `draw.rs:396` | `(rect.x + rect.w) as usize` 加 `.max(0)`（i32 负值 `as usize` 会回卷成巨值 → 误填整行） | 防御性修复，未构造出可达序列 |

> 对比度用 WCAG 相对亮度公式实测（区域取按钮内部纯色区，避开抗锯齿边缘）。
> 验证命令：`--shot 2 --confirm 3 --echo --theme {dark,light}` 出图后测区域最暗/最亮像素。

### 13.2 归档图基线重建（重要）

`docs/host-ui-desktop.png` 等**深色图长期停留在 §9 状态（2026-09-24 20:07）**，
从未随 §10 的视觉质量冲刺更新——而 `2cccb6a` 的提交信息却用它作为"深色像素级零回归"的
比对基线。实测该图与当时的实际渲染差 **1.727%**（15272 像素，最大差 15），差异集中在
Dock 托盘与次要文字。

**本轮重建了全部 8 张归档图**（深色 4 + 明亮 4），生成命令如下（`--shot N`：1 Float /
2 TwoCol / 3 ThreeCol / 4 Monocle）：

```bash
B=./target/debug/aether-compositor.exe
P=python   # scripts/bmp2png.py，仅标准库
gen() { $B $1 && $P scripts/bmp2png.py preview.bmp "$2"; }

gen "--shot 2 --theme dark"                  docs/host-ui-desktop.png
gen "--shot 3 --theme dark"                  docs/host-ui-threecol.png
gen "--shot 2 --confirm 3 --theme dark"      docs/host-ui-confirm.png
gen "--shot 2 --menu --theme dark"           docs/host-ui-menu.png
gen "--shot 2 --theme light"                 docs/host-ui-light-desktop.png
gen "--shot 2 --confirm 3 --theme light"     docs/host-ui-light-confirm.png
gen "--shot 2 --installer --theme light"     docs/host-ui-light-installer.png
gen "--shot 2 --menu --theme light"          docs/host-ui-light-menu.png
```

**教训**：归档图必须与其生成提交绑定。做视觉回归前先确认归档图的 mtime / 生成提交
是否等于当前 HEAD，否则"零回归"的结论会建立在错误的基线上（`target/uishot/` 下常有
作者未归档的中间走查图，是更近的基线）。

> **2026-09-27 更新**：这条教训**又犯了一次** —— 09-27 的文件管理器改动后，8 张归档图
> 再次全部落后于 HEAD（实测差异 0.95%–1.31%，包围盒覆盖整个文件窗口区）。
> 根因不是疏忽而是流程：归档靠手敲命令，必然漏。现已工具化，见 §15.2。

---

## 14. 合成器性能优化（2026-09-26）

背景：VM 里鼠标很卡。完整报告见 `docs/archive/PERF-REPORT-2026-09-26.md`。

**结论**：瓶颈不是 sleep，而是 `draw_window` 每帧要跑 5 层投影 SDF + 逐像素圆角渐变
（**91ms/帧**），再叠加固定的 100ms sleep，有效帧率只有 ~5fps。

三项改动（合计 **91.31 → 22.53 ms/帧，-75%**）：

1. **`sdf_round_rect` 条件化 sqrt** —— 只有角部需要开方，边部/内部直接分量相加（恒等变形）
2. **`gradient_stops` 行内快速路径** —— 用 `row_inner_span` 把每行切成"两端圆角带 + 中间直填带"
3. **投影烘焙** —— 投影只取决于窗口几何/激活态，烘焙进 `shadow_layer` 用几何指纹判失效

另加：**自适应帧率**（按 33ms 周期补足剩余时间，不再固定睡 100ms）、
**`fbdev::blit_dirty`**（只写变化的行，鼠标移动时从 4MB 降到几十字节）、
壁纸刷新间隔 240 → 1800 帧（提帧率后原间隔会让周期性卡顿变密）。

**新增观测手段（长期保留）**：
- `aether-compositor --bench [N]` —— 帧耗时基准
- `AETHER_RENDER_TIMING=1` —— 逐阶段耗时（`bg_gen` / `windows` / `dock` …）

**视觉回归**：与优化前基线逐像素比对仅差 154 像素（0.016%），全部落在顶栏时钟区。
投影烘焙的副作用是"窗口之间不再互相投影"，平铺布局下无视觉差异。

⚠️ **VM 实机未验证**：数据全部来自 Windows 开发机的 `--bench`。`blit_dirty` 的写入
路径在 Windows 上跑不到（无 `/dev/fb0`），仅其脏行选择逻辑有单测覆盖。



---

## 15. 壁纸分帧生成 + 归档图工具化（2026-09-27）

接手方复核 09-26/09-27 的三个提交时提出三项问题，本节记录修复。

### 15.1 壁纸分帧生成：首帧 561 → 30 ms

**问题**：`draw_background` 整屏逐像素生成（极光带 + 颗粒）一次要 0.4–0.6 秒，
压在单帧里就是开机后一次肉眼可见的卡死。此前只把刷新间隔从 240 帧提到 1800 帧
（降低频率），**没有解决单次成本**。

**改法**：
- 拆出 `draw_background_rows(buf, w, h, t, y0, y1)`，按行带生成；每帧生成
  `BG_ROWS_PER_FRAME = 32` 行 → 760 行 24 帧铺完（30fps 下 0.8 秒），
  单帧增量约 17ms，落在 33.3ms 预算内。
- 生成目标是**独立的 `bg` 缓冲**，屏幕内容来自 `shadow_layer`。所以：
  - 生成期间每帧把 `bg` 拷进 `shadow_layer`（只拷不烘，约 0.4ms）→ 画面**平滑逐层刷出**；
  - 铺满后做一次完整投影烘焙。
  - 若不做这一步，屏幕会停在第一帧那张"一条壁纸 + 一片黑"上直到铺满，比原来更难看。
- `--shot` / `--bench` 走 `Renderer::prepare_background()`（整屏一次补全），
  保证单帧输出完整。

**验收数字**（`--bench 120`，Windows 开发机）：

| 指标 | 改前 | 改后 |
|---|---|---|
| 壁纸整屏生成（一次性） | 561 ms（计入首帧） | 398–424 ms（**不再计入帧内**） |
| **交互路径首帧** | **561 ms** | **30 ms** |
| 分帧期间单帧增量 | — | ≈16.6 ms |
| 稳态帧耗时 | 15.3 ms | 15.3 ms（无变化） |

**回归**：新增 2 项单测 ——
`background_rows_match_full_generation`（行带生成与整屏生成**逐像素一致**，两种主题各验一遍；
这条必须有：交互路径分帧、`--shot` 整屏，两条路径不一致就是"截图与真机不同"的隐性 bug）、
`background_rows_clamps_out_of_range`（越界/反向区间不得写入）。
单帧输出与改动前逐像素比对：**明亮模式 0 差异，深色模式差异全部落在顶栏时钟区**。

### 15.2 归档图工具化：`scripts/archive-ui-shots.py`

一条命令重建全部归档图，`--check` 作为视觉回归门禁：

```bash
cargo build -p aether-compositor --offline   # 脚本直接调二进制，不经过 cargo
python scripts/archive-ui-shots.py           # 重建 docs/host-ui-*.png
python scripts/archive-ui-shots.py --check   # 门禁：差异 > 0.02% 即失败（退出码 1）
```

> **2026-09-28 更新**：归档图已从 8 张扩到 **10 张**（新增 `host-ui-ime.png` /
> `host-ui-light-ime.png`，覆盖中文输入法的候选框）。当日复核 `--check`：**10/10 全部 0 差异像素**。

- 直接调用 `target/debug/aether-compositor[.exe]`，**不经过 cargo** —— 见 §11：嵌套调用
  时 PATH 会被改写成 Windows 分号形式，cargo 静默失败。
- 比对时屏蔽两个非确定区：顶栏时钟（`x > w-80` 且 `y < 32`）与 AI 指令条光标
  （`x 515–530, y 595–630`）。加完掩码后 8 张全部 `0.0000%`。
- 顺手清掉了工作区里未跟踪的 `docs/host-ui-files-real.png`（无任何文档引用的中间产物）。

### 15.3 一行数统计的坑：`aether-*/src/*.rs` 漏掉 `aetherd`

`aetherd` 的目录名**没有连字符**，所以 `aether-*` 这个 glob 匹配不到它 —— 此前
INDEX 里的「7,500 行」「8,600 行」都是这样少算了一个 crate（2,041 行）。

**当时的实测**：7 个 crate 共 **10,684 行 / 25 个 .rs 文件**（compositor 6,021 / aetherd 2,041 /
init 1,119 / ops 703 / install 505 / ipc 284 / shell 11）。INDEX 已改为逐 crate 列表并标注此坑。

> ### ⚠️ 2026-09-28 订正：这个坑**没修干净**，多漏了两层
>
> 上面那组数字本身仍是**错的** —— 它只修了"漏一个 crate"，没修"漏二级子目录"，
> 而且顺手把总额又算错了一次：
>
> | 陷阱 | 后果 |
> |---|---|
> | `aether-*` 匹配不到 `aetherd`（目录名无连字符） | 漏掉整个 crate（3,842 行） |
> | `$c/src/*.rs` 匹配不到 `$c/src/wayland/*.rs` 这类**二级子目录** | 漏掉 7 个文件 / 2,192 行 |
> | 把"逐 crate 之和"与"通配符聚合结果"混用 | 表里写 10,684，而逐 crate 相加是 19,165 → **自相矛盾** |
>
> **2026-09-28 实测（工作区，HEAD `d11af11`）：19,426 行 / 40 个 .rs**
> （compositor 12,719 / aetherd 3,862 / init 1,266 / ops 729 / install 505 / ipc 334 / shell 11）。
>
> **正确口径**：统计必须同时包含 `$c/src/*.rs` 与 `$c/src/*/*.rs`，且**逐 crate 相加**，
> 永远不要用 `aether-*/src/*.rs` 这种聚合通配符。INDEX 已按此重写。

### 15.4 顺手同步的文档

`INDEX.md`（行数、crate 表、`--theme`/`--bench`/`AETHER_RENDER_TIMING`/归档脚本、
测试分布 91 项、新报告清单）、`docs/roadmap.md`（新增「工程化与性能」章节）。

---

## 16. 生产力化首批：输入层、窗口管理、列表滚动（2026-09-27）

依据 `docs/archive/PRODUCTION-PLAN-2026-09-27.md` 的 Phase 0 首批 + Phase 1 前置。
本轮的共同点：**都是"看着能用、实际不能用"的地方**，改完才是真能用。

### 16.1 输入层：把纯逻辑从 Linux 限定里拿出来

`input.rs` 此前整个模块被 `#[cfg(target_os = "linux")]` 挡住 —— 后果是**按键翻译这段纯逻辑
在开发机上既不能编译也不能测**（它 4 项测试长期是"Linux 专属"）。现在拆开：

| 部分 | 平台 |
|---|---|
| `UiEvent` / `Mods` / `NavKey` / `translate` / `key_char` / `shift_of` | **跨平台**（含 11 项单测，开发机可跑） |
| `spawn` / `open_devices`（evdev 采集） | 仅 Linux |

### 16.2 修饰键状态机（三处共同的前置）

**此前完全没有 Ctrl/Shift/Alt 概念** —— 所以 `Ctrl+C`、`Ctrl+V`、`Shift+选择`
在任何地方都不可能实现。新增 `Mods` 状态（跨事件累积），并新增两个事件变体：

- `UiEvent::Ctrl(char)`：Ctrl+字母**独立于 `Char`**上报。混在一起会让"按住 Ctrl 时字符被吞掉"
  变成隐式行为；而 Ctrl+符号直接丢弃（否则 `Ctrl+;` 会漏出一个散字）。
- `UiEvent::Nav(NavKey)`：方向/翻页/Home/End/Tab/Delete。
- Shift 统一经 `shift_of()` 施加：字母变大写、`1`→`!`、`-`→`_` …
  **布局切换用 Alt+1–4**（2026-09-29 从"裸数字 1–4"改过来）：裸数字必须是普通字符，
  否则 1–4 在终端与 AI 输入框里都打不出来 —— 实测表现为 `wget http://10.0.2.2/i`
  被敲成 `10.0.../i`（两个 `2` 都被当快捷键吞掉）。

### 16.3 窗口管理：红绿灯不再是装饰

红绿灯此前**只画不响应**，开满 8 个窗口后没有任何关闭手段（UNIMPLEMENTED 里的 P1-1）。
现在登记命中区（圆点 10px 太小，命中区扩到 24×24）并接线：

- 关闭 → `close_window()`：**同时修正 `active`，并要求调用方清 `drag`**
  —— 拖拽中关掉被拖的窗口正是 P2-3 那类越界 panic。
- 最大化 → `toggle_zoom()`：用 `Win.restore` 记住原矩形，再点一次恢复；必须 `floating = true`
  否则下一帧被平铺布局覆盖回去。
- 中间那颗（最小化）**不接线**：没有"最小化到哪去"的语义，不假装支持。
- 键盘补充：`Ctrl+W` 关闭活动窗口、`Tab` 轮换焦点。

### 16.4 文件列表：滚动 + 键盘导航

`Desktop.scroll` + `FileView.scroll`；几何抽成**渲染与键盘共用**的一份：

```rust
pub fn files_grid_rect(win: Rect) -> Rect      // 网格区
pub fn grid_layout(area: Rect) -> GridLayout   // 列/行/单元格
pub fn scroll_to_show(sel, scroll, per_page) -> usize   // 选中项可见（纯函数）
```

两处各写一套几何是这类 bug 的温床（"方向键移动 3 列"和"画出来 4 列"会悄悄漂移）。
键盘：`Up/Down` 按行（= cols）、`Left/Right` 单格、`PgUp/PgDn` 整页、`Home/End` 首尾。
**滚轮不做**（minifb 与 evdev 两条路径都要接线，成本远高于键盘，见计划 §0.4）。

界面如实化：状态栏由"76 个项目（显示 8）"改为 **"1–8 / 76 项"**（可见区间），
右侧加细滚动条 —— 没有它用户不知道下面还有东西。

### 16.5 Phase 1 前置：init 挂载 devpts

`aether-init` 此前只挂 proc/sysfs/devtmpfs。**没有 devpts，`posix_openpt` 创建不出 slave 端，
PTY 终端根本无从谈起**。新增挂载（`gid=5,mode=620,ptmxmode=0666`），并特意处理两件事：

- 已挂载则跳过：重复 mount 返回 EBUSY，会打出**假告警**，让"终端可用"在日志里看起来像坏了。
- 失败**显式告警**：静默失败会让"终端打不开"变成查不出原因的现象。

### 16.6 验证

| 项 | 结果 |
|---|---|
| `cargo test --workspace --offline` | **112 项全过**（91 → 112：+11 input 跨平台、+10 窗口管理） |
| Windows 目标 | 零警告 |
| Linux musl 目标（compositor / init） | 零警告（顺手修掉 init 的 1 处 unused import + 2 处 unnecessary unsafe） |
| Linux musl 目标（全 workspace） | ⚠️ **本机不可用**：`aetherd` 的 `ureq`→`ring` 需要 `x86_64-linux-musl-gcc`（本机未装），与本次改动无关 |
| 归档门禁 | 8/8 `0.0000%`（视觉有变：状态栏文案 + 滚动条，已按流程重建） |

新增测试覆盖的正是验收标准：`close_keeps_active_in_range`、`close_shifts_focus_when_removing_before_active`、
`close_to_empty_is_safe`、`zoom_toggles_and_restores_rect`、`tab_cycles_focus_including_wrap`、
`nav_moves_selection_by_grid`、`nav_keeps_selection_visible`、`nav_on_non_files_window_is_noop`、
`ctrl_w_closes_active_window`，以及一条**反向断言**：
`delete_is_deliberately_not_implemented`（写操作必须先扩权限模型，不能顺手加）。
