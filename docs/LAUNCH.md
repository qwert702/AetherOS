# 让 AetherOS 被看见：发布与传播清单

> 2026-09-29 编写。针对"仓库没人看"这个具体问题，给出**机制说明 + 已做的改动 + 只有你能做的事 + 现成文案**。

## 一、先说清机制：GitHub 不会"推荐"你

GitHub **没有内容推荐算法**（不像 B 站/抖音/YouTube 会把你推给陌生人）。一个仓库的流量只有五个来源：

| 来源 | 说明 | 本项目现状 |
|---|---|---|
| **站内搜索** | 名字 / 描述 / topics / README 关键词命中 | 描述与 16 个 topics 都设了 ✅ |
| **外部链接** | HN、Reddit、X、知乎、博客、群聊……**新项目 90%+ 的 star 来自这里** | ❌ 24 天里没有任何一次对外发布 |
| **Trending** | 需要**短时间内的 star 爆发**，而爆发来自上一行 | ❌ 不可能凭空发生 |
| **关注者 / 组织** | 别人 follow 你之后能看到你的动态 | ❌ 新账号，0 关注者 |
| **被收录** | awesome-list、周刊、别人文章引用 | ❌ 尚未提交 |

**结论：不是"东西不行所以没人看"，而是根本没有入口。**
2026-09-05 建库到 09-29，24 天里唯一的变化只有代码本身 —— 对 GitHub 来说，一个没有 star、
没有 release、没有外部引用的仓库，**等于不存在**。

还有一个客观障碍：**README 只有中文**。全球 osdev / Rust 社区主要在英文世界，
他们点进来三秒读不懂就走了。这不是内容问题，是入口语言问题。

## 二、本次已做的（把"入口"造出来）

| 动作 | 效果 |
|---|---|
| 建 **Release v0.1.0** 并附 ISO | 任何链接都能落到"下载就跑"，不再需要别人自己建 ISO |
| 写 **`README.en.md`** | 国际读者能读懂；英文关键词也能被站内搜索命中 |
| 写 **`docs/index.html`**（落地页） | 可挂 GitHub Pages，填进仓库 Homepage 字段；页面里写了 SEO 用的 title/description |
| README 顶部加**下载入口 + 实拍图** | 首屏三秒内让人知道"这是什么、怎么跑起来" |
| 本文档 | 现成文案，复制即可发 |

## 三、只有你能做的（按性价比排序）

1. **发一次**（最重要，一次就能超过过去 24 天的总和）。文案见第四节。
2. **录一段 20–40 秒的屏**（桌面启动 → 敲命令装 htop → htop 跑起来）。
   操作系统项目里**动图/视频的转化率远高于文字**。B 站 + YouTube 各发一份，README 顶部放 GIF。
3. **社交预览图**：仓库 Settings → Social preview 上传一张（就用 `docs/screenshot-htop-app.png` 或落地页截图）。
   别人在群里分享你的链接时，这张图决定有没有人点。
4. **开启 GitHub Pages 并把地址填进 Homepage**：
   Settings → Pages → Source 选 `main` 分支的 `/docs` 目录 →
   地址形如 `https://aether.cbnac.com/` → 再回仓库首页把 Homepage 填上。
