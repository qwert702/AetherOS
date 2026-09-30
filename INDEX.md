# AetherOS 代码索引

生成时间：2026-09-29 12:20（复核自 git 历史 + 代码实测）· 源码 **22,947 行**
（7 个 crate，43 个 .rs 源文件，不含 `target/`）

> 上一版（09-27）写的是 **10,684 行 / 25 文件** —— 那不是笔误，是**漏统计**：
> `wc -l aether-*/src/*.rs` 的通配 `aether-*` **匹配不到 `aetherd`**（目录名没有连字符），
> 一次性漏掉整个 crate；同时 `src/wayland/` 这类**二级子目录**也被漏掉（7 个文件 / 2,192 行）。
> 逐 crate 统计见下表，统计时必须同时包含 `$c/src/*.rs` 与 `$c/src/*/*.rs`。

| crate | 行数 | 文件 | 职责 |
|---|---|---|---|
| `aether-compositor` | 12,845 | 18 | 自研合成器 + 桌面 Shell 职责（`draw.rs` 3,621 视觉层 / `main.rs` 3,438 主循环 / `wayland/` 7 文件 2,194 行为 3.1 spike） |
| `aetherd` | 5,565 | 11 | AI 中枢：agent / 工具 / 权限闸门 / 混合路由 / 模型配置 / 回收站 / **应用安装** |
| `aether-init` | 1,319 | 6 | PID 1 与服务管理 |
| `aether-ops` | 736 | 3 | AI 运维：日志监控 + 故障诊断 |
| `aether-install` | 505 | 1 | 磁盘安装器（isohybrid 整盘写入） |
| `aether-ipc` | 334 | 1 | 全系统 IPC 协议 |
| `aether-shell` | 11 | 1 | 占位（职责当前由 compositor 承担） |
| **合计** | **22,947** | **43** | |

