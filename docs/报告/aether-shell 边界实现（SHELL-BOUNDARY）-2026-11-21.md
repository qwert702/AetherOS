# aether-shell 边界实现（SHELL-BOUNDARY）— 2026-11-21

> 对应 ADR：[`../架构决策（ADR）-aether-shell归属-2026-11-13.md`](../架构决策（ADR）-aether-shell归属-2026-11-13.md)
> 任务：E2「aether-shell 归属决策的实施」。把 11 行空占位改成边界明确、可运行、可测试的最小实现；**不拆分**。

## 一、Crate 定位

- `aether-shell` 是 **保留命名空间（reserved namespace）**，不是正在开发的自研 Shell。
- Shell 运行时职责（顶栏、Dock、工作区、AI 指令条）实际由 `aether-compositor` 承担；本 crate 不实现这些界面与交互。
- 本 crate 只做一件事：把 ADR 里的归属边界事实集中到库层（`ShellOwnership` 结构 + `ownership()`），通过 `--status` / `--describe` 子命令对外说明，并由单元测试锁定这些事实，避免归属说法散落在打印字符串里。
- 代码仅依赖 `std`，未新增 crate 依赖；Windows / Linux 均可编译运行。

## 二、改动文件

| 文件 | 改动 |
|---|---|
| `aether-shell/src/lib.rs` | 新增：`ShellOwnership` 结构、归属常量、`ownership()`、`parse_args()` / `exit_code()` / `describe_text()` 与单元测试 |
| `aether-shell/src/main.rs` | 重写：解析 `--status` / `--describe` / `--help` / `-h`，未知参数退出码 2；事实文本来自库层 |
| `aether-shell/Cargo.toml` | 仅更新 `description`（原描述"自研 Shell：顶栏/启动器…"与 ADR 口径矛盾）；依赖声明不动，避免改动 `Cargo.lock` |

未触碰：`aether-compositor/**`、`docs/README.md`、`.github/`、根 `Cargo.toml` / `Cargo.lock`。

## 三、`--status` 输出样例

### Linux 预检（云主机，stable x86_64-unknown-linux-gnu，离线构建）

```
aether-shell — Shell 归属边界
Shell 职责归属: aether-compositor
本 crate 定位: 保留命名空间（reserved namespace）；暂不拆分，本 crate 不实现 Shell 运行时职责
再评估时机: M10 平台化阶段
再评估触发条件:
  - 出现第三方 Shell 需求
  - 出现多会话 / 多用户会话需求
  - 出现独立 Wayland 会话需求
ADR: docs/架构决策（ADR）-aether-shell归属-2026-11-13.md（决策日期 2026-11-13）
```

### Windows 复跑（待本机恢复后回填）

命令：

```powershell
& "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu run -p aether-shell --locked -- --status
```

> 预期输出与上节一致（纯 std、无平台相关分支）；本机离线期间尚未取得 Windows 真实输出，回填后删除本提示。

退出码核验（Linux 预检实测）：`--status` = 0，`--help` = 0，未知参数 `--bogus` = 2。

## 四、测试清单（库内单元测试，共 5 项）

| # | 测试名 | 断言点 |
|---|---|---|
| 1 | `归属声明与决策一致` | 归属 crate = `aether-compositor`；定位含"保留命名空间""暂不拆分"；里程碑 = `M10 平台化阶段`；ADR 路径含 `aether-shell归属`、日期 = `2026-11-13`；三类触发条件（第三方 Shell / 多会话 / Wayland 会话）全部在册 |
| 2 | `status文本包含全部必需字段` | `--status` 文本含归属、定位、保留命名空间、M10、三类触发条件、ADR 路径等 12 个必需短语 |
| 3 | `已知参数解析为状态或帮助` | `--status` / `--describe` → Status；`--help` / `-h` / 无参数 → Help |
| 4 | `未知参数返回非零退出码` | 未知参数 → Unknown，退出码 2；Status / Help 退出码 0 |
| 5 | `未知参数混在合法参数中仍判未知` | `--status --bogus` 混合仍判 Unknown，退出码非 0 |

## 五、验证命令与结果

### 本机（Windows GNU 工具链，要求实跑）

```powershell
& "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu test -p aether-shell --locked
& "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu run -p aether-shell --locked -- --status
& "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu clippy -p aether-shell --all-targets --locked
```

结果：**待本机恢复后回填**（本机 QYY 离线，本轮未能在 Windows 上执行）。

### Linux 预检（云主机，离线）

- `cargo test -p aether-shell --offline`：**5 passed; 0 failed**（lib 5 项；bin / doc-test 0 项）。
- `cargo clippy -p aether-shell --all-targets --offline`：**Finished，0 warning**。
- 二进制实跑：`--status` 退出 0；`--help` 退出 0；`--bogus` 退出 2。

## 六、不拆分的后果（沿用 ADR，本轮重申）

- `aether-compositor` 继续承担 Shell 职责；`draw.rs` 的拆分（theme / settings / control_layout）优先于 Shell 拆分。
- `aether-shell` 不再作为"功能缺口"排期；它是保留占位，本轮落地的是边界声明与自检入口，不是 Shell 功能。
- 本 crate 不提供跨进程 Shell 协议、不维护独立窗口表 / 输入状态 / AI IPC 连接；这些仍在 compositor 内。

## 七、再评估触发条件

出现以下任一情况时，**先写新 ADR**（定义协议、生命周期、权限边界、回滚方案），再决定拆分或删除；评估里程碑为 **M10 平台化阶段**：

1. 出现第三方 Shell 需求；
2. 出现多会话 / 多用户会话需求；
3. 出现独立 Wayland 会话需求。

## 八、未完成 / 待办

- Windows GNU 工具链下的 `test` / `run --status` / `clippy` 三条命令尚未实跑，真实输出待回填本报告第三节与第五节。
- `aether-shell/Cargo.toml` 中既有 `aether-ipc`、`anyhow` 依赖声明本轮保留未删：新代码仅用 `std`，未实际调用二者；为不触碰 `Cargo.lock`（`--locked` 门禁）而暂留。后续锁文件刷新时可评估移除，需同步更新 `Cargo.lock`。
