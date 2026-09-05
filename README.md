# AetherOS

新一代 AI-First 操作系统：Linux 内核 + 全自研用户态 + AI 中枢。

> 以太（Aether）——古人设想中充满宇宙的介质。我们设想中连接人与机器的介质。

## 项目定位

不是发行版换皮，而是一个真正意义上"除了内核全是自己的"操作系统：

| 层 | 来源 |
|---|---|
| 内核 | Linux |
| 底层积木（libc、Mesa、驱动栈） | 成熟开源件 |
| init / 服务管理 | **自研** aether-init |
| 窗口合成器（Wayland） | **自研** aether-compositor |
| Shell / 界面 | **自研** aether-shell |
| AI 中枢 | **自研** aetherd（本地 Ollama + 云端 API 混合） |
| AI 运维自修复 | **自研** aether-ops |

## 组件

| 目录 | 说明 | 里程碑 |
|---|---|---|
| `aether-ipc/` | 全系统 IPC 协议（通信脊柱） | M0 |
| `aether-compositor/` | 自研 Wayland 合成器 | M1–M2 |
| `aether-shell/` | 顶栏、启动器、AI 对话面板 | M2 |
| `aether-init/` | 自研 PID 1 与服务管理 | M3 |
| `aetherd/` | AI 中枢守护进程 | M4 |
| `aether-ops/` | AI 运维与自修复 | M5 |
| `platform/` | rootfs 组装、ISO 打包 | M3 |
| `docs/` | 架构决策、协议、权限模型 | 持续 |

## 快速开始（当前阶段）

在任意桌面系统上预览 Aether 桌面视觉设计（M1 开发模式）：

```bash
cargo run -p aether-compositor
```

## 路线图

见 [docs/roadmap.md](docs/roadmap.md)。