> 口径：2026-09-29 12:20 工作区实测（`wc -l` 口径：统计换行符个数，不含 `target/`）。
> 逐文件行数**每次提交都会漂**，引用前先跑 `python scripts/repo-stats.py`
> —— 该脚本与上表同源，`--check` 模式可当门禁：数字对不上就非零退出。

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
│   └── src/wayland/         3.1 spike：自研 wl_display 子集（7 文件 2,192 行，跨平台可测）
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
├── .workbuddy/ .workbuddy-ai/ .mimosa/ .claude/ .zcode/   AI 助手会话与钩子数据（已 gitignore）
└── target/                  编译产物（不纳入索引）
```

## 组件与入口

### aether-ipc（通信协议，334 行）
| 文件 | 内容 |
|---|---|
| `src/lib.rs` | 协议全部定义 + 编解码 |

- 请求（10）：`Request::{Ping, Chat, ToolCall, RegisterUi, ConfirmCancel, ClipboardSet, ClipboardGet, ReloadConfig, SysInfo, ServiceControl}`
  - `ToolCall` 带 `approval` 一次性确认令牌；`RegisterUi` 注册 UI 通道（剪贴板与 `ReloadConfig` 要求已注册）
- 响应（12）：`Response::{Pong, ChatChunk, ToolResult, ServiceAck, ClipboardText, ClipboardWritten, ConfigReloading, Action, NeedsConfirmation, UiRegistered, ConfirmCancelled, Error}`
  - `ChatChunk` 带 `channel` 推理通道标识；`NeedsConfirmation` 为 L2+ 确认卡片数据
- 结构体：`SysReport`、`ServiceStatus`，枚举 `SysInfoScope`、`ServiceAction`
- 常量：`DEFAULT_PORT=7311`（aetherd）、`INIT_PORT=7312`（aether-init）
- 函数：`encode`（JSON+换行）/ `decode`（NDJSON 帧）
- ⚠️ 新增 `Request` 变体是**权限模型的敏感动作**：剪贴板这条路（P1-1）就曾因走变体而绕过 `Gate`。新变体必须有对应的门槛测试（见 `server.rs::ipc_gating_tests`）

### aether-compositor（合成器，14,477 行 / 20 文件）
| 文件 | 内容 |
|---|---|
| `src/main.rs` (3,502) | 主循环：事件 → 布局动画 → 渲染；AI 指令条与 aetherd 通信；`--shot` 截图模式（支持 `--bubble`/`--ai-status`/`--confirm`/`--installer`/`--fonttest`/`--theme`/`--bench` 等走查参数）；`apply_action` 落地 AI 桌面行为；安装向导；L2+ 确认弹窗交互；`dispatch_nav`（三级导航分流）；`feed_terminal`（终端按键归属） |
| `src/draw.rs` (3,905) | "Essence" 视觉语言：`theme` 设计令牌（色板/字阶/圆角/阴影/交互态/尺寸 6 子模块）、**双模色板**（`color::*()` 函数 + 模式原子量，`--theme light\|dark`，默认明亮）、`sdf_round_rect`（填充/描边/裁切共用距离场）、`fill_clipped`（内容区贴合窗口圆角）、`gradient_stops`（多段垂直渐变，窗口一次成型）、壁纸双模（深色=结构化极光带 + 颗粒，明亮=近白底 + 四角粉彩柔光团）、菜单栏/Dock/Toast/三类回复气泡/确认卡片、文件网格/终端/音乐/Wayland surface 窗口内容、BMP 导出 |
| `src/vt.rs` (762) | **VT/ANSI 解析器**（跨平台纯逻辑，29 项单测）：转义序列状态机、屏幕缓冲、滚屏、光标 |
| `src/term.rs` (550) | 终端胶合层：Linux 走真 PTY；开发机喂真实 ANSI 脚本走同一解析/渲染路径。渲染**逐格绝对定位**（比例字体不能靠字符串推进） |
| `src/pty.rs` (170, Linux) | PTY 打开/读写（Windows 无法验证，刻意保持薄） |
| `src/input.rs` (551, 键位翻译跨平台) | evdev 输入后端：键盘/鼠标原始事件 → UI 事件；`Mods` 修饰键跟踪；`NavKey` 导航语义。**2026-09-29 修复**：布局快捷键从"裸数字 1–4"改为 **Alt+1–4** —— 裸数字当全局快捷键会让 1–4 在任何输入框里都打不出来（终端里 `wget http://10.0.2.2/...` 会被敲成 `10.0...`），这条是靠真机注入按键测出来的 |
| `src/textview.rs` (292) | 2.4 文本视图共享状态：`TextCursor`（**字符级**索引，中文安全）+ `ScrollView`（夹取只有一份）+ `nav_action`（导航键→语义动作，唯一映射） |
| `src/ime.rs` (449) | 2.3 中文输入法（最小可用）：**自建词表 ~100 条**（体积可忽略、无许可问题）+ 拼音状态机（前缀匹配/数字选词/空格提交/上下选择/退格）。`Ctrl+Space` 切换、**默认关**；无候选时拼音原样上屏（不丢字） |
| `src/text.rs` (377) | fontdue 文本渲染（微软雅黑，CJK 可读）+ `vcenter` 垂直居中；`strings` 模块收口 UI 文案 |
| `src/widgets.rs` (570) | **控件原语**（2026-09-29 P1 新增）：按钮 / 开关 / 滑杆 / 分段 / 列表行 / 滚动条 / 徽标 + 共享命中表 `HitTable`；六态统一，几何与颜色一律取 `theme` 令牌（不硬编码）。配套 `input.rs` 新增**滚轮事件**（此前完全没有映射，长列表只能翻页） |
| `src/settings.rs` (333) | **用户设置（P3）**：`/var/lib/aether/settings.json` （`/var` 是唯一持久分区）的读写与声明。JSON 用 `serde_json::Value` 手读字段、**逐字段容错**（缺失/类型错/越界一律回落默认），写入走临时文件 + `rename` 原子替换；`apply_hit` 是设置点击的纯逻辑（可用性校验 + 数值夹取），与界面解耦以便单测 |
| `src/layout.rs` (185) | 布局引擎：`Layout::{Float,TwoCol,ThreeCol,Monocle}`、平铺目标矩形计算、拖拽边缘吸附 `Snap::{Left,Right,Top}` |
| `src/fbdev.rs` (285, Linux) | framebuffer 打开/上屏（stride 对齐、复用缓冲） |
| `src/wayland/mod.rs` (39) | 3.1 spike 入口（**未接入生产路径**，见 `docs/PHASE3-DECISION-2026-09-28.md`） |
| `src/wayland/wire.rs` (423) | 线协议编解码（消息头、字节序、截断必须报错） |
| `src/wayland/protocol.rs` (259) | 12 个接口的**请求表 + 事件签名表**（索引即 opcode；事件必须用事件签名编码） |
| `src/wayland/object.rs` (184) | 对象表（id → 接口/版本，跨会话一致） |
| `src/wayland/session.rs` (897) | 协议状态机：**只吃字节、吐事件**（与传输分离，开发机可完整测）→ registry/bind/surface/shm/xdg/ack/map 全流程 |
| `src/wayland/shm.rs` (229) | `wl_shm` 像素读取（offset/stride/format → RGBA 纯逻辑；XRGB8888 字节序专项测试） |
| `src/wayland/server.rs` (163, Linux) | Unix socket 服务端 + 每连接一线程（fd 的 `SCM_RIGHTS` 收包**待真机验证**） |

