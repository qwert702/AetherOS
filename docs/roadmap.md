# AetherOS 路线图

> 状态：M0–M6 **全部完成**（2026-09-21 复核，依据 git 历史 + 代码实测 + docs/HANDOVER.md）。
> UI 设计系统 Step 1–4 + 视觉质量冲刺（§10）+ 双模主题（§12）完成，Step 5 仅剩实机重拍，见 docs/ui-design-handover.md。
>
> **生产力化清单见 `docs/PRODUCTION-PLAN-2026-09-27.md`** —— 五个阶段、每项带验收标准，
> 以及「生产力」的六个硬门禁（当前 0/6）。

## M0 — 开发环境与架构设计 ✅
- [x] 仓库骨架、Cargo workspace
- [x] 架构文档、IPC 协议草案、AI 权限模型
- [x] aether-ipc 协议 crate（含单元测试）
- [x] aether-compositor 渲染预览模式（Windows 可运行）
- [x] VirtualBox / VMware / QEMU 虚拟机验证环境（scripts/，Ubuntu 24.04 构建机）
- [x] Linux 侧构建：musl 静态编译入 ISO（Buildroot 外部树）

## M1 — 第一个自研窗口：合成器 MVP ✅（经 fbdev 路径达成）
- [x] draw.rs 视觉迭代：中文字体渲染（fontdue）、抗锯齿圆角/描边、柔和投影
- [x] 窗口内容渲染（终端/文件管理示意）、顶栏（品牌/工作区/时钟/AI 状态）、AI 面板
- [x] 主机预览交互：拖拽窗口、点击置顶（`cargo run -p aether-compositor`）
- [x] `--shot` 单帧截图自检管线（BMP→PNG）
- [x] 窗口布局引擎：两列/三列/独占/自由四种模式 + 缓动动画（layout.rs，含单元测试）
- [x] 拖拽边缘吸附（左半屏/右半屏/最大化）+ 实时吸附预览
- [x] 布局切换 Toast 反馈 + AI 指令入口
- [x] 多布局截图自检通过（--shot 1-4）
- [ ] smithay/Wayland 后端未接入：桌面经 **DRM→fbdev→软件光栅化** 上屏（M3/M4 在 QEMU/VBox/VMware 实测闭环）。ADR-003 的"直写 DRM 后路"未走，未来需要真实 Wayland 客户端支持时再接入

## M2 — 自研 Shell 雏形 ✅（职责由 compositor 承担）
- [x] 顶栏（品牌/菜单/AI 状态/搜索/时钟）、Dock（运行指示 + 打开应用）、AI 指令条
- [x] 拖拽置顶、边缘吸附、布局切换 Toast、AI 回复气泡
- [x] L2+ 权限确认弹窗（模态卡片）与安装向导共用同一条确认通路
- [x] 视觉体系 token 化（draw.rs::theme："Essence" 设计令牌，Step 1–4 完成）
- [ ] aether-shell crate 仍是占位骨架（7 行）：后续可把 compositor 中的 Shell 职责拆出，或明确其归属

## M3 — 系统地基 ★（核心里程碑达成！）
- [x] aether-init：白名单服务模型（编译期字面量命令，杜绝注入）+ 状态机 + 拓扑排序 + 环依赖检测 + 监督退避重启（含测试）
- [x] aether-init：服务控制 IPC（Linux Unix socket 0600 / 开发态 TCP 7312：start/stop/restart/status/sys_info）
- [x] 服务定义：network → aetherd → compositor/getty/ops，dry-run 冒烟通过
- [x] Buildroot 外部树 + defconfig（initramfs + ISO9660 引导）+ rootfs overlay（/init + 服务定义）+ build-iso.sh
- [x] VM 全自动化：VirtualBox 静默安装、Ubuntu 无人值守、SSH/免密 sudo/国内镜像
- [x] **首次构建出 AetherOS ISO（约 30MB）并在测试 VM 启动成功：内核 → /init → aether-init(PID 1) → 拓扑启动服务 → aetherd IPC 上线** ✅
- [x] 完整开机验证：network 服务 udhcpc 获取 IP + DNS；监督退避重启在真实环境运转 ✅
- [x] 持久化：安装器建 ext4 分区（卷标 AETHER）→ aether-init 启动挂载到 /var（日志/诊断/审计跨重启保留，两轮重启实测）
- [x] 日志链路：aether-init logtee 把服务 stdout/stderr 落盘 /var/log/aether/<unit>.log（8MB 轮转）+ 控制台 tee
- [ ] 图形栈走 fbdev（mesa/DRM + smithay 合成器后端未接入，见 M1 备注）
- 验收：ISO 开机，从内核到桌面整条链路上没有任何现成桌面/发行版组件 ✅
- 验收：ISO 开机，从内核到桌面整条链路上没有任何现成桌面/发行版组件 ✅

