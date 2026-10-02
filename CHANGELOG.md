# 版本发布说明

> 这里是**版本级**的发布说明（首个公开版本 v0.1.0）。
> **日常改动**记录在同目录的 [`更新日志/`](更新日志/) 里 —— 按日期一天一个文件。
>
> 官网 `aether.cbnac.com/changelog.html` 由 `scripts/gen-site.py` 把两者一起渲染：
> 先按日期**倒序**列出每日更新，再把本文档（最早的版本说明）放在最后。
> **两个来源各只有一份，不重复维护。**
>
> This file holds **release-level** notes only. Day-to-day changes live in
> [`更新日志/`](更新日志/) (one file per day). The generator renders both into the changelog page —
> daily entries newest-first, then this file (the earliest) last.

---

## v0.1.0 — 2026-09-29

**首个公开版本。** 可引导 ISO，QEMU / VirtualBox / VMware 三个平台实测开机；可安装到虚拟磁盘后独立引导。

*First public release. Bootable ISO verified on QEMU, VirtualBox and VMware; installable to a virtual disk and bootable from it.*

### 系统内核之上，全部自研 / A userspace written from scratch

- **PID 1**（`aether-init`）：服务编排、持久化分区、串口日志、故障自愈。
- **窗口合成器**（`aether-compositor`）：直接写 framebuffer 的立即模式合成器，
  自带窗口管理、平铺布局、阴影与圆角、字形缓存、脏行上屏与自适应帧率。
- **终端模拟器**：PTY + VT 解析器，自己实现，不是包一层现成的终端库。
- **中文输入法**：拼音输入 + 候选框。
- **AI 中枢**（`aetherd` + `aether-ipc`）：NDJSON over TCP 的线协议，
  **每一次工具调用都要过权限闸门**，L2+ 操作弹确认框；架构上客户端 → 用户自己的网关 → 各家模型，
  系统内**不存放上游 API Key**。

### 能装市面上的 Linux 软件 / Runs real Linux software

- 静态二进制与常见动态依赖都能装能跑；镜像补齐 ncurses + terminfo、zlib、openssl、libffi、expat。
- **实测**：从宿主拉下应用包 → `aetherd` 安装 → 在桌面里跑起 **htop 3.3.0**（Ubuntu 24.04 官方包）。
- 应用包格式与安装器（`aether-install`）自研。

### 工程与验证 / Engineering

- 规模、测试数、走查图都进**门禁**：`scripts/repo-stats.py --check`（文档口径）、
  `cargo test --workspace`、musl 静态目标 `--all-targets` 零警告、
  `scripts/archive-ui-shots.py --check`（界面截图逐像素比对）。
- 可引导 ISO **约 38.5 MB**。
- 许可证：**GPL-3.0-only**。