- 常量：`WIDTH=1280`、`HEIGHT=760`、设计尺寸全部收口在 `theme::metric`
- 与 aetherd 交互：`query_aether`（后台线程连接 7311，发 `Request::Chat`，收 ChatChunk[含 channel]/Action/NeedsConfirmation）
- 顶栏 AI 三态：`AiStatus::{Local,Cloud,Offline}`（本地青/云端紫/离线灰），由 ChatChunk.channel 驱动

### aetherd（AI 中枢，5,565 行 / 11 文件）
| 文件 | 内容 |
|---|---|
| `src/tools.rs` (1,409) | 工具系统：`registry`（**15 个工具**，见下）、`execute` 管线（闸门→审计→执行）、罐头探针（命令全为编译期常量）、写白名单 **写 ⊆ 读**（有测试守着） |
| `src/apps.rs` (1,211) | **应用安装**：清单校验、目录安装、卸载（进回收站）、**安装前预检**。2026-09-29 增强：预检改为**递归依赖闭包**（只查第一层会漏「libA→libB 缺 libB」）、新增 **terminfo 检查**（curses 程序缺终端条目会直接报 `Error opening terminal`）、新增**拦截包内 glibc 家族**（混用两套 libc 必撞 `GLIBC_PRIVATE`）、**库名路径穿越防护**（`DT_NEEDED` 是外部输入，含分隔符的名字一律不认）；包装脚本同时设 `LD_LIBRARY_PATH` 与 `TERMINFO_DIRS`。包格式、打包器与边界见 `docs/APP-PACKAGES.md` |
| `src/main.rs` (636) | 入口：`chat` / `serve` / `config` / **`app`** 四组子命令；`serve` 启动时把已装应用挂进 `/usr/local/bin` |
| `src/server.rs` (572) | `serve`：TCP 127.0.0.1:7311 每连接一线程（连接数/行长上限）；`handle_chat` 先快速意图后 LLM，Action/确认必须先于 done 发送；`ipc_gating_tests` 把"每个 `Request` 变体都过闸门"固化成回归测试 |
| `src/perm.rs` (412) | 权限闸门 `Gate`：`Level::{L0..L3}`、`judge` 裁决、`audit` 审计日志（**带轮转**）；`Approvals` 一次性确认令牌表（128 位、绑定 tool+参数、5 分钟、用后即废） |
| `src/trash.rs` (414) | 回收站：条目数 + 总字节 + 存活天数**三重上限**；恢复不覆盖、挡路径穿越 |
| `src/modelcfg.rs` (235) | 模型配置持久化：优先级**环境变量 > 文件 > 默认**；损坏回退默认；`summary()` 脱敏；启用条件"有 Key **或** 有 Base URL"（支持自建网关） |
| `src/router.rs` (164) | 混合推理路由：隐私强制本地 → 云端不可用→本地 → 本地不可用→云端 → 复杂任务上云 → 默认本地；`Channel::label` 供 IPC 回传 |
| `src/clipboard.rs` (133) | 跨进程剪贴板（2.2）：仅注册 UI 通道可读写 |
| `src/intent.rs` (130) | 离线快速意图通道（快思考）：布局/时间/状态的中英规则匹配，产出 `DesktopAction` |
| `src/llm.rs` (249) | OpenAI 兼容 LLM 客户端：`complete`、`local_available`（Ollama 探活）。**非 2xx 时把服务器正文带进错误信息**（接真模型时唯一的排障线索；5 项回归测试用本地假端点，不联网） |

**工具清单与等级**（等级决定是否弹确认，`auto_approve_below = L2`）：

| 等级 | 工具 |
|---|---|
| L0（免确认） | `sys_info`、`read_file`、`sys_probe`、`trash_list`、**`app_list`** |
| L1（免确认） | `clipboard_read`、`clipboard_write`、`desktop` |
| L2（**需确认**） | `file_write`、`file_delete`、`file_rename`、`trash_restore`、**`app_install`**、**`app_remove`** |
| L3（**需确认 + confirm 回显目标**） | `install_disk` |

- `read_file` / `clipboard_read` 标记为**敏感输出**（`is_sensitive_output`）
- **写白名单必须比读白名单窄**：不含 `/etc/aether`（提权）与 `/var/log/aether`（灭证）。有测试守着（`write_roots_are_subset_of_read_roots`）
- ⚠️ **不要为写操作另加 `Request` 变体** —— 那会绕过闸门（这正是 P1-1 的形态）。L2 写操作一律走 `ToolCall`

- 环境变量：`AETHER_API_KEY`/`AETHER_API_BASE`/`AETHER_MODEL`（云端 GLM）、`AETHER_LOCAL_URL`/`AETHER_LOCAL_MODEL`/`AETHER_LOCAL_ONLY`、`AETHER_BIND`（调试用，出厂不设）
- 端口：7311（aetherd）、11434（Ollama 默认）

