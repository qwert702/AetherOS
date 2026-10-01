# 更新日志 / Changelog

> 本文件是 **`aether.cbnac.com/changelog.html` 的唯一来源**，由 `scripts/gen-site.py` 渲染成页面。
> 每一条都对应仓库里真实存在的提交；版本号、ISO 体积、行数、测试数由生成器**从仓库实测注入**，不手写。
>
> This file is the **single source** for the changelog page. Every entry maps to real commits;
> version, ISO size, line counts and test counts are injected from the repository by the generator.

---

## 未发布 / Unreleased

### 界面重做：从「AI 味」到桌面系统 / UI rework: from "AI-flavoured" to a real desktop

- **静态中性壁纸**：删除极光带、粉彩柔光团、颗粒与暗角；浅色改为纯白 `#FFFFFF → #F4F5F7`。
  *Static neutral wallpaper — aurora bands, pastel blooms, grain and vignette removed; light theme is now pure white.*
- **单一强调色**：删除紫色与全部渐变，AI 相关元素统一为 teal。界面不再有"渐变紫"，那是 AI 产品味的来源。
  *Single accent colour — violet and every gradient removed.*
- **圆角 8/12/16 → 4/6/8**；**五层大软影 → 两层轻影**，层次改由 1px 描边承担。
  *Radii 8/12/16 → 4/6/8; five-layer shadows → two light layers, hierarchy carried by 1px hairlines.*
- **自带字体**：构建期由 `scripts/mkfont.py` 生成 Noto Sans CJK SC 子集（GB2312 + 拉丁，约 5.7 MB，含**真粗体**），
  修掉此前"粗体 = 正文字体"的伪粗。
  *Bundled Noto Sans CJK SC subset with a genuine bold face.*
- **控件库**：六态控件（normal / hover / pressed / focus / disabled / selected）+ 共享命中表，
  并补上此前**完全没有**的**滚轮**事件。
  *Six-state widget library and wheel-scroll support.*
- **设置中心**：8 页分组导航；「时钟与时区」「输入」「外观」「AI」「关于」真实可用，
  设置原子写入 `/var/lib/aether/settings.json` 并持久化（`/var` 是唯一挂真盘的分区）。
  *Settings centre with real, persisted settings.*
- **控制中心**：点顶栏状态簇弹出面板 —— AI 三态、中文输入法开关、24 小时制、快捷跳转。
  *Control centre popover.*
- **主题可切换**：默认明亮（纯白），用户可在设置中心或控制中心切到深色，**切换即时生效**
  （背景缓存与投影合成层同步重建）。
  *Switchable light/dark theme, applied live.*
- **桌面图标**：默认桌面不再预开窗口，改为左侧图标（文件管理 / 终端 / 系统设置）。
  *Desktop icons; the desktop no longer opens windows by default.*
- **Windows 风格标题栏三键**：最小化 / 最大化 / 关闭，替换原 macOS 红绿灯；最小化**真接线**
  （窗口收进 Dock，点 Dock 图标原位恢复）。
  *Windows-style caption buttons replacing the macOS traffic lights, with real minimise.*
- **修复**：点桌面图标会连开多个窗口（把电平信号当成了单击）；鼠标灵敏度被硬编码成 2 倍。
  *Fixed: clicking a desktop icon spawned many windows; mouse sensitivity was hard-coded to 2×.*

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
