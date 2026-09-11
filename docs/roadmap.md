# AetherOS 路线图

## M0 — 开发环境与架构设计 ✅（当前）
- [x] 仓库骨架、Cargo workspace
- [x] 架构文档、IPC 协议草案、AI 权限模型
- [x] aether-ipc 协议 crate（含单元测试）
- [x] aether-compositor 渲染预览模式（Windows 可运行）
- [ ] WSL2 + QEMU 环境（需用户手动安装，见下）
- [ ] Linux 侧冒烟：WSL2 里 cargo build 全 workspace

> Linux 环境用 Ubuntu 虚拟机（用户选择 VirtualBox/VMware，不用 WSL2）：
> 1. 安装 VirtualBox → 创建 Ubuntu 24.04 VM（4GB+ 内存）
> 2. 项目文件夹设为共享文件夹挂进 VM
> 3. VM 内：`sudo apt install -y build-essential qemu-system-x86 ollama pkg-config libwayland-dev`
> 4. 系统镜像构建（M3）与 Wayland 后端测试（M1）都在 VM 内完成

## M1 — 第一个自研窗口：合成器 MVP（进行中）
- [x] draw.rs 视觉迭代：中文字体渲染（fontdue）、抗锯齿圆角/描边、柔和投影
- [x] 窗口内容渲染（终端/文件管理示意）、顶栏（品牌/工作区/时钟/AI 状态）、AI 面板
- [x] 主机预览交互：拖拽窗口、点击置顶（`cargo run -p aether-compositor`）
- [x] `--shot` 单帧截图自检管线（BMP→PNG）
- [x] 窗口布局引擎：两列/三列/独占/自由四种模式 + 缓动动画（layout.rs，含单元测试）
- [x] 拖拽边缘吸附（左半屏/右半屏/最大化）+ 实时吸附预览
- [x] 布局切换 Toast 反馈 + AI 指令入口（A 键模拟"把窗口排成两列"，将来由 aetherd 经 IPC 触发同一逻辑）
- [x] 多布局截图自检通过（--shot 1-4）
- [ ] Ubuntu VM（VirtualBox，替代 WSL2 方案）中跑通 smithay 后端，真 Wayland 客户端上屏
- 验收：虚拟机里出现自绘桌面 + 可移动窗口

## M2 — 自研 Shell 雏形
- [ ] 顶栏、启动器、工作区、通知
- [ ] UI 组件库沉淀
- 验收：键盘鼠标全流程操作

## M3 — 系统地基 ★（核心里程碑达成！）
- [x] aether-init：白名单服务模型（编译期字面量命令，杜绝注入）+ 状态机 + 拓扑排序 + 环依赖检测 + 监督退避重启（8 项测试）
- [x] aether-init：服务控制 IPC（端口 7312：start/stop/restart/status/sys_info）
- [x] 服务定义：network → aetherd → compositor/getty/ops，dry-run 冒烟通过
- [x] Buildroot 外部树 + defconfig（initramfs + isolinux 引导）+ rootfs overlay（/init + 服务定义）+ build-iso.sh
- [x] VM 全自动化：VirtualBox 7.2.16 静默安装（TUNA 镜像）、Ubuntu 24.04.3 无人值守、SSH/免密 sudo/国内镜像、VDI 迁移 D 盘
- [x] **首次构建出 AetherOS ISO（26MB）并在测试 VM 启动成功：内核 → /init → aether-init(PID 1) → 拓扑启动服务 → aetherd IPC 上线** ✅
- [x] 修复遗留启动报错：三组件（init/aetherd/ops）musl 编译入 ISO、udhcpc 修正为 /sbin/udhcpc、compositor 延后至 smithay 就绪、VBox 光驱缓存需 detach/attach 刷新
- [x] **完整开机验证：network 服务通过 udhcpc 获取 IP（10.0.2.15）+ DNS 配置成功——AetherOS 联网；监督退避重启机制在真实环境运转（ops/network 退出后按 2^n 退避自动拉起）** ✅
- [ ] 图形栈接入：mesa/DRM + smithay 合成器后端，开机直达自研桌面
- 验收：ISO 开机，从内核到桌面整条链路上没有任何现成桌面/发行版组件
- 验收：ISO 开机，从内核到桌面整条链路上没有任何现成桌面/发行版组件

## M4 — AI 中枢 aetherd ★（第一阶段完成）
- [x] 权限模型落地：L0–L3 闸门 + 审计日志（perm.rs，带测试）
- [x] 混合推理路由：隐私强制本地 / 复杂任务上云 / 双侧降级（router.rs，6 项测试）
- [x] 工具系统：罐头探针 + read_file + sys_info，闸门→审计→执行管线（tools.rs，4 项测试）
- [x] LLM 客户端：OpenAI 兼容协议（GLM / Ollama /v1 通用）
- [x] chat CLI：单轮 agent（路由 → LLM → 工具循环 → 回答），探活失败优雅降级
- [x] `serve` 常驻模式：TCP 127.0.0.1:7311 + aether-ipc NDJSON 协议（端到端烟雾测试通过）
- [x] 离线快速意图通道：布局/时间/状态等系统指令免 LLM 直接执行（intent.rs，5 项测试）——"快慢双思"架构
- [x] 合成器 AI 指令条真实输入：Enter 发送 → ChatChunk 回复气泡 + Action 执行布局切换
- [x] IPC Action 通道：`layout_set` 等 aetherd → Shell 桌面行为指令
- [x] LLM 深度接入桌面：`desktop` 工具（layout_set/open_app/close_active，白名单校验）+ ToolCtx 行为队列 → IPC Action → 合成器执行（端到端验证通过）
- [x] 离线快速意图扩展：整理桌面/铺满 等模糊指令的确定性解释
- [ ] L2+ 确认卡片 UI（NEEDS_CONFIRMATION → 确认弹窗 → 带 approval 重试）
- [ ] 中文输入支持（预览期受 minifb 限制仅英文，M2 字体子系统解决）
- [ ] 接入真实 LLM 验证模糊指令全链路（需 Ollama 或 GLM API Key）
- 验收：自然语言操作整台"电脑"

## M5 — AI 运维自修复
- [ ] 日志监听、异常预警、诊断与一键修复
- 验收：注入故障被 AI 发现并修复

## M6 — 0.1 发布
- [ ] 安装器、品牌（Logo/开机动画/壁纸）、README+截图
- 验收：ISO 安装到虚拟硬盘，脱离 ISO 独立运行