### aether-init（PID 1，1,319 行 / 6 文件）
| 文件 | 内容 |
|---|---|
| `src/manager.rs` (468) | 状态机 + 拓扑排序（DFS 环检测）+ 指数退避重启（2^n × 500ms，封顶 6 步，稳定 60s 计数归零）+ 僵尸收割（waitpid(-1) 含孤儿）。**只有 `restart: true` 的服务才会被拉起**（`manager.rs` 的 `if svc.spec.restart`） |
| `src/unit.rs` (232) | 服务单元：`KnownService` 白名单（network/aetherd/compositor/ops/getty/noop）、`spawn_command` 字面量命令（无注入面）、`ServiceSpec`、`load_dir`。含不变量测试 `shipped_essential_services_must_be_restartable` |
| `src/main.rs` (210) | 入口：`--pid1` / `--dry-run`；监督循环（200ms tick） |
| `src/ipc.rs` (155) | 服务控制通道：Linux Unix socket 0600（root-only）/ 开发态 TCP 7312 |
| `src/logtee.rs` (130) | 服务 stdout/stderr → /var/log/aether/<unit>.log（8MB 轮转）+ 控制台 tee |
| `src/persist.rs` (124) | 持久化分区挂载（卷标 AETHER 候选盘列表 → /var）+ boot.log 启动记录 |

> **`essential: true` 必须同时 `restart: true`** —— 这条不变量有测试守着（2026-09-28 新增）。
> 起因是一次**跨提交的语义漂移**：`compositor.json` 的 `restart: false` 是 09-12 设的，
> 理由是"崩溃由 ops 巡检自愈、监督器不接管"；09-19 的 P1-3（`a0f1268`"重启策略以服务定义为
> 唯一事实来源"）把 ops 改成不接管 `restart:false` 的服务，语义变成"**没人接管**"，而
> **compositor 的配置没回头核对** → 一次 panic 桌面永久死掉、只能人工重启整机。
> 现已改回 `restart: true`（init 监督 + ops 冷拉起双通道），实机 kill 验证通过。
>
> **启动顺序**（2026-09-28 修）：`run_loop` 必须**先 `ipc::spawn` 建 `/run/aether-init.sock`，再启动服务**。
> 反过来的话，任何启动后立刻查询 init 的服务（`aether-ops` 第一轮巡检就是）会报一次
> "服务状态查询失败（No such file or directory）"的假故障。启动序列**持锁**执行，
> 让这期间到达的 IPC 请求排队到 boot 完成。实机串口日志可验证：socket 行必须早于"自启动序列"行。

### aether-install（磁盘安装器，505 行 / 1 文件）
| 文件 | 内容 |
|---|---|
| `src/main.rs` | isohybrid dd 整盘安装：防呆校验（块设备/容量/`--yes`/禁自读自写）→ MBR 持久化分区规划（>2TB 放弃、不覆盖既有分区）→ mkfs.ext4 + /var 骨架 + 引导记录 |

### aether-ops（AI 运维，736 行 / 3 文件，Linux 常驻）
| 文件 | 内容 |
|---|---|
| `src/monitor.rs` (407) | 决策核心（纯函数带测试）：指标解析、服务状态、`plan` 自愈决策（restart 标记/essential 告警/冷却）、`scan_new_lines` 日志字面量扫告警（**只命中字面量罐头，不猜语义**） |
| `src/main.rs` (175) | 巡检主循环：15s/轮，冷却 20 轮，心跳/告警/诊断报告落盘 |
| `src/diagnose.rs` (154) | 诊断报告组装（纯函数带测试）：事件 + 现场 + 行动 + 日志尾部 |

> ⚠️ 整个 crate 的测试在 Windows 上**不参与**（`cfg(target_os="linux")` 门控），本机 `cargo test` 显示 0 项。
> 改这里必须跑交叉检查，否则等于没测。

### aether-shell（占位骨架）
- `aether-shell/src/main.rs`（11 行）— M2 职责当前由 aether-compositor 承担；**仍是有意保留的占位**，
  拆分与否是一个待决策项（拆出去能解掉 compositor 12.7k 行的体量，但会引入一层窗管/Shell 的跨进程协议）

