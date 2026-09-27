# AetherOS 代码索引

生成时间：2026-09-27（复核自 git 历史 + 代码实测）· 源码 **10,684 行**（7 个 crate，25 个 .rs 源文件，不含 `target/`）

> ⚠️ 统计陷阱：`wc -l aether-*/src/*.rs` 会**漏掉 `aetherd`**（它的目录名没有连字符，2,041 行不计入）。此前文档里的 7,500 / 8,600 行都是这个原因少算的。逐 crate 统计见下表。

| crate | 行数 | 文件 | 职责 |
|---|---|---|---|
| `aether-compositor` | 6,021 | 6 | 自研合成器 + 桌面 Shell 职责（`draw.rs` 3,273 为视觉层） |
| `aetherd` | 2,041 | 7 | AI 中枢：agent / 工具 / 权限闸门 / 混合路由 |
| `aether-init` | 1,119 | 6 | PID 1 与服务管理 |
| `aether-ops` | 703 | 3 | AI 运维：日志监控 + 故障诊断 |
| `aether-install` | 505 | 1 | 磁盘安装器（isohybrid 整盘写入） |
| `aether-ipc` | 284 | 1 | 全系统 IPC 协议 |
| `aether-shell` | 11 | 1 | 占位（职责当前由 compositor 承担） |
| **合计** | **10,684** | **25** | |

## 目录总览

```
Aether/
├── Cargo.toml               workspace 定义（7 个 crate 成员，license = GPL-3.0-only）
├── LICENSE                  GPL-3.0 全文（第三方组件清单见 README「许可证」段）
├── rust-toolchain.toml      stable-x86_64-pc-windows-gnu
├── .cargo/config.toml       rust-lld 链接器 + 自包含 mingw（本机无 MSVC）
├── ARCHITECTURE.md          架构与设计决策（ADR-001 ~ ADR-006）
├── README.md                项目定位与组件表
├── INDEX.md                 本索引
├── aether-ipc/              M0 全系统 IPC 协议 crate（通信脊柱）
├── aether-compositor/       M1 自研合成器（fbdev 软件渲染桌面，主机预览）
├── aether-shell/            M2 自研 Shell（骨架占位，职责由 compositor 承担）
├── aetherd/                 M4 AI 中枢守护进程（agent + 工具 + 混合推理 + 权限）
├── aether-ops/              M5 AI 运维自修复（巡检/日志监听/诊断）
├── aether-init/             M3 自研 PID 1 与服务管理
├── aether-install/          M6 磁盘安装器（isohybrid dd + 持久化分区）
├── platform/                Buildroot 外部树、rootfs overlay、ISO 构建
├── scripts/                 Windows/VM 宿主开发与端到端验证脚本
├── docs/                    架构、协议、权限模型、路线图、交接与 UI 设计文档
├── aether-audit.log         AI 工具调用审计日志（运行时生成）
├── aetheros-0.1-amd64.iso   M3 构建产物（可引导 ISO，约 30MB）
└── target/                  编译产物（不纳入索引）
```

## 组件与入口

### aether-ipc（通信协议，181 行）
| 文件 | 内容 |
|---|---|
| `src/lib.rs` | 协议全部定义 + 编解码 |

- 请求：`Request::{Ping, Chat, ToolCall, SysInfo, ServiceControl}`（ToolCall 带 `approval` 一次性确认令牌）
- 响应：`Response::{Pong, ChatChunk, ToolResult, SysInfo, ServiceAck, Action, NeedsConfirmation, Error}`（ChatChunk 带 `channel` 推理通道标识；NeedsConfirmation 为 L2+ 确认卡片数据）
- 结构体：`SysReport`、`ServiceStatus`，枚举 `SysInfoScope`、`ServiceAction`
- 常量：`DEFAULT_PORT=7311`（aetherd）、`INIT_PORT=7312`（aether-init）
- 函数：`encode`（JSON+换行）/ `decode`（NDJSON 帧）

