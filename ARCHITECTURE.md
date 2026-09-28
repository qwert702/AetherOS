# AetherOS 架构

状态：M0 定稿。本文档记录系统分层、组件职责与关键设计决策。

> ⚠️ **这是 M0 阶段（2026-09-05）的冻结文档，保留为设计意图记录，不要当作现状读**。
> 两处已知与现状不符：
> - 分层图里 compositor 写的是 `Wayland 合成器(smithay)`，实际走的是 **DRM→fbdev→软件光栅化**，
>   smithay 未接入（见 ADR-003，以及 `docs/roadmap.md` M1 备注）。2026-09-28 起有一个**自研 `wl_display` 子集**
>   的 spike（`aether-compositor/src/wayland/`），但**未接生产路径**，见 `docs/PHASE3-DECISION-2026-09-28.md`。
> - 分层图把 `aether-shell` 画在最上层，实际它是**占位骨架（11 行）**，Shell 职责当前由 compositor 承担。
>
> **看现状请看**：`INDEX.md`（组件与入口）、`docs/roadmap.md`（里程碑）、
> `docs/PRODUCTION-PLAN-2026-09-28.md`（生产力化与门禁）。

## 1. 分层

```
┌─────────────────────────────────────────────┐
│  aether-shell        顶栏/启动器/AI 面板      │  ← 自研，Rust
├─────────────────────────────────────────────┤
│  aether-compositor   Wayland 合成器(smithay) │  ← 自研，Rust
├─────────────────────────────────────────────┤
│  aetherd / aether-ops   AI 中枢 / AI 运维     │  ← 自研，Rust
├─────────────────────────────────────────────┤
│  aether-ipc          统一消息协议(JSON/NDJSON)│  ← 自研，Rust
├─────────────────────────────────────────────┤
│  aether-init         PID 1 / 服务管理         │  ← 自研，Rust (musl 静态)
├─────────────────────────────────────────────┤
│  glibc · Mesa · NetworkManager · 固件        │  ← 成熟开源件（底层积木）
├─────────────────────────────────────────────┤
│  Linux kernel                                │  ← 内核
└─────────────────────────────────────────────┘
```

## 2. 设计决策记录（ADR）

### ADR-001：内核用 Linux，不写自己的内核
所有新一代 OS（Android、ChromeOS、webOS）都用 Linux 内核。内核是几十年
数万开发者积累的硬件支持层，重写它不产生任何差异化价值。我们的差异化在
AI 中枢与交互层。

### ADR-002：行为层 100% 自研
init、窗口系统、Shell、AI 中枢全部自研。系统行为完全可控、可解释、
可被 AI 深度操作——这是"AI-First"的前提：AI 能操作的前提是每个系统
行为都有清晰、机器可调用的接口（aether-ipc）。

### ADR-003：合成器基于 smithay，不碰 KMS 原始接口
smithay 是 Rust 生态成熟的 Wayland 合成器框架（System76 COSMIC 同款）。
它处理 KMS/DRM、输入、GPU 加速等脏活，我们专注桌面逻辑与视觉。
未来如有需要可绕过 smithay 直写 DRM 后端（留有后路）。

### ADR-004：AI 混合推理路由
- 本地（Ollama/llama.cpp）：小模型，隐私任务、离线场景、高频低难度请求
- 云端（GLM 等 API）：复杂推理、长上下文、多步规划
- 路由器按任务特征（长度、是否含隐私上下文、是否离线）自动选择，用户可覆盖

### ADR-005：IPC 用 newline-delimited JSON 起步
M0–M4 用 NDJSON over Unix socket，调试友好、迭代快。协议消息类型定义在
aether-ipc，未来可无损切换到二进制编码（protobuf/postcard），消息语义不变。

### ADR-006：视觉语言"以太"
深空底色 + 流动极光渐变（青/紫/品红）+ 玻璃质感面板。色板定义在
aether-compositor/src/draw.rs 的 `palette` 模块，全系统共享。

## 3. 启动链路（M3 后）

```
BIOS/UEFI → GRUB → Linux kernel → initramfs
  → aether-init (PID 1)
    → 伪文件系统挂载、驱动加载(udev)、网络、日志
    → aetherd（AI 中枢）
    → aether-compositor（占屏）
    → aether-shell（顶栏 + AI 面板）
```

## 4. 进程间关系

- 所有系统行为（服务控制、设置、窗口管理、文件操作）通过 aether-ipc 暴露
- aetherd 是唯一被允许"理解自然语言"的组件；它把语言翻译成工具调用
- aether-ops 订阅日志流，异常时通过 aetherd 请求用户授权修复