### platform（系统构建）
| 文件 | 内容 |
|---|---|
| `build-iso.sh` | ISO 一键构建：musl 静态编译 → Buildroot → defconfig → ISO |
| `br2-external/configs/aetheros_defconfig` | Buildroot 配置：x86_64、initramfs、ISO9660、引导、busybox、e2fsprogs |
| `br2-external/Config.in` / `external.mk` / `external.desc` | 外部树（AETHER） |
| `br2-external/board/aether/linux.fragment` | 内核片断（PCNET32、fbdev/evdev、virtio 等） |
| `br2-external/grub-embedded.cfg` / `isolinux.cfg` | 两种引导方式的嵌入配置（构建期踩过的坑留在此处） |
| `overlay/init` | 内核首用户态：挂载伪文件系统 + lo 拉起 → `exec aether-init --pid1` |
| `overlay/etc/aether/services/*.json` | 服务定义（**实际 5 个文件**）：network / aetherd / compositor / ops / getty |
| `README.md` | 构建说明 |
| `build/` | 构建中间产物（空目录，产物不纳入版本控制） |

### 系统里到底有什么 / 能不能装软件（2026-09-28 实测）

实测自构建产物（`platform/build/output/target`，**41 MB**）：

| 内容 | 说明 |
|---|---|
| BusyBox | 约 200 个 applet，`/bin` 与 `/usr/bin` 基本全是它。含 wget / telnet / md5sum / less / top / lsof / fdisk / chroot |
| e2fsprogs | `mke2fs` / `fsck.ext4` / `tune2fs` / `dumpe2fs` —— 持久化分区要用 |
| grub + syslinux | 引导安装工具（`grub-install` 等） |
| **glibc** | `libc.so.6` + `ld-linux-x86-64.so.2`。**不是 musl** —— 这是"能不能跑外部二进制"的关键 |
| 自研 5 个二进制 | `aether-init` / `aetherd` / `aether-ops` / `aether-compositor` / `aether-install`，都在 `/usr/bin`（本身是 musl 静态链接） |
| 文泉驿微米黑 | compositor 在 Linux 下从中加载中文字形，缺了桌面就没字 |
| **没有** | 任何包管理器（opkg / apk / apt / rpm / pacman 全无）、编译器、解释器（无 gcc / python / perl） |

**结论：没有包管理器 —— 但有一套自己的应用安装机制**（`aetherd/src/apps.rs`）。

Buildroot 的模型是构建期定死、运行期不改系统，所以 opkg/apt/rpm 这类东西不在设计里。
取而代之的是"应用目录 + 清单"：`/var/apps/<id>/` 里放 `app.json` 与可执行文件，
`aetherd app install/list/remove` 管它，AI 侧也有 `app_list`/`app_install`/`app_remove`
三个工具（后两个 L2 走确认卡片）。包格式与边界见 `docs/APP-PACKAGES.md`。

**关键是装之前会预检依赖** —— 实测（`chroot` 进构建输出）证明跑不跑得起来取决于依赖库：

| 情况 | 结果 | 原因 |
|---|---|---|
| 静态链接的 `hello` | ✅ 跑通 | 不依赖共享库 |
| **宿主（Ubuntu 24.04，glibc 2.39）编的动态 `hello`** | ✅ **也跑通** | 它只用到 `GLIBC_2.2.5 / 2.3.4 / 2.34`，镜像的 glibc **2.38 向后兼容**盖得住 |
| 宿主 `/usr/bin/ls`（真实发行版程序） | ❌ 挂 | `libselinux.so.1: cannot open shared object file` —— **缺共享库** |
| 自研 5 个二进制 | ✅ | musl 静态 |

所以"动态链接的发行版二进制基本跑不起来"这个说法**不准确**：常见的失败原因是
**① 缺共享库**（发行版程序基本都要拖一串）、② 依赖新于 glibc 2.38 的符号。
镜像里没有 `ld.so.cache`，也没有任何依赖解析 —— 要装就只能静态链接，或把依赖一起带上。

给应用作者的三条建议：

1. **静态链接最省事** —— `gcc -static`、Rust 的 musl 静态、Go 默认输出，这些一定能跑
2. 动态链接就要看依赖：镜像里只有 glibc / libgcc / libblkid 等少数几个库，
   缺 `.so` 会报 `error while loading shared libraries`。注意 **glibc 版本不是问题**（向后兼容），
   缺库才是。要么静态，要么把依赖放进包的 `lib/`
3. 想加**常驻**软件（不是应用，而是系统组件）→ 改 `aetheros_defconfig` 加 `BR2_PACKAGE_*`
   重建 ISO，或丢进 `platform/overlay/`

文件系统布局：系统区只读（ISO9660 + initramfs 启动时整个读进内存）；
只有持久化分区（`/dev/vda2`，卷标 `AETHER`，挂到 `/var`）可写且跨重启保留。
`/init` 负责挂 `/proc`、`/sys`、`/dev`、拉起 `lo`，然后 `exec aether-init --pid1`。

**是 Linux 软件吗**：是 —— 内核是 Linux，用户态是 glibc 的 x86-64，能跑的就是 Linux ELF 二进制。

