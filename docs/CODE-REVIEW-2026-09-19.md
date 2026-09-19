# AetherOS 代码审查报告

日期：2026-09-19
范围：`aether-ipc` / `aetherd` / `aether-init` / `aether-ops` / `aether-compositor` / `aether-shell` / `aether-install` / `platform` / `scripts`
代码量：6112 行 Rust（7 个 crate）
方法：全量源码逐文件阅读 + `cargo check --workspace --all-targets` + `cargo test --workspace` + `cargo clippy` + 实机运行 `aetherd serve` 做 IPC 端到端复现

## 0. 构建与测试基线

| 检查项 | 结果 |
|---|---|
| `cargo check --workspace --all-targets` | 通过，仅 dead_code / unused_imports 警告 |
| `cargo clippy --workspace --all-targets` | 无 error，仅风格类 warning |
| `cargo test --workspace` | **失败 1 项**：`aether-init::manager::tests::supervision_restarts_and_backs_off` |

失败为确定性失败（连续 3 次均失败），非偶发抖动。根因见 P2-11。

---

## 1. 严重（P0）——建议发布前必修

### P0-1 `install_disk` 权限等级错标为 L1，整盘擦除零确认

- 位置：`aetherd/src/tools.rs:96-108`（`level: Level::L1`）、`aetherd/src/perm.rs:61`（`auto_approve_below: Level::L2`）、`aetherd/src/perm.rs:71-76`（`judge`）
- 逻辑：`judge` 的判定是 `level >= auto_approve_below && !approved` 才要求确认。L1 < L2，所以 L1 工具**永远直接执行**。
- 实证（本次实机复现）：向运行中的 `aetherd serve` 发送
  `ToolCall{tool:"install_disk", arguments:{disk:"/etc/passwd"}}`，返回的不是 `NEEDS_CONFIRMATION`，而是工具内部的路径校验错误 —— 说明请求已经穿过权限闸门进入工具体。审计日志同时落盘：
  ```
  1789816538	L1 可逆写	install_disk	{"disk":"/etc/passwd"}	allowed
  1789816538	L1 可逆写	install_disk	{"disk":"/dev/__bogus__"}	allowed
  ```
- 影响：`install_disk` 的自我描述是"整盘覆盖，目标盘数据将丢失"，属于**不可逆**操作。但它在权限模型里被标成"L1 可逆写"，AI（或被提示词注入操纵的 AI）可以一句话抹掉整块磁盘，无确认、无二次确认、无确认短语。
- 这直接违反项目自己的规范：`docs/ai-permissions.md:12` 明确 "L3 危险 | 格式化…| 双重确认 + 输入确认短语"，`:10` 明确 "L1 可逆写 | 改壁纸、开/关服务、移动窗口"。
- 修复：`install_disk` 改为 `Level::L3`；并在 `tool_install_disk` 内要求显式 approval token + 目标盘设备名回显确认（"输入 /dev/vda 以确认"）。

### P0-2 工具失败被上报为成功，安装失败在界面上显示为"完成"

- 位置：`aetherd/src/server.rs:62-64`
  ```rust
  let output = tools::execute(gate, &mut ctx, &tool, &arguments, false)
      .unwrap_or_else(|e| format!("[工具错误] {e}"));
  let mut out = vec![Response::ToolResult { tool, ok: true, output }];  // ok 恒为 true
  ```
- 下游：`aether-compositor/src/main.rs:482` 用 `ok` 判定成败 → `if ok => Ok(output)` → `InstallEvent::Done` → `draw.rs:859` 渲染绿色 `完成 · 重启后从磁盘引导`
- 实证（本次实机复现）：
  ```
  未知工具        → {"tool":"no_such_tool","ok":true,"output":"[工具错误] 未知工具: no_such_tool"}
  非法磁盘路径    → {"tool":"install_disk","ok":true,"output":"[工具错误] 磁盘参数必须…拒绝: /etc/passwd"}
  不存在的盘      → {"tool":"install_disk","ok":true,"output":"[工具错误] 磁盘参数必须…拒绝: /dev/__bogus__"}
  ```
