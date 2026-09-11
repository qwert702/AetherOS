# AetherOS 代码索引

生成时间：2026-09-05 · 源码约 7,200 行（64 个文本文件，不含 `target/`）

## 目录总览

```
Aether/
├── Cargo.toml               workspace 定义（6 个 crate 成员）
├── rust-toolchain.toml      stable-x86_64-pc-windows-gnu
├── .cargo/config.toml       rust-lld 链接器 + 自包含 mingw（本机无 MSVC）
├── ARCHITECTURE.md          架构与设计决策（ADR-001 ~ ADR-006）
├── README.md                项目定位与组件表
├── INDEX.md                 本索引
├── aether-ipc/              M0 全系统 IPC 协议 crate（通信脊柱）
├── aether-compositor/       M1 自研 Wayland 合成器（当前为主机渲染预览）
├── aether-shell/            M2 自研 Shell（骨架占位）
├── aetherd/                 M4 AI 中枢守护进程（agent + 工具 + 混合推理）
├── aether-ops/              M5 AI 运维自修复（骨架占位）
├── aether-init/             M3 自研 PID 1 与服务管理
├── platform/                Buildroot 外部树、rootfs overlay、ISO 构建
├── scripts/                 Windows 宿主开发脚本
├── docs/                    协议、权限模型、路线图
├── aether-audit.log         AI 工具调用审计日志（运行时生成）
├── preview.bmp / preview.png   --shot 截图自检产物
└── target/                  编译产物（不纳入索引）
```

## 组件与入口

### aether-ipc（通信协议，150 行）
| 文件 | 内容 |
|---|---|
| `src/lib.rs` | 协议全部定义 + 编解码 |

- 请求：`Request::{Ping, Chat, ToolCall, SysInfo, ServiceControl}`
- 响应：`Response::{Pong, ChatChunk, ToolResult, SysInfo, ServiceAck, Action, Error}`
- 结构体：`SysReport`、`ServiceStatus`、枚举 `SysInfoScope`、`ServiceAction`
- 常量：`DEFAULT_PORT=7311`（aetherd）、`INIT_PORT=7312`（aether-init）
- 函数：`encode`（JSON+换行）/ `decode`（NDJSON 帧）

### aether-compositor（合成器，1,475 行）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (510) | 主循环：事件 → 布局动画 → 渲染；AI 指令条与 aetherd 通信；`--shot` 截图模式；`apply_action` 落地 AI 桌面行为 |
| `src/draw.rs` (664) | "Essence" 视觉语言：`palette` 色板、SDF 抗锯齿圆角/描边、多层柔和投影、柔光漂移壁纸、菜单栏/Dock/Toast/回复气泡、BMP 导出 |
| `src/layout.rs` (145) | 布局引擎：`Layout::{Float,TwoCol,ThreeCol,Monocle}`、平铺目标矩形计算、拖拽边缘吸附 `Snap::{Left,Right,Top}` |
| `src/text.rs` (182) | fontdue 文本渲染（微软雅黑，CJK 可读）；`strings` 模块收口全部 UI 文案；`clock_str` 北京时间 |

- 常量：`WIDTH=1280`、`HEIGHT=760`、`MENUBAR_H=32`、`TITLE_H=36`、`DOCK_ICON=46`、`layout::GAP=14`、`TOP_BAR=32`、`BOTTOM_DOCK=104`
- 与 aetherd 交互：`query_aether`（后台线程连接 7311，发 `Request::Chat`，收 ChatChunk/Action）

### aetherd（AI 中枢，837 行）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (150) | 入口：`chat` 单轮 agent CLI / `serve` 常驻 IPC；`agent_run` 工具调用循环（≤4 轮）；`config_from_env` 读环境变量；`SYSTEM_PROMPT` |
| `src/intent.rs` (85) | 离线快速意图通道（快思考）：布局/时间/状态的中英规则匹配，产出 `DesktopAction` |
| `src/router.rs` (94) | 混合推理路由（隐私强制本地 → 云端不可用→本地 → 本地不可用→云端 → 复杂任务上云 → 默认本地） |
| `src/llm.rs` (64) | OpenAI 兼容 LLM 客户端：`complete`、`local_available`（Ollama 探活） |
| `src/tools.rs` (239) | 工具系统：`registry`（sys_info / read_file / sys_probe / desktop）、`execute` 管线（闸门→审计→执行）、罐头探针（命令全为编译期常量） |
| `src/perm.rs` (89) | 权限闸门 `Gate`：`Level::{L0..L3}`、`judge` 裁决、`audit` 追加审计日志 |
| `src/server.rs` (102) | `serve`：TCP 7311 每连接一线程；`handle_chat` 先快速意图后 LLM；`sys_report` 占位 |