### scripts / docs
| 文件 | 内容 |
|---|---|
| `scripts/setup-vm.ps1` | Windows 宿主：VirtualBox 装 Ubuntu 24.04 构建机。密码读 `AETHER_VM_PASSWORD`（不设则安全提示输入），**脚本内不硬编码** |
| `scripts/transfer.py` | 上传到构建机。⚠️ 两点：Git Bash 下**必须** `MSYS2_ARG_CONV_EXCL="*"`（否则 `/home/...` 被 MSYS 改写成本地路径）；它会对文本做 **CRLF→LF 归一**，所以上传后 md5 必然与本地不同（差值 = CR 个数），**比对前要先去掉 CR**，否则会误判成"文件损坏" |
| `scripts/vm.py` | 构建机远程命令（`vm.py sh "<cmd>" [timeout]`）。凭据走 `AETHER_VM_PASSWORD` / `AETHER_VM_KEY`（**不再硬编码**）；主机密钥 TOFU 记入 `scripts/.vm_known_hosts` 后**严格校验**。⚠️ 命令里的 `$VAR` 会被**本地** bash 先展开，要用单引号或绝对路径，别写 `$PATH` |
| `scripts/vm-pull.py` | 从构建机取回文件，带重试（sftp 的 `SSHFX_NO_SUCH_FILE` 偶发）。同样需要 `MSYS2_ARG_CONV_EXCL="*"` |
| `scripts/vm-build-iso.sh` | 在构建机内跑 ISO 构建：补 `~/.cargo/bin` 到 PATH（**SSH 非交互会话没有它**）、输出落 `iso-build.log`、末尾打 `RESULT=PASS/FAIL` |
| `scripts/qmp-verify.py` / `qemu-verify.sh` / `qmp-slow-click.py` | QEMU QMP 键鼠注入 + 截图端到端验证 |
| `scripts/e2e-permission-confirm.py` | L2+ 权限链路端到端（需先起 `aetherd serve`） |
| `scripts/m6-run-install.py` / `m6-toolcall.py` / `upload-*.py` | M6 安装器端到端与构建机上传 |
| `scripts/shot-diff.py` | 截图像素回归（仅适用于"纯重构不应有视觉变化"的改动） |
| `scripts/bmp2png.py` | `--shot` 产物 BMP→PNG（仅标准库，供目视走查；**勿包成 .sh 调用**，见 ui-design-handover §11） |
| `scripts/png-crop.py` | 走查图裁剪 + 整数倍放大（1:1 检查边框/字重/图标比例） |
| `scripts/archive-ui-shots.py` | **归档走查图工具**：重建 `docs/host-ui-*.png` 全部 **10 张**；`--check` 为视觉回归门禁（屏蔽时钟/AI 光标非确定区，差异 > 0.02% 即失败） |
| `scripts/repo-stats.py` | **规模口径唯一来源**：逐 crate / 逐文件行数与文件数（口径 = `Cargo.toml` 的 workspace 成员 + `wc -l` 语义）。`--per-file` 出逐文件表，`--check` 比对 README/INDEX/roadmap 里声明的合计，不一致即非零退出（**可当门禁**） |
| `scripts/mkapp.py` | **应用打包器**（2026-09-29 新增）：把市面上的 Linux 程序打成 AetherOS 能装的包 —— 递归解析 ELF 依赖、**跳过 glibc 家族**、只收镜像里没有的库、按 `.gnu.version_r`/`.gnu.version_d` **校验符号版本**（含"镜像里的库版本符号对不上"这种坑）、按需带上 terminfo，产出包目录 + `.aep`（未压缩 tar）。`--check` 只回答"能不能跑"，`--selftest` 是解析器自测（17 项，跨平台可跑） |
| `scripts/serve-apps.py` | **只读分发服务**（2026-09-29 新增）：宿主起 HTTP 把 `dist/apps/*.aep` 喂给 guest（QEMU 用户态网络里宿主就是 `10.0.2.2`）。只实现 GET/HEAD、路径规范化防穿越、默认只绑回环、打印 sha256 供核对 |
| `docs/PHASE3-DECISION-2026-09-28.md` | Phase 3（Wayland）决策框架：本轮不做的理由与条件、**放弃条件** |
| `docs/WRITE-OPS-2026-09-28.md` | 4.1 可写文件操作：写白名单比读窄、回收站三重上限、权限模型约束 |
| `docs/PANIC-AUDIT-2026-09-28.md` | 0.8 关键路径 panic 审计（修 1 个 P2 越界） |
| `docs/LLM-E2E-2026-09-28.md` | 0.7 LLM 端到端：双假端点 21 项断言（协议层；真模型待接） |
| `docs/archive/CODE-REVIEW-2026-09.md` | **四轮代码审查总集**（原文 6 份共 1,686 行已移出，取回见文末）：各轮发现与处置、跨轮教训、未修/未验证清单、已钉住的边界 |
| `docs/archive/` | **历史快照目录**（决策历史，**不作现状依据**）：`PRODUCTION-PLAN-2026-09-27`、`PERF-REPORT-2026-09-26`（91 → 15.5 ms/帧推导）、`UNIMPLEMENTED-2026-09-27`、`ui-design-plan`、`image-gen-prompts` |
| `docs/ai-permissions.md` | AI 权限模型：L0-L3、审计、确认令牌 |
| `docs/roadmap.md` | 路线图 |
| `docs/HANDOVER.md` | 交接报告 v4（M4/M5/M6 实测记录 + 构建/验证手册） |
| `docs/ui-design-handover.md` | UI 设计系统交接 + 视觉质量冲刺 + 本机环境陷阱 + 双模主题（**视觉改动的权威依据**） |
| `docs/host-ui-*.png` | 主机 `--shot` 走查图 **10 张**（深空 5 / 明亮 5，含 ime 候选框两张。与实机 `screenshot-*.png` 区分） |