## M4 — AI 中枢 aetherd ✅（含 L2+ 确认全链路）
- [x] 权限模型落地：L0–L3 闸门 + 审计日志（perm.rs，带测试）
- [x] 混合推理路由：隐私强制本地 / 复杂任务上云 / 双侧降级（router.rs，带测试）
- [x] 工具系统：罐头探针 + read_file + sys_info + desktop + install_disk，闸门→审计→执行管线（tools.rs，带测试）
- [x] LLM 客户端：OpenAI 兼容协议（GLM / Ollama /v1 通用）
- [x] chat CLI：单轮 agent（路由 → LLM → 工具循环 → 回答），探活失败优雅降级
- [x] `serve` 常驻模式：TCP 127.0.0.1:7311 + aether-ipc NDJSON 协议（端到端烟雾测试通过）
- [x] 离线快速意图通道：布局/时间/状态等系统指令免 LLM 直接执行（intent.rs）——"快慢双思"架构
- [x] 合成器 AI 指令条真实输入：Enter 发送 → ChatChunk 回复气泡 + Action 执行布局切换
- [x] IPC Action 通道：`layout_set` 等 aetherd → Shell 桌面行为指令
- [x] LLM 深度接入桌面：`desktop` 工具（layout_set/open_app/close_active，白名单校验）+ ToolCtx 行为队列 → IPC Action → 合成器执行（端到端验证通过）
- [x] 离线快速意图扩展：整理桌面/铺满 等模糊指令的确定性解释
- [x] **L2+ 确认卡片 UI 全链路** ✅：一次性确认令牌（服务端签发、绑定 tool+参数、5 分钟、用后即废）→ `Response::NeedsConfirmation` → 模态确认弹窗（L2 黄/L3 红徽章、参数明文、L3 回显输入）→ 带令牌重发；协议层 13 项断言 + UI 像素断言全过（2026-09-21 复核）
- [x] 推理通道回传：`ChatChunk.channel`（"local"/"cloud"）→ 顶栏 AI 三态（本地青/云端紫/离线灰）实装并像素验证（2026-09-21）
- [ ] 中文输入支持（预览期受 minifb 限制仅英文，M2 字体子系统解决——待实机键盘验证）
- [ ] 接入真实 LLM 验证模糊指令全链路（需 Ollama 或 GLM API Key；路由/客户端已就绪）
- 验收：自然语言操作整台"电脑" ✅（离线意图 + Action 端到端实测）

## M5 — AI 运维自修复 ✅
- [x] 常驻巡检 agent（aether-ops）：15s 一轮，/proc 真实指标 + aether-init 服务状态
- [x] 自修复：异常服务经 ServiceControl 重启（20 轮冷却防风暴；重启策略以服务定义 restart 标记为唯一事实来源）
- [x] 日志监听预警：aether-init logtee 落盘 + ops 增量扫描异常行即时告警
- [x] 故障诊断报告：采集 → 上下文 → 落盘 /var/diag → 串口（diagnose.rs，纯函数带测试）
- [x] 关键服务（essential）异常升级告警
- 实测：killall compositor → 一轮内发现并自动拉起；AI 条注入乱串 → 日志告警 ✅
- 验收：注入故障被 AI 发现并修复 ✅