### aether-compositor（合成器，3,687 行）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (1,347) | 主循环：事件 → 布局动画 → 渲染；AI 指令条与 aetherd 通信；`--shot` 截图模式（支持 `--bubble`/`--ai-status`/`--confirm`/`--installer`/`--fonttest` 等走查参数）；`apply_action` 落地 AI 桌面行为；安装向导；L2+ 确认弹窗交互 |
| `src/draw.rs` (3,273) | "Essence" 视觉语言：`theme` 设计令牌（色板/字阶/圆角/阴影/交互态/尺寸 6 子模块）、**双模色板**（`color::*()` 函数 + 模式原子量，`--theme light|dark`，默认明亮）、`sdf_round_rect`（填充/描边/裁切共用距离场）、`fill_clipped`（内容区贴合窗口圆角）、`gradient_stops`（多段垂直渐变，窗口一次成型）、壁纸双模（深色=结构化极光带 + 颗粒，明亮=近白底 + 四角粉彩柔光团）、菜单栏/Dock/Toast/三类回复气泡/确认卡片、文件网格/终端/音乐三种窗口内容、BMP 导出 |
| `src/layout.rs` (171) | 布局引擎：`Layout::{Float,TwoCol,ThreeCol,Monocle}`、平铺目标矩形计算、拖拽边缘吸附 `Snap::{Left,Right,Top}` |
| `src/text.rs` (277) | fontdue 文本渲染（微软雅黑，CJK 可读）+ `vcenter` 垂直居中；`strings` 模块收口 UI 文案 |
| `src/input.rs` (228, Linux) | evdev 输入后端：键盘/鼠标原始事件 → UI 事件 |
| `src/fbdev.rs` (192, Linux) | framebuffer 打开/上屏（stride 对齐、复用缓冲） |

- 常量：`WIDTH=1280`、`HEIGHT=760`、设计尺寸全部收口在 `theme::metric`
- 与 aetherd 交互：`query_aether`（后台线程连接 7311，发 `Request::Chat`，收 ChatChunk[含 channel]/Action/NeedsConfirmation）
- 顶栏 AI 三态：`AiStatus::{Local,Cloud,Offline}`（本地青/云端紫/离线灰），由 ChatChunk.channel 驱动

### aetherd（AI 中枢，1,495 行）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (370) | 入口：`chat` 单轮 agent CLI / `serve` 常驻 IPC；`agent_run` 工具调用循环（≤4 轮，L2+ 中断上抛结构化确认）；`config_from_env` 读环境变量；真实系统状态汇总（/proc + init） |
| `src/intent.rs` (116) | 离线快速意图通道（快思考）：布局/时间/状态的中英规则匹配，产出 `DesktopAction` |
| `src/router.rs` (106) | 混合推理路由：隐私强制本地 → 云端不可用→本地 → 本地不可用→云端 → 复杂任务上云 → 默认本地；`Channel::label` 供 IPC 回传 |
| `src/llm.rs` (70) | OpenAI 兼容 LLM 客户端：`complete`、`local_available`（Ollama 探活） |
| `src/tools.rs` (437) | 工具系统：`registry`（sys_info / read_file / sys_probe / desktop / install_disk）、`execute` 管线（闸门→审计→执行）、罐头探针（命令全为编译期常量）、install_disk 路径白名单 + confirm 回显双确认 |
| `src/perm.rs` (203) | 权限闸门 `Gate`：`Level::{L0..L3}`、`judge` 裁决、`audit` 审计日志；`Approvals` 一次性确认令牌表（128 位、绑定 tool+参数、5 分钟、用后即废） |
| `src/server.rs` (193) | `serve`：TCP 127.0.0.1:7311 每连接一线程（连接数/行长上限）；`handle_chat` 先快速意图后 LLM，Action/确认必须先于 done 发送 |

- 环境变量：`AETHER_API_KEY`/`AETHER_API_BASE`/`AETHER_MODEL`（云端 GLM）、`AETHER_LOCAL_URL`/`AETHER_LOCAL_MODEL`/`AETHER_LOCAL_ONLY`、`AETHER_BIND`（调试用，出厂不设）
- 端口：7311（aetherd）、11434（Ollama 默认）

### aether-init（PID 1，1,021 行）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (110) | 入口：`--pid1`（挂载伪文件系统 + 持久化分区后进入）/ `--dry-run`；监督循环（200ms tick） |
| `src/unit.rs` (189) | 服务单元：`KnownService` 白名单（network/aetherd/compositor/ops/getty/noop）、`spawn_command` 字面量命令（无注入面）、`ServiceSpec`、`load_dir` |
| `src/manager.rs` (412) | 状态机 + 拓扑排序（DFS 环检测）+ 指数退避重启（2^n × 500ms，封顶 6 步，稳定 60s 计数归零）+ 僵尸收割（waitpid(-1) 含孤儿） |
| `src/ipc.rs` (146) | 服务控制通道：Linux Unix socket 0600（root-only）/ 开发态 TCP 7312 |
| `src/persist.rs` (109) | 持久化分区挂载（卷标 AETHER 候选盘列表 → /var）+ boot.log 启动记录 |
| `src/logtee.rs` (55) | 服务 stdout/stderr → /var/log/aether/<unit>.log（8MB 轮转）+ 控制台 tee |