## 关键链路

```
【0】握手（必须先做）：UI ──RegisterUi──▶ aetherd:7311 ──▶ UiRegistered
        └─ 剪贴板与 ReloadConfig *要求已注册*；未注册连接一律拒（P1-1 的修复）

aether-compositor AI 指令条 ──Chat──▶ aetherd:7311
      │                              │ intent.rs 快速意图（规则，离线）
      │                              │ router.rs 路由（本地/云端）→ channel
      │                              │ llm.rs 请求 → tools.rs 执行（≤4 轮）
      │                              │ perm.rs 闸门(L0-L3) → aether-audit.log（带轮转）
      │◀──ChatChunk(channel) / Action / NeedsConfirmation(token)───┘
      │      └─顶栏三态(channel) └─布局/开应用/关窗口 └─确认弹窗→带令牌重发

aetherd:7311 ToolCall 无令牌 → NeedsConfirmation(L2 弹卡片 / L3 额外要求 confirm 回显目标)
      → UI 弹确认卡片 → 用户回显匹配 → 带 approval 令牌重发 → 过闸门执行
      （L2 写操作：file_write / file_delete / file_rename / trash_restore）

aetherd:7311 剪贴板： ClipboardSet{text} → ClipboardWritten   /   ClipboardGet → ClipboardText
aetherd:7311 配置热载：ReloadConfig → ConfigReloading → 进程退出 → 由 init 重新拉起

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
python scripts/archive-ui-shots.py [--check]                    # 归档走查图重建 / 视觉回归门禁（10 张）
cargo run -p aetherd -- chat "把窗口排成两列"                    # 单轮 agent（需 Ollama 或云端端点）
cargo run -p aetherd -- serve                                   # 常驻 IPC 服务（7311）
cargo run -p aetherd -- config --show                           # 模型配置（Key 脱敏）
cargo run -p aetherd -- config --api-key K --cloud-base URL      # 配置云端（也支持自建网关）
cargo run -p aether-init -- --dry-run ./platform/overlay/etc/aether/services  # 服务监督自检
python scripts/e2e-permission-confirm.py                        # 权限链路端到端（先起 aetherd serve）
cargo test --workspace --offline --no-fail-fast                 # 全部单元测试（本机 351 项，分布见「测试分布」）

# ⚠️ 改了 cfg(target_os="linux") 的代码后必须交叉检查：Windows 构建会整段屏蔽那些路径
cargo check --offline --target x86_64-unknown-linux-musl --all-targets -p aether-compositor -p aether-init -p aether-ops -p aether-install -p aether-ipc -p aether-shell
# ↑ 这 6 个 crate 在开发机上就能查（不是只能编译检查：连 Linux 专属的测试代码一起编）。
#   2026-09-29 实测：0 警告 0 错误 —— 含 Windows 上**整段不编译**的 aether-ops。
# aetherd 查不了：ureq→ring 要 x86_64-linux-musl-gcc 交叉 C 编译器，本机没有（环境限制，非代码问题）。
#   它有 Linux 门控代码（tools.rs 白名单等），改了必须上构建机：
#   /home/aether/.cargo/bin/cargo check --offline --target x86_64-unknown-linux-musl
```

## 键盘