- 影响：安装失败时用户看到的是绿色"安装完成，重启后从磁盘引导"，拔盘重启直接黑屏。这是最容易被用户实际踩到的缺陷。
- 修复：`ok` 应取 `execute(...).is_ok()`；失败走 `ok:false` 或 `Response::Error`。合成器侧同步改为按 `ok` 分流。

### P0-3 出厂 ISO 把 AI 中枢暴露到全网卡，且该端口可触发整盘擦除

- 位置：`platform/overlay/init`
  ```sh
  # NAT 虚拟机环境：允许宿主经 QEMU/VBox hostfwd 直连 AI 中枢做调试与自动化
  export AETHER_BIND=0.0.0.0
  ```
  配合 `aetherd/src/server.rs:15-16`（`AETHER_BIND` 直接作为 bind 地址），且整条 IPC 协议**没有任何认证/授权**。
- 影响链：网络内任意主机 → 一行 NDJSON → `ToolCall{install_disk}` → 因 P0-1 免确认 → 整盘覆盖。此外 `read_file` 工具无路径限制（`tools.rs:155-162`），可读任意文件；`sys_probe` 可执行 `ps aux` 等。
- 修复：发行镜像改回仅回环；调试需要时走 Unix socket + `SO_PEERCRED` 校验，或加一次性 token。至少把 `AETHER_BIND` 从 overlay 里移除，改由调试脚本注入。

---

## 2. 高（P1）

### P1-1 服务定义加载失败 → 静默以空服务集启动

- 位置：`aether-init/src/unit.rs:144-162`（任一 JSON 解析/校验失败即整个 `load_dir` 返回 `Err`）、`aether-init/src/main.rs:74-77`（`unwrap_or_else` 吞掉错误，用 `Vec::new()` 继续）
- 影响：`/etc/aether/services/` 里任何一个文件写坏，开机后 aetherd、compositor、network **一个都不启动**，系统黑屏，只有一行 `警告: 加载服务定义失败`。这是"单点配置错误导致整机不可用"。
- 修复：单个文件失败只跳过并记录文件名；空服务集视为致命错误（PID 1 应显式报错/进救援模式），不要静默继续。

### P1-2 `essential: true` 字段完全未使用

- 位置：`aether-init/src/unit.rs:120` 定义，`platform/overlay/etc/aether/services/*.json` 中 3 个服务都标了 `essential: true`
- 全仓 grep：除定义与 JSON 外**无任何读取点**
- 影响：配置项是假的。关键服务（network/aetherd/compositor）崩溃不会触发任何差异化处理（无告警升级、无系统级响应）。

### P1-3 两套监督者对 `restart: false` 的处理互相矛盾

- 位置：`aether-init/src/manager.rs:179`（init 遵守 `svc.spec.restart`）；`aether-ops/src/monitor.rs:58-79`（ops 只看服务状态，`ServiceStatus` 里根本没有 restart 字段）
- 实例：`platform/overlay/etc/aether/services/compositor.json` 明确写了 `"restart": false`，但 compositor 崩溃后 ops 仍会通过 7312 把它拉起来。
- 影响：配置意图失效；两套监督者（init 退避重启 + ops 冷却重启）对同一服务各自决策，行为不可预测。

### P1-4 自动重启路径丢失日志管道，自愈链随之断掉

- 位置：`aether-init/src/manager.rs:194-198`（`tick` 里的重启只 `command.spawn()`，**没有** `.stdout(piped).stderr(piped)`，也没有调用 `logtee::tee_child`）
- 对比：`manager.rs:126-133` 的首次 `start()` 是接管道 + tee 的
- 影响：服务第一次启动的日志进 `/var/log/aether/<unit>.log`，之后每次自动重启的日志全部直落 PID 1 的 stdio。而 `aether-ops` 的异常检测**完全依赖**扫描这些日志文件（`monitor.rs:190-225`），因此"重启后的崩溃"ops 永远看不到 —— 日志告警与诊断报告这条自愈链在第一次重启后就失效了。

### P1-5 spawn 失败形成 200ms 级重试风暴（退避失效）

