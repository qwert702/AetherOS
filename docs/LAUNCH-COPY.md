# 发布文案（可直接复制）

> 2026-10-01 编写。配套 `docs/LAUNCH.md`（机制说明）与 `aether.cbnac.com`（站点）。
>
> **先说一句实话**：站点决定"来的人会不会下载"，不决定"有没有人来"。
> 一个没人知道的网站，访问量仍然是 0。**发出去**才是唯一的入口。
>
> 用法：挑 2–3 个渠道，复制下面整段（标题 + 正文），配上第一张图，发出去。
> 数据都来自仓库实测（ISO 38.5 MB / 23,694 行 Rust / 356 项测试 / 三平台实测）。

---

## 一、中文渠道

### V2EX（节点：/go/create 或 /go/programmer）

**标题**：`[分享] 用 Rust 从 PID 1 写了一个桌面操作系统，能装 htop`

**正文**：

```
做了四个月的一个项目：AetherOS —— Linux 内核 + 完全自研的 Rust 用户态。

内核没重写（Android、ChromeOS 都用 Linux 内核，重写它没有差异化价值），
力气全花在内核之上：PID 1、窗口合成器、终端模拟器（自己写的 PTY + VT 解析器）、
中文输入法、AI 中枢 —— 没有一行现成的桌面组件。

它不是一个"能画个窗口"的 demo：能装市面上的 Linux 软件。
下面这段是真机录屏（未剪辑）：终端敲一行命令，从宿主拉下应用包，
aetherd 装好 htop，再敲 htop 就在自研的桌面里跑起来了。

    wget -O- http://10.0.2.2/i | sh
    htop

几个数字：
· 可引导 ISO 只有 38.5 MB（自己写的合成器/终端/输入法，体积可控）
· 23,694 行 Rust / 43 个源文件 / 7 个 crate
· 356 项单元测试全绿，两个编译目标 0 警告
· QEMU / VirtualBox / VMware 三个平台实测开机
· 16 张界面截图做逐像素回归

AI 中枢带逐工具权限闸门：每次工具调用都过闸门，L2+ 操作弹确认框并记审计；
架构是 客户端 → 你自己的网关 → 各家模型，系统里不存上游 API Key。

下载（38.5 MB，附 SHA256）：https://aether.cbnac.com/download.html
源码（GPL-3.0）：https://github.com/qwert702/AetherOS

目前是 v0.1.0 早期版本，欢迎试用并拍砖。想听的主要是两类反馈：
1) 在你的虚拟机里能不能顺利进桌面；2) 你希望它下一步补什么。
```

**配图**：`docs/demo-htop.gif`（最强）或 `docs/host-ui-light-desktop-clean.png`
**发布时机**：工作日上午 10–11 点（V2EX 白天活跃）

---

### 少数派（sspai.com）

**标题**：`我把操作系统从 PID 1 开始写了一遍：AetherOS 的取舍`

**正文要点**（少数派偏"思路与取舍"，不是罗列功能）：

```
1. 为什么不用自己写的内核 —— 差异化不在内核里
2. 为什么坚持"能装真实软件"这条底线 —— 否则就只是玩具
3. 去"AI 味"这件事：我把界面里的渐变、紫色、光晕全删了
   （AI 产品味的来源之一就是"彩色渐变底座 + 极光壁纸"）
4. 38.5 MB 是怎么做到的：自己写合成器/终端/输入法，体积才可控
5. 权限闸门的设计：AI 要动系统，先过闸门，L2+ 弹确认
```

**配图**：桌面 + 设置中心 + 控制中心三张（`docs/host-ui-light-*.png`）

---

### 掘金 / 知乎

**标题**：`用 Rust 写一个操作系统用户态：从 PID 1 到窗口合成器`

**正文**：技术向，讲实现（可直接用 `docs/ARCHITECTURE.md`、`docs/HANDOVER.md` 的素材）：

```
· 合成器为什么是"立即模式 + 直接写 framebuffer"，以及 5 层阴影怎么把帧率打到 91ms
  （优化到 15.5ms：脏行上屏 + 字形缓存 + 自适应帧率）
· IPC 线协议：NDJSON over TCP，serde tag/content
· 权限闸门：每次工具调用怎么判定 L0–L3
· 怎么让静态二进制与动态依赖的市面软件都能跑（ncurses + terminfo 是关键）
```

**结尾**：给站点与仓库链接。

---

