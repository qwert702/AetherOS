# AetherOS 文档导航

> 整理日期：2026-10-31 · 适用代码：`main` @ `da26059`
>
> 这里是全仓文档的统一入口。根目录 [`README.md`](../README.md) 是项目门面；本页负责回答
> “现在该读哪一篇、哪些是历史快照、哪些是当前事实”。

## 当前事实（先看这一张表）

| 项目 | 当前值 | 权威来源 |
|---|---|---|
| 代码版本 | `main` @ `da26059` | `git rev-parse HEAD` |
| Rust 规模 | **52,544 行 / 77 个源文件 / 7 个 crate** | `python scripts/repo-stats.py` |
| 单元 + 集成测试 | **793 项全绿** | `cargo test --workspace --offline --locked` |
| 完整门禁 | **42/42 全过** | `python scripts/gates.py` |
| CI | run #86–#91 连续全绿（最新 #91） | [CI 诊断](证据/CI诊断-2026-10-24.md) |
| ISO | v0.1.1 · **41.0 MB** · SHA256 `eea68da9…` | `site/release.json`、[下载页](https://aether.cbnac.com/download.html) |
| 本地站点 | `gen-site.py --check` 通过，页面数据与仓库一致 | `python scripts/gen-site.py --check` |
| 线上站点 | 可访问、TLS 与安全头正常；**内容仍是旧版数字** | [站点巡检](报告/站点巡检（SITE-CHECK）-2026-10-31.md) |

> 线上站点的“旧版数字”是当前唯一的公开信息不一致项：首页仍写 51,138 行 / 71 文件 / 768 测试，
> 仓库已是 52,544 / 77 / 793。原因与处理见站点巡检报告。

## 先读这五篇

1. [修复审计（FIX-AUDIT）-2026-10-31](报告/修复审计（FIX-AUDIT）-2026-10-31.md) —— 本轮 W0–W7 做了什么、怎么验证、还剩什么。
2. [交接摘要（HANDOVER-SUMMARY）-2026-10-17](报告/交接摘要（HANDOVER-SUMMARY）-2026-10-17.md) —— 5 分钟看懂系统边界与“卡在人手上”的事。
3. [路线图与已知缺口（ROADMAP）-2026-09-05](路线图与已知缺口（ROADMAP）-2026-09-05.md) —— 版本里程碑与长期方向。
4. [功能缺口清单（FEATURE-GAPS）-2026-10-02](报告/功能缺口清单（FEATURE-GAPS）-2026-10-02.md) —— 按用户预期落差排的 P0/P1/P2 清单。
5. [代码索引（INDEX）-2026-09-11](代码索引（INDEX）-2026-09-11.md) —— 每个 crate / 源文件的职责与行数。

## 文档地图

### 入口与架构

| 文档 | 用途 |
|---|---|
| [根 README](../README.md) | 项目介绍、快速开始、能力与边界 |
| [架构与设计决策（ARCHITECTURE）](架构与设计决策（ARCHITECTURE）-2026-09-05.md) | 内核之上为什么这样分层 |
| [代码索引（INDEX）](代码索引（INDEX）-2026-09-11.md) | 逐 crate / 逐文件索引、已知缺口 |
| [交接报告（HANDOVER）](交接报告（HANDOVER）-2026-09-11.md) | 早期完整交接记录 |
| [视觉设计交接（UI-DESIGN-HANDOVER）](视觉设计交接（UI-DESIGN-HANDOVER）-2026-09-20.md) | 设计系统、字体、布局与走查图说明 |

### 协议与规范

| 文档 | 用途 |
|---|---|
| [AI 权限模型（AI-PERMISSIONS）](AI权限模型（AI-PERMISSIONS）-2026-09-05.md) | L0–L3、确认令牌、审计裁决 |
| [IPC 协议（IPC-PROTOCOL）](进程间通信协议（IPC-PROTOCOL）-2026-09-05.md) | NDJSON over Unix socket / 可选 TCP |
| [应用包格式（APP-PACKAGES）](应用包格式（APP-PACKAGES）-2026-09-28.md) | 自研应用包与安装器约定 |

### 现状、验收与安全

| 文档 | 用途 |
|---|---|
| [修复审计（FIX-AUDIT）](报告/修复审计（FIX-AUDIT）-2026-10-31.md) | 当前修复批次、三遍审计、残余风险 |
| [CI 诊断与修复记录](证据/CI诊断-2026-10-24.md) | 从 74 次 0 成功到 run #86/#87/#89 全绿的证据链 |
| [收尾报告（FINAL）](报告/收尾报告（FINAL）-2026-10-04.md) | 2026-10-04 的完成度总表（历史快照） |
| [交接摘要（HANDOVER-SUMMARY）](报告/交接摘要（HANDOVER-SUMMARY）-2026-10-17.md) | 最新交接摘要与待验清单 |
| [站点巡检（SITE-CHECK）](报告/站点巡检（SITE-CHECK）-2026-10-31.md) | 本地/线上站点一致性、可用性与安全头检查 |
| [安全审计（SECURITY-AUDIT）](报告/安全审计（SECURITY-AUDIT）-2026-10-02.md) | 全仓安全审计与修复台账（大文件） |
| [依赖 CVE 核对（CVE-AUDIT）](证据/依赖CVE（CVE-AUDIT）-2026-10-14.md) | 依赖漏洞与版本核对 |
| [门禁数快照](证据/gate-count.txt) | `gates.py` 写入的本次运行门禁数 |

### 路线图与生产化

| 文档 | 用途 |
|---|---|
| [路线图与已知缺口（ROADMAP）](路线图与已知缺口（ROADMAP）-2026-09-05.md) | M0–M6 里程碑与后续方向 |
| [后续发展计划（NEXT-PLAN）](报告/后续发展计划（NEXT-PLAN）-2026-10-31.md) | M7–M10 工作项、验收标准与执行顺序（当前计划） |
| [功能缺口清单（FEATURE-GAPS）](报告/功能缺口清单（FEATURE-GAPS）-2026-10-02.md) | P0/P1/P2 缺口、里程碑建议 |
| [生产化计划（PRODUCTION-PLAN）](报告/生产化计划（PRODUCTION-PLAN）-2026-09-28.md) | 生产化实施计划（当前版本） |
| [第三阶段决策（PHASE3-DECISION）](报告/第三阶段决策（PHASE3-DECISION）-2026-09-28.md) | 阶段 3 范围与取舍 |

### 构建、安装与运维

| 文档 | 用途 |
|---|---|
| [镜像重建手册（ISO-REBUILD）](报告/镜像重建手册（ISO-REBUILD）-2026-10-11.md) | Buildroot 构建机、ISO 重建、常见坑 |
| [实机验收手册（RUNBOOK）](报告/实机验收手册（RUNBOOK）-2026-10-11.md) | 物理机 45 项验收清单 |
| [装机与更新操作手册（OPS-MANUAL）](报告/装机与更新操作手册（OPS-MANUAL）-2026-10-17.md) | 装机、更新、回滚步骤 |
| [VMware 装机与自动更新（VMWARE-OTA）](报告/VMware装机与自动更新（VMWARE-OTA）-2026-10-15.md) | VMware 端到端方案与实施细节 |

### 发布

| 文档 | 用途 |
|---|---|
| [发布清单（LAUNCH）](发布/发布清单（LAUNCH）-2026-09-29.md) | 发布渠道、排期与纪律 |
| [发布文案（LAUNCH-COPY）](发布/发布文案（LAUNCH-COPY）-2026-10-01.md) | 可直接复制的渠道文案 |
| [发布部署清单（DEPLOY-ACCESS）](报告/发布部署清单（DEPLOY-ACCESS）-2026-10-31.md) | 部署需要什么账号、密钥与最小权限 |

### 证据

| 文档 | 证明什么 |
|---|---|
| [自动更新链路（P1）](证据/自动更新链路（P1）-2026-10-17.md) | updater / 打包 / 发布的真实边界 |
| [T3 验证：keep-var 保住 /var](证据/T3验证-keepvar保住var-2026-10-17.md) | 换系统后持久分区存活 |
| [VMware 无人装机（AUTOINSTALL）](证据/VMware无人装机（AUTOINSTALL）-2026-10-16.md) | 引导期自动安装 |
| [装机验证（INSTALL-VERIFY）](证据/装机验证（INSTALL-VERIFY）-2026-10-14.md) | 安装器实测 |
| [鼠标绝对坐标（EV_ABS）](证据/鼠标绝对坐标（EV_ABS）-2026-10-17.md) | 输入设备修复 |
| [等宽字体（MONO-FONT）](证据/等宽字体（MONO-FONT）-2026-10-15.md) | 字体收口 |
| [i18n 终扫](报告/i18n终扫（I18N-FINAL-SCAN）-2026-10-04.txt) | 全库 i18n 扫描结果 |
| [Toast 双语审计](证据/toast-i18n-审计-2026-10-11.md) | `push_toast` 结构化覆盖 |

### 归档（历史快照，不再维护）

以下文档保留当时的判断与数字，**不要拿它们和当前 HEAD 对比后直接改数字**：

- [代码审查总集（CODE-REVIEW）](归档/代码审查总集（CODE-REVIEW）-2026-09-29.md)
- [生产化计划（旧版）](归档/生产化计划（PRODUCTION-PLAN）-2026-09-27.md)
- [未实现项清单（UNIMPLEMENTED）](归档/未实现项清单（UNIMPLEMENTED）-2026-09-27.md)
- [性能报告（PERF-REPORT）](归档/性能报告（PERF-REPORT）-2026-09-26.md)
- [界面设计方案（UI-DESIGN-PLAN）](归档/界面设计方案（UI-DESIGN-PLAN）-2026-09-19.md)
- [配图生成提示词（IMAGE-GEN-PROMPTS）](归档/配图生成提示词（IMAGE-GEN-PROMPTS）-2026-09-24.md)

## 文档维护规则

1. **当前事实只有一个来源**：代码规模看 `scripts/repo-stats.py`，发布时间/ISO 看
   `site/release.json`，测试数看 `cargo test --list`，门禁数看 `docs/证据/gate-count.txt`。
2. **日期文档是快照**：`报告/` 与 `证据/` 里的历史数字不追着改；新结论写到新日期的文档，
   或在当前入口文档里加“更新”说明。
3. **提交前跑检查**：
   ```bash
   python scripts/doc-numbers-check.py
   python scripts/check-links.py
   python scripts/gates.py --fast
   ```
4. **新增文件放对目录**：可复跑证据放 `证据/`，阶段性报告放 `报告/`，过时但保留价值的放 `归档/`。
5. **站点数字不要手抄**：`site/` 由 `python scripts/gen-site.py --refresh` 生成，提交前跑
   `python scripts/gen-site.py --check`；线上发布用 `scripts/deploy-site.py`，不要直接改服务器。