- 环境变量：`AETHER_API_KEY`/`AETHER_API_BASE`/`AETHER_MODEL`（云端 GLM）、`AETHER_LOCAL_URL`/`AETHER_LOCAL_MODEL`/`AETHER_LOCAL_ONLY`
- 端口：7311（aetherd）、11434（Ollama 默认）

### aether-init（PID 1，M3，约 700 行）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (91) | 入口：`--pid1`（挂载 /proc /sys /dev）/ `--dry-run`；启动序列 + 监督循环（200ms tick） |
| `src/unit.rs` (194) | 服务单元：`KnownService` 白名单（network/aetherd/compositor/ops/getty/noop）、`spawn_command` 字面量命令（无注入面）、`ServiceSpec`、`load_dir` |
| `src/manager.rs` (308) | 状态机 + 拓扑排序（DFS 环检测）+ 指数退避重启（2^n × 500ms，封顶 6 步） |
| `src/ipc.rs` (103) | 服务控制通道：TCP 7312，处理 `ServiceControl` / `SysInfo` / `Ping` |

### aether-shell / aether-ops（骨架占位）
- `aether-shell/src/main.rs`（7 行）— M2 规划：顶栏/启动器/通知/AI 面板
- `aether-ops/src/main.rs`（5 行）— M5 规划：日志监听/故障诊断/自修复

### platform（系统构建）
| 文件 | 内容 |
|---|---|
| `build-iso.sh` | ISO 一键构建：musl 静态编译 → Buildroot 2024.02.1 → defconfig → ISO |
| `br2-external/aetheros_defconfig` | Buildroot 配置：x86_64、initramfs、ISO9660、GRUB2、busybox、e2fsprogs |
| `br2-external/Config.in` / `external.mk` / `external.desc` | 外部树骨架（AETHER） |
| `overlay/init` | 内核首用户态：挂载伪文件系统 → `exec aether-init --pid1` |
| `overlay/etc/aether/services/*.json` | 服务定义：aetherd / compositor / getty / network / ops |
| `README.md` | 构建说明 |

### scripts / docs
| 文件 | 内容 |
|---|---|
| `scripts/setup-vm.ps1` | Windows 宿主：winget 装 VirtualBox → 下载 Ubuntu 24.04 ISO → 建 VM → 无人值守安装 |
| `docs/ipc-protocol.md` | aether-ipc 协议草案（消息表 + 约定） |
| `docs/ai-permissions.md` | AI 权限模型：L0-L3 等级、审计、撤销栈、熔断、注入防线 |
| `docs/roadmap.md` | M0–M6 路线图与勾选状态 |

## 关键链路

```
aether-compositor AI 指令条 ──Chat──▶ aetherd:7311
      │                              │ intent.rs 快速意图（规则）
      │                              │ router.rs 路由（本地/云端）
      │                              │ llm.rs 请求 → tools.rs 执行
      │                              │ perm.rs 闸门 → aether-audit.log
      │◀──ChatChunk / Action─────────┘
      └── apply_action → 布局切换 / 开应用 / 关窗口

aether-init:7312 ──ServiceControl──▶ 白名单服务启停 / 监督重启
```

## 常用命令

```bash
cargo run -p aether-compositor            # 桌面预览（交互模式）
cargo run -p aether-compositor -- --shot 1  # 单帧截图自检（1-4 布局）
cargo run -p aetherd -- chat "把窗口排成两列"  # 单轮 agent（需 Ollama 或配置 AETHER_API_KEY）
cargo run -p aetherd -- serve             # 常驻 IPC 服务（7311）
cargo run -p aether-init -- --dry-run ./platform/overlay/etc/aether/services  # 服务监督自检
cargo test -p aetherd -p aether-ipc -p aether-compositor -p aether-init  # 全部单元测试
```

## 测试分布（约 30 项）

- `aether-ipc`：请求/响应编解码 roundtrip（2）
- `aether-compositor`：布局全覆盖/三列主列/吸附分区（layout.rs 3）
- `aetherd`：快速意图 6、路由 6、权限闸门 3、工具系统 5
- `aether-init`：拓扑/环检测/白名单/缺依赖/监督退避（manager 5 + unit 3）