| 按键 | 作用 |
|---|---|
| `Tab` | 轮换活动窗口（焦点） |
| `Ctrl+W` | 关闭活动窗口 |
| `Ctrl+Space` | **中英输入切换**（IME 默认关；见 `ime.rs`） |
| `方向键` | 文件网格移动选择（上下按行、左右按格）；终端聚焦时交给 shell |
| `PgUp` / `PgDn` | 文件网格整页翻动 |
| `Home` / `End` | 跳到首/末条目 |
| `1`–`4` | 切换布局（按住 Shift 时让位给上档符号 `!@#$`） |
| `Esc` | 关闭浮层 / **取消正在拼的中文** |
| `Enter` / `Backspace` | 确认 / 退格（拼字中：提交 / 删一个字母） |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | 复制 / 粘贴（终端内容、文件完整路径、AI 指令条） |
| `拖窗口边缘` | 缩放窗口（上边不参与：那是标题栏，归拖动） |
| 终端内的 `Ctrl+C` | 送给 shell（中断命令）——所以复制用 `Ctrl+Shift+C` |
| `Delete` | 删除文件管理器里选中的项。**走 L2 确认链路**：`request_delete` 发一条不带令牌的 `file_delete` ToolCall → 服务端判 L2 → 弹确认卡片 → 点「允许一次」才真删（`main.rs::request_delete`）。⚠️ 只在文件管理器聚焦、且指令条为空时生效；**没有做过实机按键验证** |

> 修饰键状态在输入层统一跟踪（`input::Mods`）。**键盘归属**：终端聚焦时按键归 shell，
> 否则 `Ctrl+C` 会被误判成关窗 —— 这是 `Ctrl+Shift+C` 存在的原因。

## IME（中文输入，2026-09-28）

`Ctrl+Space` 切换，**默认关**（避免打断英文输入）。自建词表 ~100 条，拼音前缀匹配 →
候选框 → 数字键选词 / 空格提交 / 上下选择 / 退格。无候选时**拼音原样上屏**（不丢字）。

- 已验证：走查图 `docs/host-ui-ime.png` / `host-ui-light-ime.png`（归档门禁内）
- **指令条与终端都已接**：`feed_terminal(desktop, key, &mut ime)` 处理拼字中的数字选词 /
  空格提交 / 退格删拼音，**未被 IME 吃掉的字符才送 PTY**（`3a34f49`）；Esc 取消拼字同轮接上
- ⚠️ 终端里的中文输入**只做过编译与源码级确认，没有实机键盘交互验证**（`--shot` 出静态帧，测不了输入）

## 测试分布（2026-09-29 实测：**Windows 351 全绿；双目标零警告**；Linux 361 为推算，见下）

> Linux 列的 333 是**按 `aetherd` 增量推算**的（`llm.rs` 新增 5 项，该文件无平台门控：
> 328 + 5 = 333），**不是构建机实测值** —— 引用前先上构建机跑一遍。

**两个目标都要跑** —— 不是可选项。`cfg(target_os="linux")` 门控的代码在 Windows 上
整段不编译，**Linux 侧的问题在开发机上一次都发现不了**。实测踩到两次：
`aether-ops` 的 `ServiceStatus` 少两个字段（Windows 全绿、Linux 直接编译失败），
以及 3 条只在 Linux 出现的警告（`aether-ops` 全 crate 在 Windows 上不编译）。

| crate | Windows | Linux | 备注 |
|---|---|---|---|
| `aether-compositor` | 208 | 204 | 差的 4 项是演示脚本解析测试，标了 `#[cfg(not(target_os="linux"))]` —— Linux 下 `Terminal::spawn` 开的是**真 PTY**，没有演示脚本可解析（终端行为改由实机验证覆盖） |
| `aether-ops` | 0 | **13** | 整个 crate 是 Linux 专属，Windows 不参与 |
| `aetherd` | 110 | 111 | 含 `apps.rs` 的 22 项（清单校验/目录穿越/符号链接拒绝/ELF 解析/**递归依赖闭包**/**terminfo 判定**/**glibc 家族拦截**/**库名穿越防护**/包装脚本）+ `llm.rs` 的 5 项（错误路径必须带出服务器正文；本地假端点，不联网） |
| `aether-init` | 17 | 17 | 含 `shipped_essential_services_must_be_restartable`（见下） |
| `aether-install` | 9 | 9 | |
| `aether-ipc` | 7 | 7 | |
| `aether-shell` | 0 | 0 | 占位 |
| **合计** | **351** | **361** | |

跑 Linux 侧的方式：在构建机上 `cargo test --workspace --offline --no-fail-fast`
（Rust 不在 SSH 非交互 PATH 里，用 `/home/aether/.cargo/bin/cargo`）。

> **端到端**：`scripts/e2e-permission-confirm.py`（权限链路，需先起 `aetherd serve`）、
> `scripts/m6-run-install.py`（安装器）。Wayland socket 与 `SCM_RIGHTS` 收包**只能在真机验证**。
>
> ⚠️ 测试数在 09-28 一天内从 185 → 300+，其中约 60 项来自 Wayland spike。
> 引用具体数字前先跑一遍 —— 这一天里 HEAD 动了 15 个提交。