### B 站（视频，转化率最高的形式）

**标题**：`我用 Rust 从零写了个操作系统，它能装 htop`

**简介**（前三行决定有没有人点开）：

```
不是 Linux 换皮：从 PID 1 到窗口合成器、终端、中文输入法，全部自研。
可引导 ISO 只有 38.5 MB，能装市面上的 Linux 软件。
下载与源码在简介下方。
```

**视频脚本（30–40 秒，照着录）**：
1. 0–5s：QEMU 启动 → 进桌面（白底、Dock、图标）
2. 5–15s：点终端图标 → 敲 `wget -O- ... | sh` → 显示安装过程
3. 15–25s：敲 `htop` → htop 跑起来（**这是高潮，别剪**）
4. 25–35s：点设置中心 → 切深色主题（展示"这是个完整系统"）
5. 结尾 3s：站点地址

---

## 二、英文渠道（国际社区，流量最大但门槛也最高）

### Hacker News（Show HN）

**标题**：`Show HN: AetherOS – a desktop OS whose userspace is written from scratch`

**正文**（HN 讨厌营销腔，只讲事实与取舍）：

```
AetherOS is a desktop OS built on the Linux kernel with a userspace written from
scratch in Rust: PID 1, window compositor, terminal emulator (own PTY + VT parser),
Chinese IME, and an AI hub with per-tool permission gating.

I deliberately did not write a kernel. Android and ChromeOS use the Linux kernel;
the differentiation is above it. What I wanted was the combination that hobby
projects rarely reach: everything above the kernel is mine, and it still installs
and runs real software. The demo installs htop 3.3.0 from the official Ubuntu
24.04 archive and runs it on the self-written desktop.

Numbers: bootable ISO 38.5 MB, 23,694 lines of Rust across 7 crates, 356 unit
tests, zero compiler warnings on both targets, verified on QEMU/VirtualBox/VMware.
16 UI screenshots are regression-checked pixel by pixel.

The compositor writes straight to the framebuffer (immediate mode). Early
perf: 91 ms/frame with 5-layer shadows, now 15.5 ms after dirty-row upload,
glyph caching and adaptive frame rate.

The AI hub never stores upstream API keys: client -> your own gateway -> any model.
Every tool call passes a permission gate; L2+ actions prompt and are audited.

Site: https://aether.cbnac.com/  Source (GPL-3.0): https://github.com/qwert702/AetherOS

It is an early v0.1.0. I would especially like to hear whether it boots on your
hypervisor, and what you think the highest-value next subsystem is.
```

**发布时机**：美国太平洋时间周二–周四 07:00–09:00（= 北京时间 22:00–24:00）

---

### Reddit

- **r/osdev**（最相关，人不多但极精准）
  标题：`AetherOS: everything above the kernel written from scratch (compositor, PID 1, terminal, IME)`
- **r/rust**（强调 Rust 实现细节与工具链）
  标题：`A userspace written from scratch in Rust: compositor, PTY/VT terminal, IME, AI hub`
- **r/linux**（强调"能装真实软件"这条底线）

三个板块**不要同一天发同样的内容**：每个板块改写开场白，先说该板块最在意的那一点。

---

## 三、长期动作（一次发布只能带来一波）

| 动作 | 说明 | 成本 |
|---|---|---|
| **提交 awesome-list** | [`awesome-os`](https://github.com/awesome-os/awesome-os)、[`awesome-rust`](https://github.com/rust-unofficial/awesome-rust)、`awesome-osdev` 各一条 PR | 每个 10 分钟，长期带量 |
| **仓库 Social preview** | 上传 `site/assets/img/og-cover.png`（Settings → Social preview） | 2 分钟 |
| **Homepage 字段** | 仓库首页填 `https://aether.cbnac.com` | 1 分钟 |
| **Topics** | 确认 16 个 topics 里有 `rust` `operating-system` `osdev` `compositor` | 2 分钟 |
| **每周一条进度** | 在 V2EX / X / 即刻发"这周做了什么"（有内容才发，别硬凑） | 持续 |

## 四、诚实的预期

- 一次发布通常带来**几十到几百访问、个位数 star**；这是正常的，不是失败。
- 真正起作用的是**持续**：发一次 → 看反馈 → 改 → 再发。第一次发布的作用是"让项目存在"。
- 别刷 star、别买量：被发现一次刷量，社区信任就没了，而这个项目唯一能靠的就是可信度。
