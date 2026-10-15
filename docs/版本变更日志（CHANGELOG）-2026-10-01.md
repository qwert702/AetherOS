# 版本发布说明

> 这里是**版本级**的发布说明（首个公开版本 v0.1.0）。
> **日常改动**记录在同目录的 [`更新日志/`](../更新日志/) 里 —— 按日期一天一个文件。
>
> 官网 `aether.cbnac.com/changelog.html` 由 `scripts/gen-site.py` 把两者一起渲染：
> 先按日期**倒序**列出每日更新，再把本文档（最早的版本说明）放在最后。
> **两个来源各只有一份，不重复维护。**
>
> This file holds **release-level** notes only. Day-to-day changes live in
> [`更新日志/`](../更新日志/) (one file per day). The generator renders both into the changelog page —
> daily entries newest-first, then this file (the earliest) last.

---

## v0.1.1 — 2026-10-15

**v0.1.0 之后的第一个更新**（仍是 prerelease）。相对 v0.1.0 的变化：

- **ISO 真的 isohybrid 了**（offset 510 = `55aa`）：v0.1.0 的资产**从未过 isohybrid**，
  所以 `dd` 到 U 盘起不来、装不了机 —— 根因是同步工具按字符串前缀跳过了 `platform/build-iso.sh`，
  构建机两周一直在用旧脚本。已修，并有门禁盯着。
- **终端等宽字体补上**：此前镜像里没有等宽字体，终端只能逐字回退成非等宽。
- **通知正文全量双语**（118 个生产调用点，含外部来源文本的两语言消毒）。
- **依赖漏洞修复**：`rustls` 0.23.43 → 0.23.46（RUSTSEC-2026-0285）；`cargo audit` 复验 0 漏洞。
- **装机验证四步全过**：起 ISO → 安装器 → **拔掉光驱从盘引导** → 重启后 `/var` 写入仍在。
- 规模：51,138 行 Rust / 71 文件 · 768 项测试 · 34 项门禁全绿。

校验和与体积见下载页 —— 由发布流程**从资产本身**算出（这一次实测回下载一致）。

## v0.1.0 — 2026-09-29

**首个公开版本。** 可引导 ISO，QEMU / VirtualBox / VMware 三个平台实测开机；可安装到虚拟磁盘后独立引导。

*First public release. Bootable ISO verified on QEMU, VirtualBox and VMware. ~~installable to a virtual disk and bootable from it~~ — **corrected 2026-10-02: the published ISO is not isohybrid, so disk installation does not work; see the note below.***

**⚠️ 2026-10-02 更正**：上面"可安装到虚拟磁盘后独立引导"**对本版本发布的那个 ISO 不成立**。
实测该资产（40,327,168 字节 / sha256 `7c50f481…`）首 512 字节无 MBR 签名，即**未经 `isohybrid` 处理**，
不能 dd 到磁盘引导 —— 装机路径会失败。引导与 Live 桌面不受影响。构建脚本已补上 isohybrid 步骤与自检
（`platform/build-iso.sh`），重新构建后装机才可用。详见 [`报告/安全审计（SECURITY-AUDIT）-2026-10-02.md`](报告/安全审计（SECURITY-AUDIT）-2026-10-02.md) 的 H-6。

*Correction (2026-10-02): the disk-install claim does not hold for the ISO shipped with this release —
it was measured to have no MBR signature (not isohybrid), so it cannot be written to a disk and booted.
Booting and the live desktop are unaffected. The build script now applies isohybrid; a rebuilt ISO will
support installation.*

### 系统内核之上，全部自研 / A userspace written from scratch

- **PID 1**（`aether-init`）：服务编排、持久化分区、串口日志、故障自愈。
- **窗口合成器**（`aether-compositor`）：直接写 framebuffer 的立即模式合成器，
  自带窗口管理、平铺布局、阴影与圆角、字形缓存、脏行上屏与自适应帧率。
- **终端模拟器**：PTY + VT 解析器，自己实现，不是包一层现成的终端库。
- **中文输入法**：拼音输入 + 候选框。
- **AI 中枢**（`aetherd` + `aether-ipc`）：NDJSON 线协议，**双通道** ——
  Unix socket（`/run/aetherd.sock`，0600，root-only）**优先**，回环 TCP 作为开发/调试备用。
  **每一次工具调用都要过权限闸门**，L2+ 操作弹确认框。
  推理通道支持两种部署：**用户自己的网关**（客户端 → 自有服务器 → 各家模型），
  或**直接指向上游端点** —— 后者需要 API Key，它会持久化在 `/etc/aether/model.json`
  （权限 **0600**，与 `ui.key` 同级处理）。两种方式都**不经过本项目的任何中转服务**：
  密钥只存在用户自己的机器上。

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
