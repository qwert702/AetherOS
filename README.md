# AetherOS

**Linux 内核 + 全自研 Rust 用户态 + AI 中枢。**

不是发行版换皮 —— 从 PID 1 到窗口合成器、从终端到 AI 运维，用户态每一层都是自己写的。
内核用 Linux：重写它不产生任何差异化价值，差异在 **AI 中枢与交互层**。

> 以太（Aether）—— 古人设想中充满宇宙的介质。我们设想中连接人与机器的介质。

![AetherOS 桌面 · 明亮主题](docs/host-ui-light-desktop.png)

---

## 它是什么

| 层 | 来源 |
|---|---|
| 内核 | Linux |
| 底层积木（libc、驱动栈、固件） | 成熟开源件 |
| **init / 服务管理** | **自研** `aether-init`（PID 1，musl 静态） |
| **窗口合成器** | **自研** `aether-compositor`（软件光栅化，无 GPU 依赖） |
| **Shell / 界面** | **自研**（当前由 compositor 承担；`aether-shell` 仍是占位骨架） |
| **AI 中枢** | **自研** `aetherd`（快速意图 + 本地/云端混合路由 + 工具系统 + 权限闸门） |
| **AI 运维自修复** | **自研** `aether-ops` |
| **磁盘安装器** | **自研** `aether-install` |
| **IPC 协议** | **自研** `aether-ipc`（通信脊柱） |

自研部分共 **19,556 行 Rust / 40 个源文件 / 7 个 crate**（2026-09-28 实测，不含 `target/`）。

## 现在能做什么

**桌面** —— 自研合成器，fbdev 软件光栅化上屏；双模主题（明亮为默认，深空备选）；
四种布局（自由 / 两列 / 三列 / 独占）+ 拖边吸附 + 窗口缩放；中文字体渲染（fontdue）。
稳态帧耗时从 91 ms 优化到 **15.5 ms**（推导见 `docs/PERF-REPORT-2026-09-26.md`）。

**终端** —— 真 PTY，**自研 VT/ANSI 解析器**（30 项单测，跨平台可测）；逐格绝对定位渲染；
拖选复制 + `Ctrl+Shift+C/V`。

**中文输入** —— 自研输入法：自建词表 ~100 条（无外部数据依赖、无许可问题）+ 拼音状态机。
`Ctrl+Space` 切换，默认关；无候选时拼音原样上屏（不丢字）。

![中文输入法候选框](docs/host-ui-light-ime.png)

**AI 中枢** —— `aetherd` 常驻 7311：

- **快慢双思** —— 布局 / 时间 / 状态等系统指令走离线规则通道，**免 LLM 直接执行**；复杂请求才进模型
- **混合路由** —— 隐私上下文强制本地 → 云端不可用降级本地 → 复杂任务上云 → 默认本地；推理通道回传 UI
- **12 个工具** —— 文件读写 / 删除 / 重命名、回收站、剪贴板、系统探针、桌面控制、整盘安装
- **权限闸门 L0–L3** —— L2+ 弹确认卡片，L3 额外要求回显目标；一次性确认令牌（128 位、绑定工具与参数、5 分钟、用后即废）
- 所有工具调用落审计日志（带轮转）

![L3 权限确认卡片](docs/host-ui-light-confirm.png)

**系统** —— `aether-init` 作 PID 1：白名单服务模型（命令为编译期字面量，无注入面）+ 拓扑排序 +
环依赖检测 + 指数退避重启 + 僵尸收割；持久化分区（日志与审计跨重启保留）。
`aether-install` 支持整盘安装（MBR 持久化分区 + 引导记录）。
产物是可引导 ISO（约 30 MB），已在 QEMU / VirtualBox / VMware 实测开机。

**AI 运维** —— `aether-ops` 每 15 秒一轮巡检，读真实 `/proc` 指标与服务状态，异常服务自动重启，
故障诊断报告落盘。

## 还不能做什么

这一节存在的理由是：**知道边界比知道进度更有用**。

- **它不是日常可用的系统。** 现在处于"每个子系统都真跑通了"的阶段，不是"能替代你桌面"的阶段
- **无 GPU 加速** —— 全部软件光栅化。这是刻意的取舍（先把软件路径做到够快），代价是大面积重绘余量有限
- **不支持 Wayland 客户端** —— 桌面走 `DRM → fbdev → 软件光栅化`。2026-09-28 起有一个自研 `wl_display`
  子集的 spike（线协议 / 对象表 / 协议表 / `wl_shm` 像素读取 / surface 接进渲染管线均已完成），
  但**未接入生产路径**，见 `docs/PHASE3-DECISION-2026-09-28.md`
