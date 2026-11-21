# ADR：aether-shell 的归属与拆分时机

- 状态：**接受**
- 日期：2026-11-13
- 相关：`aether-compositor`、`aether-shell`、M2 / M10

## 背景

`aether-shell` 目前只有 11 行占位骨架；顶栏、Dock、工作区、AI 指令条等 Shell 职责实际都在
`aether-compositor` 内。这个状态已经持续很久，文档里同时存在"Shell 由 compositor 承担"和
"aether-shell 是占位"两种说法，容易让接手者误判归属。

## 决策

**暂不拆分，保留 `aether-shell` 作为保留命名空间；把"Shell 归属"记录为有意的架构决策，
而不是未完成缺口。M10 平台化阶段再评估是否拆出。**

## 理由

1. 当前 Shell 与合成器共享同一套渲染缓冲、输入状态、窗口表和 AI IPC 连接；拆成独立进程需要
   先定义一套跨进程 Shell 协议，否则只是把耦合换了个进程边界。
2. 目前没有第三方 Shell / 多会话需求；拆分不会改善用户可见能力，反而增加启动、同步和故障面。
3. 项目当前优先级是 M8/M9 的安装、升级、真机与图形验证；大改 Shell 边界会与这些验证抢时间。
4. 保留 crate 的成本仅为 11 行和一次编译；删除后再恢复要重新处理 workspace、文档和发布脚本。

## 后果

- `aether-compositor` 继续承担 Shell 职责，`draw.rs` 的拆分（theme/settings/control_layout）优先于
  Shell 拆分；
- 不再把 `aether-shell` 当作"功能缺口"排期；它是**保留占位**；
- 若将来出现第三方 Shell、Wayland 会话或多用户会话需求，必须先写新 ADR，定义协议、生命周期、
  权限边界和回滚方案，再决定拆分或删除。

## 验收

- 本文进入 `docs/README.md` 的入口与架构索引；
- ROADMAP M2 的 `/aether-shell` 条目改为"决策已记录"；
- 代码不变，`cargo test --workspace --locked` 与现有门禁不受影响。

## 实施状态（2026-11-21）

本轮（E2）把 11 行占位骨架改成"边界明确、可运行、可测试的最小实现"，**决策不变：仍不拆分**。

- 代码：`aether-shell` 新增库层 `src/lib.rs`，以 `ShellOwnership` 结构 + `ownership()` 常量集中承载归属事实（归属 crate、保留命名空间定位、M10 再评估、三类触发条件、ADR 路径与日期）；`src/main.rs` 提供 `--status` / `--describe` 子命令打印这些事实，未知参数以退出码 2 拒绝。
- 测试：库内单元测试断言归属声明与本 ADR 一致（关键短语）、`--status` 输出包含全部必需字段、未知参数返回非 0 退出码。
- 依赖：代码仅用 `std`，未新增 crate 依赖；Windows / Linux 均可编译运行。
- 验证：`cargo test -p aether-shell --locked`、`cargo run -p aether-shell -- --status`、`cargo clippy -p aether-shell --all-targets --locked`（结果见 `报告/aether-shell 边界实现（SHELL-BOUNDARY）-2026-11-21.md`）。
- 仍不做：本 crate 不实现顶栏 / Dock / 工作区 / AI 指令条等 Shell 运行时职责；这些继续由 `aether-compositor` 承担。拆分评估仍挂在 M10，触发条件见上文「后果」。