### aether-install（磁盘安装器，459 行）
| 文件 | 内容 |
|---|---|
| `src/main.rs` | isohybrid dd 整盘安装：防呆校验（块设备/容量/`--yes`/禁自读自写）→ MBR 持久化分区规划（>2TB 放弃、不覆盖既有分区）→ mkfs.ext4 + /var 骨架 + 引导记录 |

### aether-ops（AI 运维，650 行，Linux 常驻）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (157) | 巡检主循环：15s/轮，冷却 20 轮，心跳/告警/诊断报告落盘 |
| `src/monitor.rs` (364) | 决策核心（纯函数带测试）：指标解析、服务状态、`plan` 自愈决策（restart 标记/essential 告警/冷却） |
| `src/diagnose.rs` (129) | 诊断报告组装（纯函数带测试）：事件 + 现场 + 行动 + 日志尾部 |

### aether-shell（占位骨架）
- `aether-shell/src/main.rs`（7 行）— M2 职责当前由 aether-compositor 承担

### platform（系统构建）
| 文件 | 内容 |
|---|---|
| `build-iso.sh` | ISO 一键构建：musl 静态编译 → Buildroot → defconfig → ISO |
| `br2-external/configs/aetheros_defconfig` | Buildroot 配置：x86_64、initramfs、ISO9660、引导、busybox、e2fsprogs |
| `br2-external/Config.in` / `external.mk` / `external.desc` | 外部树（AETHER） |
| `br2-external/board/aether/linux.fragment` | 内核片断（PCNET32、fbdev/evdev、virtio 等） |
| `overlay/init` | 内核首用户态：挂载伪文件系统 + lo 拉起 → `exec aether-init --pid1` |
| `overlay/etc/aether/services/*.json` | 服务定义：network / aetherd / compositor / ops / getty |
| `README.md` | 构建说明 |

### scripts / docs
| 文件 | 内容 |
|---|---|
| `scripts/setup-vm.ps1` | Windows 宿主：VirtualBox 装 Ubuntu 24.04 构建机 |
| `scripts/transfer.py` / `vm.py` | 构建机远程命令与文件同步（注意 MSYS2_ARG_CONV_EXCL） |
| `scripts/qmp-verify.py` / `qemu-verify.sh` | QEMU QMP 键鼠注入 + 截图端到端验证 |
| `scripts/e2e-permission-confirm.py` | L2+ 权限链路端到端（13 项断言，需先起 aetherd serve） |
| `scripts/shot-diff.py` | 截图像素回归（仅适用于"纯重构不应有视觉变化"的改动） |
| `scripts/bmp2png.py` | `--shot` 产物 BMP→PNG（仅标准库，供目视走查；勿包成 .sh 调用，见 ui-design-handover §11） |
| `scripts/png-crop.py` | 走查图裁剪 + 整数倍放大（1:1 检查边框/字重/图标比例） |
| `scripts/archive-ui-shots.py` | **归档走查图工具**：重建 `docs/host-ui-*.png` 全部 8 张；`--check` 为视觉回归门禁（屏蔽时钟/AI 光标非确定区，差异 > 0.02% 即失败） |
| `docs/ipc-protocol.md` | aether-ipc 协议草案 |
| `docs/ai-permissions.md` | AI 权限模型：L0-L3、审计、确认令牌 |
| `docs/roadmap.md` | 路线图（已复核至 M0–M6 完成态） |
| `docs/HANDOVER.md` | 交接报告 v4（M4/M5/M6 实测记录 + 构建/验证手册） |
| `docs/ui-design-handover.md` | UI 设计系统交接 + §10 视觉质量冲刺 + §11 本机环境陷阱 + §12 双模主题（明亮默认/深空备选，含明亮模式四个专属陷阱） |
| `docs/CODE-REVIEW-2026-09-19.md` | 代码审查 P0–P3 修复记录（37 项） |
| `docs/CODE-REVIEW-2026-09-22.md` / `CODE-REVIEW-2026-09-26.md` | 第二、三轮代码审查 |
| `docs/FIX-REPORT-2026-09-26.md` | 第三轮审查的修复报告（含安全链路实测对比） |
| `docs/PERF-REPORT-2026-09-26.md` | 性能优化报告（91 → 15.5 ms/帧的完整推导） |
| `docs/PRODUCTION-PLAN-2026-09-27.md` | **生产力化清单**：五阶段（Phase 0 立即 → Phase 4 可靠性）、每项带验收标准与依赖、六个硬门禁。**要动手先读这份** |
| `docs/UNIMPLEMENTED-2026-09-27.md` | **未实现功能清单**（按「用户会不会卡住」排序，接手先读这份） |
| `docs/host-ui-*.png` | 主机 `--shot` 走查图（深空：桌面/三列/确认弹窗/菜单；明亮：`host-ui-light-*.png`。与实机 `screenshot-*.png` 区分） |