5. **提交 awesome-list**（一条 PR 能带来长期流量）：
   - [`awesome-osdev`](https://github.com/awesome-os/awesome-os) / [`awesome-rust`](https://github.com/rust-unofficial/awesome-rust)（Operating Systems 一节）
   - [`awesome-selfhosted`](https://github.com/awesome-selfhosted/awesome-selfhosted) 不太合适（不是服务类），别硬塞
   - 提交话术：「A hobby OS with a fully self-written Rust userland (compositor, init, terminal) and an AI hub with per-tool permission gating. Bootable 38 MB ISO.」（事实，不吹）
6. **中文社区各一篇**：知乎、V2EX（分享创造节点）、掘金、少数派。
   中文社区对"全自研系统 + AI 内建"这类题目接受度很高，且竞争小。
7. **加徽章**（可选）：build/tests/license 徽章能提升信任度，但没有 CI 之前别放假的 passing 徽章。

## 四、现成文案（复制即用）

### Hacker News（Show HN）

> **Show HN: AetherOS – a hobby OS with a fully self-written Rust userland and an AI hub**
>
> I've been building an OS from the Linux kernel up for the last few months. The kernel is stock
> (Buildroot), but everything above it is mine: a compositor + desktop shell (window management,
> terminal emulator, font rendering, IME), PID 1 with service supervision, and `aetherd` — an AI hub
> with 15 tools behind a 4-level permission gate.
>
> It boots in QEMU/VirtualBox from a **38 MB ISO**, and it can install real market software:
> there's a packer that resolves ELF dependencies recursively, refuses to bundle anything from the
> glibc family, verifies symbol versions against the target libc, and ships terminfo when needed.
> htop 3.3.0 from Ubuntu 24.04 runs on the desktop (screenshot in the repo).
>
> 21k lines of Rust, 334 unit tests, zero warnings on both targets, and a 12h43m soak with no
> crashes. Still early: no X11/Wayland client libs yet, so no GUI apps.
>
> Repo: https://github.com/qwert702/AetherOS — ISO: (release link)
> Happy to answer anything about the compositor, the permission model, or the app packing.

### Reddit — r/rust

> **AetherOS: a desktop OS where the entire userland is Rust I wrote (compositor, init, terminal, AI hub)**
>
> Short version: stock Linux kernel, ~21k lines of my own Rust above it. `aether-compositor`
> (13k lines) does window management + a terminal emulator + font rendering straight to fbdev.
> `aetherd` is an AI hub: 15 tools, L0–L3 permission gating, offline-intent + cloud routing.
>
> The part I'm happiest with: a packer that makes market binaries runnable — it walks ELF
> `DT_NEEDED` recursively, never bundles glibc (mixing two libcs breaks on GLIBC_PRIVATE symbols),
> and checks `.gnu.version_r` against the target libc's `.gnu.version_d`. htop works.
>
> 38 MB bootable ISO, 334 tests, GPL-3.0. Feedback on the architecture very welcome.

### Reddit — r/osdev

> **Hobby OS: stock Linux kernel + fully self-written Rust userland (compositor, PID 1, terminal)**
>
> I know "Linux kernel" is unusual for this sub — the interesting part for me was everything above
> it: my own compositor with 4 tiling layouts, my own init with dependency-ordered services and
> restart policy, a PTY-based terminal emulator, and an AI hub with per-tool permission gating.
>
> Boots in QEMU from a 38 MB ISO. Wrote up the whole build + the bugs I hit (kconfig "is not set"
> silently not applying, test artifacts baked into the ISO changing its size, etc.) in the repo docs.
>
> Would love criticism on the compositor/terminal side.

### V2EX（分享创造 / 程序员节点）

> **［分享创造］我从 Linux 内核往上写了一个操作系统：用户态全是自己写的 Rust**
>
> 内核用的是现成的（Buildroot 构建），但内核之上全部自研：合成器 + 桌面 Shell（窗口管理、
> 终端模拟器、字体渲染、中文输入法）、PID 1 与服务监管、以及 `aetherd`（AI 中枢：15 个工具、
> L0–L3 四级授权闸门、离线/云端混合路由）。
>
> - 可引导 ISO 只有 **38 MB**，QEMU/VirtualBox 直接开
> - **能装市面上的 Linux 软件**：自带打包器递归解析 ELF 依赖、不打包 glibc 家族（混用两套 libc
>   必撞 GLIBC_PRIVATE）、校验符号版本；实测 htop 3.3.0 装完就能跑
> - 21,490 行 Rust、334 项测试、双目标 0 警告、连续 12 小时 43 分不崩
> - 仓库里记了每轮审计发现的问题，包括**没修的**
>
> 链接：（仓库 / Release）
> 想听听大家对"AI 内建到系统层"这种做法的看法。

### 知乎 / 掘金（标题 + 开头）

> 标题：**我写了一个操作系统：Linux 内核 + 全部自己写的 Rust 用户态，38 MB，能装 htop**
>
> 开头段落：这不是一个"改主题的发行版"。内核之上——合成器、窗口管理、终端模拟器、字体渲染、
> PID 1、服务监管、以及带权限闸门的 AI 中枢——一共 21,490 行 Rust，都是我从零写的。
> 可引导 ISO 38 MB……（正文展开：架构图、怎么装市面上的软件、踩坑、如何验证）

### Bilibili / YouTube 视频简介

> 从 Linux 内核往上写一个操作系统 ｜ AetherOS 0.1 预览
> 全自研 Rust 用户态：合成器 / 终端 / PID 1 / AI 权限闸门。38 MB ISO，
> 桌面里直接装上并运行 Ubuntu 的 htop。项目开源（GPL-3.0）：（链接）

### X / 微博（短）

> 写了个操作系统：内核用现成的，内核之上全是自己写的 Rust —— 合成器、终端、PID 1、
> 带权限闸门的 AI 中枢。ISO 38 MB，能装并运行 Ubuntu 的 htop。334 项测试全绿。GPL-3.0。
> （链接）#rust #osdev

## 五、发布纪律（别被当成 spam）

- **不要同一天群发所有社区**：挑 1–2 个，隔几天再发下一个；管理好自己的回帖精力。
- **每个社区文案要改**（本清单已按社区分别写好，别复制同一段）。
- **前 24 小时的评论决定成败**：HN/Reddit 的排名看早期互动，尽量守着回。
- 标题**说事实不说形容词**（"38 MB ISO，能跑 htop" 优于 "惊艳的操作系统"）。
- 被批评时别急着辩解：osdev/rust 社区吃"我知道它在哪不行"这套，仓库里那些"没修的问题"
  反而是诚信加分项。
- 不要买 star、不要互刷 —— 会被举报且不可逆。

## 六、度量（判断哪个渠道有效）

1. 仓库 **Insights → Traffic**（需要写权限，只保留最近 14 天）：看 views / clones / referrers。
   每次发布后记录 referrer，就知道哪个渠道真的带来人。
2. 记录每次发布后的 24 小时 star 增量，做成小表格；两次之后就能定下"哪个社区值得再发"。
3. Release 的资产有下载计数（当前 0）—— 这是"有人真的想试"的最硬指标。

## 七、发布排期表（含耗时与实际预期）

| 顺序 | 动作 | 耗时 | 渠道 | 合理预期 |
|---|---|---|---|---|
| 0 | **设置社交预览图**（Settings → Social preview 传 `docs/demo-cover.png`） | 5 分钟 | — | 分享链接的点击率成倍提升（这是**零成本最高回报**的一步） |
| 1 | 发 V2EX「分享创造」+ 知乎各一篇（文首放 `docs/demo-htop.gif`） | 1 小时 | 中文社区 | 首日 100–1000 UV，10–50 star |
| 2 | 录 3–5 分钟 B 站视频（用 GIF 的分镜当脚本），简介放仓库+Release | 2–4 小时 | B 站 | **唯一有推荐算法的渠道**，长尾流量，看封面/标题可能几千–几万播放 |
| 3 | Show HN + r/rust + r/osdev（英文，美东上午发） | 2 小时 | 英文社区 | 上首页则 50–300 star；没上就是几十 UV |
| 4 | 技术博客系列，每周 1 篇（见下） | 每周 2 小时 | 掘金/知乎/dev.to | 每篇都是一次独立分发，累积搜索长尾 |
| 5 | awesome-list PR + 在 r/osdev 相关问题下有内容地回答（签名带项目） | 一次性 | — | 被动流量，慢但持续 |
| 6 | v0.2 发布（沿用同一套素材重发一次） | — | 全部 | 老读者回流 + 新读者 |

**技术博客选题**（这些是别人真正会搜、也真正难的问题，写出来就有价值）：
1. 「在 Rust 里从零写一个走 fbdev 的窗口合成器」
2. 「为什么绝不能把 glibc 打进应用包：GLIBC_PRIVATE 符号冲突实录」
3. 「AI 内建到系统层：L0–L3 逐工具权限闸门怎么设计」
4. 「把市面上的 Linux 程序装进自研系统：ELF 依赖递归解析 + 符号版本校验」
5. 「一个 kconfig 的 `is not set` 写错，安全加固静默失效」

## 八、必答题（HN / Reddit 一定会问，先想好答案）

| 会被问 | 不要这样答 | 应该这样答 |
|---|---|---|
| 「内核又不是你写的，凭什么叫操作系统？」 | 辩解或回避 | 「Android、ChromeOS 也用 Linux。价值在内核之上：从 PID 1 到合成器没有任何现成组件，这部分 21,490 行 Rust 是我写的。」（仓库里有可核对的清单） |
| 「不就是一堆 Rust 程序吗？」 | 情绪化 | 列出从 PID 1 / 合成器 / 终端 / IME 到 AI 中枢的自研边界，并给可复算的数字与测试数 |
| 「这东西有什么用？」 | 吹成要取代 Ubuntu（HN 最反感） | 诚实定位：研究/教学/自用性质；真正的技术贡献是 **AI 内建的权限模型** 与 **应用打包机制**，这两点别人可以直接借鉴 |
| 「怎么证明不是 PPT？」 | 只给截图 | 给三条可复跑命令（`cargo test`、`scripts/repo-stats.py --check`、`scripts/try-aether.sh`）+ Release 里能下载的 ISO |

## 九、一句话总结

**代码已经够格被看了，缺的只是"发一次 + 一个会传播的形态"。**
入口已经全部造好（Release+ISO、英文 README、落地页、演示动图、封面、一条命令跑起来、
现成文案）；接下来是**排期表第 0–3 步由你的账号执行**，其中第 0 步只要 5 分钟、回报最高。

**预期管理**：一个 hobby OS 的合理目标是 3 个月内 300–1000 star、5–20 个讨论、1–3 个外部贡献者。
人气 = 分发次数 × 每次的分发质量，它是**持续动作**，不是一次性动作 —— 所以第 4、6 步比第 1 步更重要。