- **AI 没有接过真实模型** —— 协议层已用双假端点验证（21 项断言全过），真实 LLM 端到端待做
- **中文输入已接线到指令条与终端，但没做过实机键盘交互验证**（`--shot` 只能出静态帧，测不了输入）
- **`aether-shell` 是 11 行的占位**，Shell 职责暂时压在 compositor 里（已 12.7k 行，该拆了）
- **没有连续长跑记录** —— 门禁 1a（连续 8 小时不崩）未做，所有验证都在虚拟机内完成
- 输入侧还有未验证的交互：`Delete` 键未接线、终端拖选复制只在源码层面确认过

## 架构

```
┌───────────────────────────────────────────────┐
│  aether-compositor + (占位) aether-shell       │  桌面 / 顶栏 / Dock / AI 指令条
│    draw · layout · text · term(vt/pty)         │  渲染与交互全部自研
│    ime · textview · input · wayland(spike)     │
├───────────────────────────────────────────────┤
│  aetherd          AI 中枢：意图 → 路由 → 工具  │  权限闸门 L0–L3 · 审计
│  aether-ops       AI 运维：巡检 → 诊断 → 自愈  │
├───────────────────────────────────────────────┤
│  aether-ipc       统一 IPC（JSON / NDJSON）    │  通信脊柱
├───────────────────────────────────────────────┤
│  aether-init      PID 1 / 服务管理 / 持久化    │  musl 静态
│  aether-install   整盘安装器                   │
├───────────────────────────────────────────────┤
│  glibc · 驱动栈 · 固件                         │  成熟开源件
├───────────────────────────────────────────────┤
│  Linux kernel                                  │
└───────────────────────────────────────────────┘
```

关键链路：

```
UI ──RegisterUi──▶ aetherd:7311 ──▶ UiRegistered    （剪贴板 / 配置热载的前置门槛）
UI ──Chat────────▶ intent 快速意图 → router 路由 → llm → tools（≤4 轮）
                    perm 闸门(L0-L3) → aether-audit.log
   ◀──ChatChunk(channel) / Action / NeedsConfirmation(token)
           └─顶栏三态 └─布局 / 开应用 / 关窗口 └─确认卡片 → 带令牌重发

aether-init:7312 / unix socket 0600 ◀─ ServiceControl ─ 服务启停与监督
aether-ops 巡检 ─▶ init 服务状态 + /var/log/aether ─▶ 自愈重启
```

## 组件

| 目录 | 行数 | 说明 | 里程碑 |
|---|---|---|---|
| `aether-compositor/` | 12,720 | 合成器 + 桌面 Shell 职责（渲染 / 布局 / 终端 / IME / Wayland spike） | M1–M2 |
| `aetherd/` | 3,945 | AI 中枢守护进程（agent / 工具 / 权限 / 路由 / 模型配置 / 回收站） | M4 |
| `aether-init/` | 1,305 | PID 1 与服务管理 | M3 |
| `aether-ops/` | 736 | AI 运维与自修复 | M5 |
| `aether-install/` | 505 | 磁盘安装器 | M6 |
| `aether-ipc/` | 334 | 全系统 IPC 协议 | M0 |
| `aether-shell/` | 11 | 占位骨架 | M2 |
| `platform/` | — | Buildroot 外部树、rootfs overlay、ISO 打包 | M3 |
| `scripts/` | — | 宿主开发、走查图、端到端验证脚本 | 持续 |
| `docs/` | — | 架构、协议、权限模型、路线图、审查报告 | 持续 |

## 快速开始

在任意桌面系统上预览 Aether 桌面（开发模式，不需要虚拟机）：

```bash
git clone https://github.com/qwert702/AetherOS.git
cd AetherOS

cargo run -p aether-compositor                     # 交互预览（默认明亮主题）
cargo run -p aether-compositor -- --theme dark     # 深空主题
cargo run -p aether-compositor -- --shot 2         # 单帧截图自检
cargo test --workspace                             # 单元测试（Windows 301 项）
```

构建可引导 ISO 需要 Linux 构建机（Buildroot），见 `platform/README.md`。

> ⚠️ **Windows 开发机注意**：`cargo` 一律加 `--offline`，否则会卡在 registry 访问。
> 其余环境坑与全部命令见 [`INDEX.md`](INDEX.md) 与
> [`docs/ui-design-handover.md`](docs/ui-design-handover.md) §11。

## 质量与验证

| 手段 | 现状 |
|---|---|
| 单元测试 | **Windows 301 / Linux 311，均全绿**（`cargo test --workspace --offline --no-fail-fast`） |
| 编译警告 | **双目标 0 条**（`cargo check --workspace --all-targets`） |
| 视觉回归门禁 | **10 张归档走查图逐像素比对**，当前 10/10 零差异（`scripts/archive-ui-shots.py --check`） |
| 代码审查 | 四轮全量 / 增量审查 + 修复报告（`docs/CODE-REVIEW-*.md`、`docs/FIX-REPORT-*.md`） |
| 端到端 | 权限链路（`scripts/e2e-permission-confirm.py`）、安装器、QEMU QMP 键鼠注入 + 截图 |
| 实机自愈 | QEMU 内 kill 掉合成器 → init 自动拉起并重新初始化显示/字体/输入（门禁 1b） |
| 性能观测 | `--bench`（首帧 / 稳态 / 上限）、`AETHER_RENDER_TIMING=1`（逐阶段耗时） |