- 位置：`aether-init/src/manager.rs:179-206`
- 逻辑缺陷：重启分支里 spawn 失败时既不更新 `last_exit`，`restarts` 也不自增。而 `ready` 的判定是 `last_exit.map(|t| now.duration_since(t) >= backoff).unwrap_or(true)`。
- 结果：spawn 失败 → 下一 tick（200ms 后）`last_exit` 仍是旧值、退避早已过期 → 立即重试，如此循环。**永不退避**。
- 影响：服务二进制缺失（如 ISO 里漏拷）时，PID 1 每 200ms 反复 fork/失败/刷日志，CPU 空转。
- 修复：spawn 失败也要写 `last_exit` 并 `restarts += 1`。

### P1-6 PID 1 不回收非亲生子进程，僵尸永久堆积

- 位置：`aether-init/src/manager.rs:154-177`，`tick` 只 `try_wait` 自己 `child` 字段记录的进程
- 影响：作为 PID 1，Linux 会把所有孤儿进程（双 fork 的 daemon、服务自己起的子进程）重新挂到 init 名下。这些进程退出后无人 `wait`，会永久停留为僵尸，且数量只增不减。
- 修复：PID 1 需要循环 `waitpid(-1, WNOHANG)` 兜底收割所有子进程（同时忽略 SIGCHLD 默认行为）。

### P1-7 PID 1 多处 `expect("毒锁")`，panic 即内核 panic

- 位置：`aether-init/src/main.rs:92`、`aether-init/src/ipc.rs:50`、`aether-init/src/ipc.rs:81`
- 链路：任意持锁线程 panic → Mutex 被毒化 → 监督主循环 `expect` panic → PID 1 退出 → 内核 panic，整机重启。
- 修复：用 `lock().unwrap_or_else(|e| e.into_inner())` 恢复，或改用 `parking_lot`（不投毒）。

---

## 3. 中（P2）

### P2-1 `aether-init` 服务控制 IPC 无认证

`ipc.rs:11` 只绑回环，但本机任意用户都能连上 7312 执行 `ServiceControl{Stop, unit:"network"}`。在"PID 1 服务管理"这种权限高度敏感的边界上，建议改 Unix socket + 权限位或 `SO_PEERCRED`。

### P2-2 Monocle 布局是空操作（功能与文案不符）

- `aether-compositor/src/layout.rs:55`：`Layout::Float | Layout::Monocle => vec![None; n]`
- `main.rs:276-278`：`if lay != Float && !win.floating { win.target = tiled[i] }` → Monocle 下 `tiled[i]` 恒为 `None`，不设置任何目标
- 但对外承诺是"独占堆叠 / 铺满 / 最大化桌面"（`intent.rs:36-38` 的快速意图、`text.rs:187` 的菜单项、`layout.rs:21-22` 的文档注释）
- 结果：用户或 AI 说"铺满"，toast 提示"已切换为独占堆叠"，窗口纹丝不动。Monocle 与 Float 行为完全等价。

### P2-3 拖拽期间窗口数组变化 → 索引越界 panic

- `main.rs:208-217`（按下分支）与 `main.rs:119`（抬起分支）都用 `drag` 中保存的索引直接索引 `desktop.wins[i]`
- 但 AI 的 `close_active`（`main.rs:581-588`）会 `wins.pop()`；`apply_action` 在步骤 2 执行，早于步骤 3 的拖拽处理
- 结果：拖拽中的窗口恰好是被关闭的那个 → `i == len` → panic → 合成器进程死亡。而 `compositor.json` 是 `"restart": false`，加上 P1-3，实际是否被拉起取决于 ops，行为不确定。
- 修复：索引访问统一走 `get_mut(i)`；窗口增删时清空 `drag`。

### P2-4 安装向导 Done 后按钮仍可点击，会二次整盘擦除

- `draw.rs:855-865`：Done 阶段按钮文案变成"完成 · 重启后从磁盘引导"，但按钮命中区照常登记
- `main.rs:201-205`：点击处理只判 `!installer.running`，不判阶段 → 再点一下又发起一次 `install_disk`（dd 重新覆盖一遍）
- 修复：Done 阶段禁用按钮或改为"重新安装需再次确认"。