## 关键链路

```
aether-compositor AI 指令条 ──Chat──▶ aetherd:7311
      │                              │ intent.rs 快速意图（规则）
      │                              │ router.rs 路由（本地/云端）→ channel
      │                              │ llm.rs 请求 → tools.rs 执行
      │                              │ perm.rs 闸门(L0-L3) → aether-audit.log
      │◀──ChatChunk(channel) / Action / NeedsConfirmation(token)───┘
      │      └─顶栏三态(channel) └─布局/开应用/关窗口 └─确认弹窗→带令牌重发

aetherd:7311 ToolCall(install_disk) 无令牌 → NeedsConfirmation(L3+回显目标)
      → UI 弹确认卡片 → 用户回显匹配 → 带 approval 令牌重发 → 过闸门执行

aether-init:7312 / /run/aether-init.sock(0600) ←ServiceControl─ 白名单服务启停/监督重启
aether-ops 巡检 → aether-init 服务状态 + /var/log/aether 日志 → 自愈 ServiceControl 重启
```

## 常用命令

```bash
# ⚠️ 本机 cargo 一律加 --offline：不带会卡在 registry 访问（实测 18 分钟无产出）
cargo run -p aether-compositor --offline                       # 桌面预览（交互模式，默认明亮）
cargo run -p aether-compositor --offline -- --theme dark       # 深空主题
cargo run -p aether-compositor --offline -- --shot 2           # 单帧截图自检
cargo run -p aether-compositor --offline -- --shot 2 --bubble tool --ai-status cloud --confirm 3 --echo  # UI 走查截图
cargo run -p aether-compositor --offline -- --bench 120        # 帧耗时基准（首帧/稳态/fps 上限）
AETHER_RENDER_TIMING=1 cargo run -p aether-compositor           # 逐帧分阶段耗时
python scripts/archive-ui-shots.py [--check]                   # 归档走查图重建 / 回归门禁
cargo run -p aetherd -- chat "把窗口排成两列"          # 单轮 agent（需 Ollama 或 AETHER_API_KEY）
cargo run -p aetherd -- serve                        # 常驻 IPC 服务（7311）
cargo run -p aether-init -- --dry-run ./platform/overlay/etc/aether/services  # 服务监督自检
python scripts/e2e-permission-confirm.py             # 权限链路端到端（先起 aetherd serve）
cargo test --workspace                               # 全部单元测试
```

## 键盘（2026-09-27 新增）

| 按键 | 作用 |
|---|---|
| `Tab` | 轮换活动窗口（焦点） |
| `Ctrl+W` | 关闭活动窗口 |
| `方向键` | 文件网格移动选择（上下按行、左右按格） |
| `PgUp` / `PgDn` | 文件网格整页翻动 |
| `Home` / `End` | 跳到首/末条目 |
| `1`–`4` | 切换布局（按住 Shift 时让位给上档符号 `!@#$`） |
| `Esc` / `Enter` / `Backspace` | 关闭浮层 / 确认 / 退格 |
| `Delete` | **故意不接线**：写操作要先扩权限模型（L2 敏感写） |

> 修饰键状态在输入层统一跟踪（`input::Mods`），`Ctrl+C`/`Ctrl+V` 已可上报 ——
> 剪贴板与终端输入是后续消费方（见 `docs/PRODUCTION-PLAN-2026-09-27.md`）。

## 测试分布（Windows 宿主实测 112 项；另 13 项 Linux 专属）

- `aether-ipc`：请求 roundtrip、响应解码、ChatChunk channel 往返/兼容、RegisterUi 往返（6）
- `aether-compositor`：布局（4）+ 形状快速路径等价性（6）+ 脏行（4）+ 壁纸行带（2）
  + 行内区间（5）+ **输入层键位翻译（11，跨平台）** + **窗口管理与导航（10）**
  + evdev 采集（4，仅 Linux）
- `aetherd`：快速意图、路由（含敏感上下文强制本地）、权限闸门/令牌/拒绝冷却、工具系统、解析降级（42）
- `aether-init`：拓扑/环检测/白名单/监督退避（6）+ 解析校验（3）+ 持久化（4）（13）
- `aether-install`：参数/防呆/MBR 读写/分区规划/命名（9）
- `aether-ops`：监控决策 + 诊断报告（13，Linux 专属）

> 端到端：`scripts/e2e-permission-confirm.py`（17 项断言，需先起 `aetherd serve`）。