> ⚠️ 改 `cfg(target_os = "linux")` 的代码后**必须**跑 `--target x86_64-unknown-linux-musl` 交叉检查 ——
> Windows 构建会整段屏蔽那些路径，编译错误只有交叉检查能发现。

## 安全模型

**AI 能操作你的机器，所以权限是这套系统里设计得最细的一块。**
完整设计见 [`docs/ai-permissions.md`](docs/ai-permissions.md)。

| 等级 | 行为 | 工具示例 |
|---|---|---|
| L0 | 免确认 | `sys_info`、`read_file`、`sys_probe`、`trash_list` |
| L1 | 免确认 | `desktop`、`clipboard_read`、`clipboard_write` |
| L2 | **弹确认卡片** | `file_write`、`file_delete`、`file_rename`、`trash_restore` |
| L3 | **确认 + 回显目标** | `install_disk` |

几条硬约束：

- **写白名单必须比读白名单窄** —— AI 可以读 `/etc/aether` 与 `/var/log/aether`，但**不能写**：
  写前者等于让它改自己的权限规则（提权），写后者等于篡改审计（灭证）。有测试守着这条不变量
- **危险操作不新增 IPC 变体** —— 一律走 `ToolCall` 过闸门。历史上剪贴板曾走 `Request` 变体而绕过整套闸门，
  这条教训已固化为回归测试（`server.rs::ipc_gating_tests`）
- **确认令牌绑定工具与参数**，5 分钟过期，用后即废；每次裁决都写审计日志
- `read_file` 与 `clipboard_read` 标记为**敏感输出** —— 其结果强制本地推理，不上云

## 文档导航

| 文档 | 内容 |
|---|---|
| [`INDEX.md`](INDEX.md) | **代码索引**：组件与入口、逐文件职责、工具清单、测试分布、键盘、命令 |
| [`docs/roadmap.md`](docs/roadmap.md) | 里程碑 M0–M6 + 已知工程质量缺口 |
| [`docs/PRODUCTION-PLAN-2026-09-28.md`](docs/PRODUCTION-PLAN-2026-09-28.md) | 生产力化清单与六个硬门禁（当前 **5.5/6**，唯一缺口是"连续 8 小时不崩"） |
| [`docs/ui-design-handover.md`](docs/ui-design-handover.md) | **视觉改动的权威依据**：设计系统、双模主题、环境陷阱 |
| [`docs/ai-permissions.md`](docs/ai-permissions.md) | AI 权限模型 |
| [`docs/WRITE-OPS-2026-09-28.md`](docs/WRITE-OPS-2026-09-28.md) | 可写文件操作的设计约束 |
| [`docs/PHASE3-DECISION-2026-09-28.md`](docs/PHASE3-DECISION-2026-09-28.md) | Wayland 决策框架与放弃条件 |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | 分层与 ADR（M0 冻结稿） |
| [`docs/HANDOVER.md`](docs/HANDOVER.md) | 交接报告：实测记录 + 构建 / 验证手册 |

## 许可证

**GPL-3.0-only** —— 全文见 [LICENSE](LICENSE)。

你可以自由使用、修改、分发 AetherOS。但如果**分发**了衍生作品（发二进制、或通过网络提供服务），
**必须同样以 GPL-3.0 开源完整对应源码**，且不得附加额外限制。
不允许把改动闭源之后拿去卖。

### 第三方组件

AetherOS 的 ISO 是**聚合体**（aggregate）—— 一批许可证各异的独立程序打包在一起：

| 组件 | 许可证 |
|---|---|
| Linux 内核 | GPL-2.0-only |
| BusyBox | GPL-2.0 |
| 自研用户态（`aether-*` 七个 crate） | **GPL-3.0-only** |
| Rust 依赖（anyhow / log / serde / libc / fontdue / minifb 等） | MIT 或 MIT + Apache-2.0 双许可 |

用户态程序通过系统调用使用内核服务，按内核 `COPYING` 的明确豁免（*"This copyright does not cover
user programs that use kernel services by normal system calls"*），**不构成衍生作品**，
因此自研部分可以独立选择 GPL-3.0。

> 注意：GPL-2.0 与 GPL-3.0 互不兼容。当前架构不受影响（用户态独立于内核），
> 但**将来若新增内核模块，该部分必须为 GPL-2.0 兼容**。