### P2-5 LLM 多工具调用导致下一轮请求非法

- `aetherd/src/main.rs:101-133`：只执行 `calls[0]`，但把携带**全部** `tool_calls` 的 assistant 消息 push 进历史（`main.rs:106`），只补了一条 `role:"tool"` 结果
- OpenAI 兼容端点（GLM 会校验）要求每个 `tool_call.id` 都有对应 tool 消息 → 400 报错，agent 循环中断
- 附带：`main.rs:107` 若 `tool_calls` 是空数组 `[]`，会 `break` 并返回"（已达本轮工具调用上限…）"，文案与实际原因不符

### P2-6 `read_line` 无长度上限（内存耗尽）

`aetherd/src/server.rs:43` 与 `aether-init/src/ipc.rs:34` 都用 `BufReader::read_line` 读客户端输入，无上限。单连接发一条超长行即可吃光内存；配合 P0-3 的 `0.0.0.0` 暴露，可远程触发。

### P2-7 审计失败被静默吞掉

- `perm.rs:78` 注释承诺："审计失败不阻断执行，但会显式报错给调用方记录"
- 实际 `tools.rs:125`：`let _ = gate.audit(...)` —— 错误被丢弃
- 影响：`/var/log/aether/` 不可写时（本次在 Windows 下实测路径解析为 `D:\var\log\aether\`），所有 AI 操作零留痕，且没有任何一方知道。对一个"AI 可操作整机"的系统，审计失效必须是显式告警事件。

### P2-8 `monitor.rs:211` 多余且致命的 `.expect("log reopen")`

同一循环内 `monitor.rs:201` 已经打开过该文件，211 行又 `File::open` 一次并 `.expect`。日志被轮转/删除的瞬间、或权限变化时直接 panic，aether-ops 退出（再由 init 拉起，形成周期性崩溃）。应复用已打开的文件句柄，并把错误降级为跳过。

### P2-9 `scan_dir` 用读取前的旧 `len` 作为新 offset → 重复告警

`monitor.rs:202-219`：`len` 取自 `metadata()`，随后才 `read_to_string`。若两者之间文件增长，`*offset = len` 记录的是旧长度，下一轮会重复扫描同一段 → 重复告警 + 重复诊断报告落盘。

### P2-10 `plan_persist_partition` 静默截断扇区数

`aether-install/src/main.rs:200`：`let sectors = (total_sectors - start as u64) as u32;`。>2TB 磁盘上 `total_sectors` 超过 `u32::MAX`，截断后写入 MBR 的是一个**错误的**分区大小，而不是显式报错。（MBR 分区表本就表达不了 >2TB，应直接 `bail!`。）

### P2-11 监督重启测试确定性失败（Windows 平台）

- 位置：`aether-init/src/manager.rs:289-314`，失败断言在 `manager.rs:300`
- 根因（已实测）：测试假设 `m.start("noop")` 后 `sleep(150ms)` 子进程即可被收割。而 Windows 上 `Noop` 走的是 `cmd /C exit 0`（`unit.rs:100-105`），实测该进程需 **600–850ms** 才变为可回收：
  ```
  trial0: reapable after 844.1 ms, rc=0
  trial1: reapable after 656.8 ms, rc=0
  trial2: reapable after 600.2 ms, rc=0
  ```
  150ms 内 `try_wait()` 返回 `Ok(None)` → 状态仍是 `Running` → 断言失败。
- 这是本项目在受支持开发平台（`rust-toolchain.toml` 明确说明 Windows 会解析为 `x86_64-pc-windows-gnu`）上的**确定性失败**，不是偶发抖动。
- 修复：不要硬编码 sleep，改为带截止时间的轮询（例如每 10ms 检查一次、最多 3s），并让断言在超时后给出状态诊断信息。

### P2-12 目标盘未校验"不等于安装源"

`aether-install` 的 `--disk /dev/sr0` 能通过全部校验（`valid_disk_path` 只要求字母数字、`is_block_device` 为真、容量足够），随后变成 `dd if=/dev/sr0 of=/dev/sr0` 自读自写。建议显式拒绝 `disk == SOURCE`。

### P2-13 `fbdev::blit` 忽略 stride

`fbdev.rs:112-116` 读取并打印了 `fix.line_length`，但 `blit` 逐行按 `fw * bytes_pp` 紧密排布写缓冲。当驱动给出的 `line_length != width * bpp / 8`（某些 VESA/DRM 模拟）时，画面会逐行错位。当前 1280 宽 32bpp 环境恰好相等，故未暴露。

### P2-14 `FBIOPUT_VSCREENINFO` 未检查前置 GET 是否成功

`fbdev.rs:95-106`：若 `FBIOGET_VSCREENINFO` 失败（`rc_v != 0`），`var` 仍是全 0，代码照样置 `activate` 并回写，等于请求把显示模式设成 0x0。应在 `rc_v == 0` 时才执行 PUT。

### P2-15 `fill_rect` 不处理负宽/负高

`draw.rs:76-86`：`((rect.x + rect.w) as usize).min(w)` 在 `rect.x + rect.w < 0` 时，`as usize` 先变成巨值，`.min(w)` 再钳到 `w`，于是**负宽度被渲染成整行填充**而不是空操作。当前布局算法取不到负值，但 `layout::work_area` 在 `h < 158` 时返回负 `h`（`layout.rs:39-46`），小分辨率 framebuffer 上会整屏刷色。建议在 `fill_rect` 入口 `if rect.w <= 0 || rect.h <= 0 { return }`。

### P2-16 英文关键词子串误命中

`aetherd/src/intent.rs:30-54` 用 `contains` 匹配 `"time"` / `"two"` / `"three"` / `"float"` / `"status"` / `"tidy"`。例如：

- "解释一下 time complexity" → 被当成问时间，返回 `现在是 HH:MM`
- "把 float 变量改成 int" → 被当成布局指令，直接切换自由布局

建议英文关键词改为按词边界匹配，或直接移除这些过宽的英文触发词。

---

## 4. 低（P3）/ 代码质量

| # | 位置 | 问题 |
|---|---|---|
| P3-1 | `manager.rs:180-181` | `restarts` 永不归零：服务稳定运行数天后，一旦重启仍按 6 次退避（32s）等待 |
| P3-2 | `aetherd/src/main.rs:135` | 达到 MAX_ROUNDS 与"模型返回空 tool_calls 数组"走同一分支，返回文案"已达本轮工具调用上限"与实际不符；且此时已入队的 `desktop_actions` 被丢弃 |
| P3-3 | `compositor/main.rs:577` | `apply_action` 的 `open_app` 忽略 `open_app()` 返回值，达到 8 窗口上限时仍提示"已打开「X」" |
| P3-4 | `compositor/main.rs:863-866` | `open_menu.filter(\|_\| true)` 是无意义代码 |
| P3-5 | `logtee.rs` | 日志追加写无轮转、无大小上限，长期运行会撑满持久化分区 |
| P3-6 | `docs/ai-permissions.md:17` | 文档写审计路径 `/var/log/aether-audit.log`，代码是 `/var/log/aether/aether-audit.log` |
| P3-7 | `docs/ai-permissions.md:18` | 文档承诺 L1 有"撤销栈"，代码中无实现（`docs/roadmap.md:59` 也未勾选） |
| P3-8 | `aether-ops/src/main.rs:5` | 注释写日志在 `/tmp/log/*.log`，实际是 `/var/log/aether`（`monitor.rs:157`） |
| P3-9 | `fbdev.rs:139` | 每帧新建约 3.9MB `Vec` 并逐像素转换（10fps ≈ 39MB/s 分配），应复用缓冲 |
| P3-10 | `server.rs:24-33` | 每连接一线程、无连接数上限、无读超时，缺少基本资源约束 |
| P3-11 | 多个文件 | 死代码/警告噪音：`persist.rs` 的 `VAR_DIRS`/`boot_record_line`/`count_boot_records`/`mount_persist`（非 Linux 未使用）、`parse_mem_mb`/`parse_uptime_secs`（仅测试用）、`draw_cursor`、`InstallerPhase` 变体、`main.rs:20` 的 unused import。建议按平台/测试条件收敛，保持 `cargo build` 零警告，否则真实警告会被淹没 |
| P3-12 | 全局 | 6112 行代码仅 43 个单元测试，且全部集中在纯逻辑层。IPC 服务、安装器真实流程、合成器主循环、ops 巡检循环**均无端到端测试**；唯一的监督逻辑测试还是失败的（P2-11） |

---

## 5. 建议修复顺序

1. **P0-1 + P0-3**：`install_disk` 提到 L3 + 移除出厂镜像的 `AETHER_BIND=0.0.0.0`（一个改等级、一个删一行，成本极低、风险收益比最高）
2. **P0-2**：`ok` 取真实结果（一行改动，直接消除"失败显示成功"）
3. **P2-11**：修掉确定性失败的测试，恢复 `cargo test` 绿色基线
4. **P1-4 + P1-5 + P1-6**：init 监督循环三处缺陷（丢日志、退避失效、不收割孤儿）一起改，都在 `manager.rs` 的 `tick`/重启路径
5. **P1-1 + P1-2 + P1-3**：服务定义健壮性 + `essential` 落地 + 统一 restart 策略
6. **P1-7 + P2-1 + P2-6**：PID 1 锁健壮性 + IPC 认证 + 输入长度上限
7. 其余 P2/P3 按迭代排期

---

## 6. 未覆盖 / 待确认

- 未在真实 Linux VM / QEMU 内运行（本次审查在 Windows 开发环境），因此 `fbdev`、`evdev`、`persist`、`aether-init --pid1`、`aether-install` 的**运行时**行为仅通过源码静态分析判断，未做实机验证
- `platform/br2-external/*`（Buildroot defconfig、kernel fragment、grub/isolinux 配置）未逐行审查
- `scripts/` 下 53 个脚本做了破坏性命令模式扫描（`rm -rf` 目标均为 buildroot 构建产物目录，未见对用户数据/系统目录的危险操作），未逐个通读
- ISO 内的引导链（isohybrid MBR → GRUB/syslinux → kernel → /init）未做实测验证

---

## 7. 修复记录（2026-09-19，Windows 开发环境）

基线：`cargo check --workspace --all-targets` 零警告；`cargo test --workspace` 58 项全绿（P2-11 恢复）；`cargo clippy` 无 error（剩余 14 条为 compositor 文字渲染函数的既有风格类 warning）。

| # | 状态 | 修复方式 |
|---|---|---|
| P0-1 | ✅ | `install_disk` → `Level::L3`（未批准时闸门直接拦下，不进工具体）；新增 `confirm` 参数须与 `disk` 完全一致的回显确认；UI 直连的 `ToolCall`（安装向导，用户已在向导里确认）按协议语义视为已授权 |
| P0-2 | ✅ | `server.rs` ToolCall 的 `ok` 取真实执行结果；失败不再下发 Action |
| P0-3 | ✅ | 移除 `platform/overlay/init` 的 `AETHER_BIND=0.0.0.0`；宿主调试改由脚本显式注入 |
| P1-1 | ✅ | `load_dir` 单文件损坏只跳过并告警；空服务集在 PID 1 下进入救援模式（保活不退出），dry-run 显式报错 |
| P1-2 | ✅ | `ServiceStatus` 增加 `essential`/`restart` 字段；ops 对关键服务异常输出升级告警行 |
| P1-3 | ✅ | 重启策略以 init 服务定义为唯一事实来源：ops 只对 `restart: true` 的服务冷拉起 |
| P1-4 | ✅ | 抽出统一启动原语 `spawn_with_logs`（管道 + tee），自动重启与首次启动同路 |
| P1-5 | ✅ | spawn 失败同样推进 `last_exit` 与 `restarts`（有回归测试 `spawn_failure_backs_off`）；`start()` 失败转入 Failed 态交监督循环接管 |
| P1-6 | ✅ | `Manager::tick` 内 Linux 专用 `waitpid(-1, WNOHANG)` 循环兜底收割，受管子进程按 pid 匹配记账，避免与 `try_wait` 竞争"偷收" |
| P1-7 | ✅ | 主循环与 IPC 全部改 `lock().unwrap_or_else(\|e\| e.into_inner())` 毒锁恢复 |
| P2-1 | ✅ | init 服务控制通道 Linux 下改 Unix socket `/run/aether-init.sock`（0600，root-only）；aetherd / aether-ops 客户端同步切换 |
| P2-2 | ✅ | Monocle = 全部窗口占满工作区 + 点击置顶堆叠（有回归测试） |
| P2-3 | ✅ | 拖拽/抬起索引改 `get_mut`，失效即取消拖拽（主机预览路径同修） |
| P2-4 | ✅ | Done 阶段按钮置灰且 `begin_install` 拒绝再次发起（失败后仍可重试） |
| P2-5 | ✅ | agent 循环为每个 `tool_call` 补对应 tool 消息；空 `tool_calls` 数组按最终回答处理，不再误报"调用上限" |
| P2-6 | ✅ | 两处 `read_line` 加 `take()` 长度上限（aetherd 1MB / init 64KB）+ 读超时 |
| P2-7 | ✅ | 审计失败显式返回调用方并落 stderr，成功输出前缀警告行 |
| P2-8 | ✅ | 复用已打开句柄，去掉 `.expect("log reopen")`，失败降级为跳过 |
| P2-9 | ✅ | offset 按实际读取字节数推进，metadata 与读取间的文件增长不再重复扫描 |
| P2-10 | ✅ | `plan_persist_partition` 对 >2TB（超出 u32::MAX 扇区）返回 None 放弃持久化，不再截断扇区数（有回归测试） |
| P2-11 | ✅ | 监督测试改为带截止时间的轮询（10ms 间隔 / 5s 上限），Windows `cmd /C` 600–850ms 收割延迟不再致假失败 |
| P2-12 | ✅ | `--disk` 等于安装源（`/dev/sr0`）显式拒绝 |
| P2-13 | ✅ | `fbdev::blit` 逐行按硬件 stride 排布，padding 保持黑色 |
| P2-14 | ✅ | `FBIOGET_VSCREENINFO` 失败时不执行 PUT（避免请求 0x0 模式） |
| P2-15 | ✅ | `fill_rect` 入口拒绝负宽/高 |
| P2-16 | ✅ | 英文触发词改为完整短语/整词匹配（`what time`/`monocle` 等），移除裸 `time`/`float`/`two`/`three`/`status` 子串（有回归测试） |
| P3-1 | ✅ | 服务稳定运行 ≥60s 后重启计数归零 |
| P3-2 | ✅ | 轮次耗尽时保留已入队的 desktop_actions（随 P2-5 一并修复） |
| P3-3 | ✅ | `apply_action` 的 open_app 传递上限/拒绝提示 |
| P3-4 | ✅ | 删除无意义的 `filter(\|_\| true)` |
| P3-5 | ✅ | logtee 超过 8MB 轮转为 `.log.1` |
| P3-6 | ✅ | 文档审计路径改为 `/var/log/aether/aether-audit.log` |
| P3-7 | ✅ | 文档标注撤销栈/熔断/注入防线为"规划中，未实现" |
| P3-8 | ✅ | ops 注释/日志更正为 Unix socket 与 `/var/log/aether` |
| P3-9 | ✅ | fbdev 复用帧缓冲，消除每帧 ~3.9MB 分配 |
| P3-10 | ✅ | 连接数上限 32 + 空闲读超时 600s |
| P3-11 | ✅ | 死代码按平台条件收敛，Windows 构建 `cargo check` 零警告 |
| P3-12 | ◐ | 测试 43 → 58 项（新增权限闸门、监督退避、布局、意图回归等）；IPC/安装器/合成器端到端测试仍缺，随实机验证补 |

待办（下轮）：
1. Linux VM 实机回归：fbdev stride/modeset、init 救援模式、Unix socket 权限、安装全链路（本报告第 6 节的未覆盖项仍全部有效）
2. `platform/br2-external/*` 与 ISO 引导链实测
3. 审计"显式上报"在合成器侧的 UI 呈现（当前仅回到 ToolResult 文本）