## M6 — 0.1 发布 ✅（v0.3）
- [x] aether-install 磁盘安装器：isohybrid dd 整盘安装（防呆：块设备/容量/显式 --yes/禁自读自写）
- [x] 持久化分区：MBR 第 2 分区 ext4（卷标 AETHER）+ 引导记录；失败不阻断引导（回退内存态）
- [x] 桌面安装向导：Live ISO Dock 图标 → 选盘 → 一键装机（与 AI 共用 L3 确认通路）
- [x] aetherd `install_disk` 工具：路径白名单 + confirm 回显双确认（L3）
- [ ] 品牌收尾：Logo / 开机动画 / 壁纸（docs/image-gen-prompts.md 已有提示词，未落地）
- [ ] 实机截图更新：docs/screenshot-*.png 需在真实 VM 重拍（见 ui-design-handover §4.5）
- 验收：ISO 安装到虚拟硬盘，脱离 ISO 独立运行 ✅（无光驱纯磁盘引导 + 两轮重启实测）

## UI 设计系统（2026-09 追加，详见 docs/ui-design-handover.md）
- [x] Step 1 设计令牌收口（theme 六子模块，消灭重复定义与魔数）
- [x] Step 2 字体渲染质量（取整/基线/覆盖增益 + --fonttest）
- [x] Step 3 核心组件重绘（五层层级模型、红绿灯悬停符号、Dock 中性玻璃化、vcenter）
- [x] Step 4 L2+ 权限确认（协议 + 弹窗 UI + 三类气泡 + AI 三态 + 安装向导同通路）
- [x] 视觉质量冲刺（handover §10）：结构化极光壁纸 + 颗粒、明度阶梯纵深、窗口标题栏分层、
      内容形态（文件网格/终端分色/音乐列表面板）、图标光照方向统一（9 轮 `--shot` 目视迭代）
- [x] 界面走查：`cargo check` 零警告、`cargo test` 65 项全绿、主机走查图归档至 `docs/host-ui-*.png`
- [x] 双模主题（handover §12）：色板常量改模式函数（172 调用点）、明亮为默认（苹果风：近白底 + 四角粉彩、
      底座/窗口/纸面三级明度阶梯、悬浮层白玻璃、阴影轻度明确）、`--theme light|dark`
      —— 深色模式像素级零回归，明亮全状态走查通过
- [ ] Step 5 剩余：VM 实机复看并重拍 `docs/screenshot-*.png`（P2，见 handover §4.5）

## 工程化与性能（2026-09-26 ~ 09-27）
- [x] 安全链路修复（handover §13）：确认令牌绑定确认方身份、`read_file` 路径白名单、
      路由不再用 `history_len` 判复杂、拒绝通路打通（审计里首次出现 `denied_by_user`）
- [x] 合成器性能优化（handover §14）：稳态 91 → 15.5 ms/帧（-83%）、自适应帧率、脏行上屏、字形缓存；
      新增 `--bench` 与 `AETHER_RENDER_TIMING` 两个观测手段
- [x] 文件管理器从视觉演示改为真实可用：读真实目录、单击选中/再点打开、文本预览、
      `← 上级目录`、读失败可见（未实现项清单见 `docs/UNIMPLEMENTED-2026-09-27.md`）
- [x] 壁纸分帧生成（handover §15）：交互路径首帧 561 → 30 ms，不再有开机卡顿
- [x] 归档走查图工具化：`scripts/archive-ui-shots.py`（重建 + `--check` 门禁），
      堵住"归档图落后于 HEAD 导致回归结论失真"这个已犯两次的流程坑
- [x] 生产力化 Phase 0 首批（见 `docs/PRODUCTION-PLAN-2026-09-27.md`）：窗口关闭/最大化（红绿灯接线）、
      修饰键状态机（Ctrl/Shift/Alt）、完整键位映射（方向键/翻页/Home/End/Tab/Delete）、
      文件列表滚动与键盘导航（含如实状态栏与滚动条）、init 挂载 devpts（PTY 前置）
      —— 测试 91 → 112 项，双目标零警告
- [x] 生产力化 Phase 1 主体：**真实终端**（PTY + VT/ANSI 解析器 + 输入路由 + 剪贴板），
      文件管理器侧栏接真实路径 + 可点击面包屑，窗口缩放，init 救援模式，日志运行期轮转
      —— 测试 112 → 172 项；六个"生产力"硬门禁 2.5/6（详见 PRODUCTION-PLAN 进度表）