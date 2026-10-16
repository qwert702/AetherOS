# VMware 完整装机 + 自动更新 方案（VMWARE-OTA）- 2026-10-15

> **一句话**：在 VMware 里把 AetherOS 装成**真正的系统**（装到虚拟磁盘、从盘引导、`/var` 持久化），
> 并把"以后每次更新"变成**宿主一条命令**（或干脆让 guest 自己拉）。
>
> 本文是**方案**，不是已完成记录。每条都标了「已验证 / 待做」，验收判据都写成**可从产物取证**的形式。

---

## 1. 目标与验收判据

| 场景 | 目标 | 判据（从产物取证，不看"应该成功了"） |
|---|---|---|
| **首次装机** | VMware 里一条宿主命令 → 无人值守装成真系统 | ① 从盘引导（**拔掉光驱**）进桌面：串口出现 `[aether-init] 持久化分区已挂载: /dev/sda2 -> /var` ② 串口出现 `巡检#1 … 服务=aetherd=✓ compositor=✓ getty=✓ network=✓ ops=✓` ③ 重启后 `/var` 的写入仍在 |
| **日常更新（文件级）** | 改一处代码 → 宿主一条命令 → 重启后生效，**全程不碰 guest** | ① 更新源里的包被 guest 自己拉到 ② `apply.list` 出现 ③ 重启后目标文件哈希 = 包内哈希 ④ 版本号/内容确实变了 |
| **整机更新（系统级）** | 换新 ISO → 宿主一条命令 → 保留 `/var` 换成新系统 | ① 更新后串口显示新版本 ② `/var` 里更新前写的探针文件仍在 ③ ISO 内的字体/二进制哈希变了 |
| **回滚** | 更新前自动快照，一条命令回到上一版 | `vmrun revertToSnapshot` 后串口/版本回到旧值；`/var` 状态符合预期 |

> 判据设计原则沿用仓库既有规矩：**每条都要有正向对照**（例如"文件变了"必须同时给出"哈希确实不同"）。

---

## 2. 现状盘点（已有能力，别重复造）

| 能力 | 在哪 | 状态 |
|---|---|---|
| 非交互安装器 | `aether-install/src/main.rs:17` — `aether-install --disk /dev/vda --yes [--no-persist] [--wipe]` | ✅ 可用（实测装过盘） |
| 分区布局 | sda1 = 系统（ISO 的 isohybrid 拷贝）+ sda2 = ext4 `/var` | ✅ **已实测**（装机验证四步） |
| `/var` 自动挂载 | `aether-init` `mount_persist` → `/dev/sda2 -> /var` | ✅ 已实测 |
| 离线更新链 | `aetherd/src/update.rs`：`incoming/` → `staged/` → `ready.json` → `apply.list` | ✅ 机制完整（校验/白名单/拒降级） |
| 更新应用 | `aether-init/src/update.rs`：PID 1 在**下次启动**应用 | ✅ |
| 更新界面 | `aether-compositor/src/updateview.rs` | ✅ |
| HTTP 客户端 | `aetherd` 已依赖 `ureq`（`llm.rs:51`、`llm.rs:130`） | ✅ **能自己发 HTTP** —— 这是"自动更新"的地基 |
| 引导配置 | `platform/br2-external/grub-embedded.cfg`、`isolinux.cfg` | ✅ 可加引导参数 |
| 发布链 | GitHub Release v0.1.1（42,991,616 字节 / `eea68da9…`）+ 站点公布哈希 | ✅ |
| VMware 工具 | `vmrun 1.17.0`（`D:\CBN-HT\Desktop\asdf\vmrun.exe`）· Workstation | ✅ |
| 现有 VMware VM | `D:\aether-vm\AetherOS-1011\AetherOS-1011.vmx`：**只有光驱、没有硬盘**；`serial0` **已落盘**到 `…-serial.txt` | ⚠️ 需要加磁盘 |

**缺的三样**：① 无人值守安装（现在必须有人在 guest 里敲命令）② 更新包的**生成器**（现在要手工拼目录+算哈希）③ 更新包的**投递/拉取**（现在要人工放进 `incoming/`）。

---

## 3. 决定性约束（先说清楚，否则方案会写错）

**VMware 的 CLI 没有按键注入**：`vmrun` 只能在 guest 里跑程序（`runProgramInGuest`），而那需要 **VMware Tools**，
AetherOS 里没有 ⇒ **任何"进 guest 敲命令"的方案在 VMware 上都不可行**。
（对比：VirtualBox 有 `keyboardputscancode`，所以上次装机验证是在 VirtualBox 做的。）

于是只有两条路，整个方案必须建立在这两条上：

* **① 引导时自动发生** —— 无人值守安装（`aether.autoinstall=1` → 引导期自动跑安装器）
* **② guest 自己发起** —— `aetherd` 自己上网拉更新（不需要任何外部输入）

**唯一可靠的观测通道是串口**（现有 VM 已配 `serial0.fileType=file` ✓）：方案里所有判据都从串口文本取，
不依赖截图、不依赖进 guest。

---

## 4. 方案（四层，每层可独立验收）

### T1 首次安装：引导期无人值守（**先做这个**）

* **改引导配置**：`platform/br2-external/grub-embedded.cfg` / `isolinux.cfg` 增加一个菜单项
  `AetherOS（自动安装到本机磁盘）`，内核参数加 `aether.autoinstall=1`；**默认项仍是普通启动**（不改变现有行为）。
* **改镜像侧**：启动早期检测"`aether.autoinstall=1` 且**根是只读 ISO（live 介质）**"→ 自动执行
  `aether-install --disk <自动选盘> --yes` → 结果打到**串口** → 不进入桌面、不留在 live。
  * 选盘规则：跳过光驱与可移动介质，取最大的固定磁盘（VMware 下就是 `/dev/sda`）。
  * ⚠️ **必须两个条件同时成立**：只看参数会导致"装完从盘引导再装一次"（自杀）；
    加上"根是只读 ISO"的条件后，从盘引导（ext4 根）永远不会触发。**这是本层最容易写错的地方。**
* **宿主一条命令**：`scripts/vmware-install.ps1`
  1. 建/复用 VM：加硬盘（建议 4 GB 稀疏）、内存 2 GB、`serial0` 落盘、`bios.bootOrder = "cdrom,disk"`；
  2. 挂 v0.1.1 ISO（或先从 Release 下载并**校验哈希**）；
  3. 用 `vmrun start` 启动 → **轮询串口文本**直到出现安装完成标记；
  4. 改引导顺序为磁盘 → 重启 → 判据：`持久化分区已挂载` + 五服务 ✓；
  5. 打一份 `docs/证据/VMware装机-<日期>.md`（串口摘录）。
* **工作量**：镜像侧半天 + 宿主脚本半天。**风险低**（安装器本身已实测可用）。

### T2 日常更新：guest 自己拉（**最省事，主推**）

* **新增配置**：`/var/lib/aether/update.json`（与 `modelcfg` 风格一致）
  ```json
  { "url": "http://<更新源>/aether/", "check_minutes": 60, "auto_apply": false }
  ```
* **`aetherd` 轮询**（新增，几十行）：定时 `GET <url>/update.json` → 比较版本 → 下载包（`ureq` ✓ 已有）
  → **校验 sha256 + 大小**（沿用现有 `stage()` 的白名单/拒降级）→ 放进 `incoming/` → 调 `stage()`；
  `auto_apply=true` 时写一个"下次启动应用"的标记（PID 1 已有的机制 ✓），`false` 时只通知界面（已有 ✓）。
* **更新源放哪**（三种都可行，见 §9 待你定）：
  * **VPS**（`aether.cbnac.com`）：已在跑、外网可达 → 最稳，推荐；
  * **局域网宿主 HTTP**：`python -m http.server`，最简单，只适合本机测试；
  * **GitHub Release**：公开、自带版本管理；但要 guest 能出网。
* **宿主一条命令**：`scripts/mkupdate.py` 生成包 → 放到源的目录 → 完事（**不需要碰 guest**）。
* **工作量**：`mkupdate.py` 半天 + `aetherd` 轮询半天。

### T3 整机更新：保留 `/var`（系统级改动才用）

* **安装器加 `--keep-var`**（默认**关**，破坏性操作要先确认 —— 与审计 M-8 同一原则）：
  装法不变（把新 ISO 的 MBR+系统分区写进磁盘头部），但**跳过** `④ 清签名 + mkfs 持久分区`，
  并沿用磁盘上已有的 `/var` 分区项 ⇒ **`/var` 原样保留**。
* **宿主一条命令**：`scripts/vmware-update.ps1` = 更新前 `vmrun snapshot` → 挂新 ISO → 无人安装（`--keep-var`）→ 从盘重启 → 判据（新版本 + `/var` 探针仍在）。
* **工作量**：镜像侧半天 + 脚本半天。

### T4 回滚与观测

* **回滚**：`vmrun snapshot` / `revertToSnapshot`（宿主侧，无需 guest 配合 ✓）；更新脚本**默认先快照**。
* **观测**：串口落盘（已有 ✓）；关键判据全部写成"串口里必须出现某行"或"磁盘上某文件哈希必须等于某值"。

---

## 5. 要动的文件（清单）

| 文件 | 新/改 | 干什么 |
|---|---|---|
| `platform/br2-external/grub-embedded.cfg`、`isolinux.cfg` | 改 | 加"自动安装"菜单项 + `aether.autoinstall=1`；默认项不变 |
| `aether-init/src/autoinstall.rs`（新）+ `main.rs` | 新/改 | 引导期检测（参数 **且** 根是只读 ISO）→ 自动装盘 → 结果打串口 |
| `aether-install/src/main.rs` | 改 | 加 `--keep-var`（默认关）；把"自动选盘"抽成可测函数 |
| `aetherd/src/updater.rs`（新）+ 配置 | 新 | 轮询更新源、下载、校验、投进 `incoming/`、`stage()` |
| `scripts/mkupdate.py` | 新 | 由构建产物生成更新包（文件 + `update.json` + 哈希），并自检 |
| `scripts/vmware-install.ps1`、`vmware-update.ps1` | 新 | 宿主一条命令（建 VM/挂 ISO/串口判据/快照/回滚） |
| `docs/报告/VMware装机与自动更新（VMWARE-OTA）…` | 本文件 | 方案与验收判据 |
| `docs/证据/VMware装机-<日期>.md` | 新 | 装机/更新各一份串口证据 |

> 每条新判据都要**进门禁**（`scripts/gates.py`）：例如"autoinstall 只在只读根上触发"、
> "`--keep-var` 不碰持久分区"、"更新包哈希校验" —— 都写成**不需要真实虚拟机的纯逻辑自检**，
> 这样本机就能跑（沿用这两天 `--selftest` 的做法）。

---

## 6. 验收判据（逐条可取证）

| 场景 | 判据 |
|---|---|
| T1 无人安装 | 串口出现安装完成的固定标记；从盘引导后 `持久化分区已挂载`；**拔掉光驱**仍能进桌面 |
| T1 负向对照 | **不传** `aether.autoinstall=1` 时，绝不自动装盘（防止"live 直接毁盘"） |
| T2 自动更新 | 源里换包后 ≤ `check_minutes`，guest 自动出现 `incoming/<pkg>`；重启后目标文件哈希 == 包内哈希；`update.log` 有记录 |
| T2 负向对照 | 篡改包内一个字节 → 拒收（哈希不符），且**不写** `apply.list`；降级包被拒 |
| T3 整机更新 | 更新后版本号变；`/var/tmp/probe` 内容不变；持久分区 UUID 与更新前相同 |
| T4 回滚 | `revertToSnapshot` 后版本回到旧值 |

---

## 7. 已知坑（这两天踩过的，直接复用，别重演）

1. **VMware 没有按键注入** ⇒ 方案必须无人化（§3）。想"进 guest 敲一下"的捷径全都不通。
2. **autoinstall 的触发条件必须同时含"根是只读 ISO"**，否则装完从盘引导会无限重装（毁盘级）。
3. **`--keep-var` 默认必须关**：它是"就地换系统"，误用会丢持久数据。
4. **串口是唯一可靠的文本观测通道**：VMware 用 `serial0.fileType=file` + `startConnected=TRUE`（现有 VM 已如此 ✓）；截图这条路我没有能力看（模型限制），判据一律走文本。
5. **更新机制的三条纪律不能绕**：路径白名单、拒绝降级、`apply.list` 最后写（否则"半个包就应用"）。
6. **虚拟机状态别用 savestate/resume**：VirtualBox 上实测恢复后 sshd 就废了；VMware 用 **snapshot**（干净）且恢复后要**重新验证**服务与网络。
7. **改 `Cargo.lock` 后构建机的离线缓存要补**（宿主 SFTP crate + 索引缓存，见《镜像重建手册》§6）。
8. **构建机是单点**：镜像侧改完必须能重构建 ISO（当前构建机走 host-only `192.168.56.101`，`AETHER_VM_HOST/PORT` 见手册）。

---

## 8. 分批与工作量

| 批次 | 内容 | 产出（可独立验收） | 预估 |
|---|---|---|---|
| **P0** | T1 无人安装（`aether.autoinstall=1` + 安装标记判据） | ✅ **2026-10-16 完成**：VMware 里挂上自动安装版 ISO 开机即自动装盘（`AETHER-AUTOINSTALL: PASS`）→ 自动关机 → 拔掉光驱从盘独立引导进桌面。证据：[VMware无人装机（AUTOINSTALL）-2026-10-16.md](../证据/VMware无人装机（AUTOINSTALL）-2026-10-16.md) | 1 天｜已完成 |
| **P1** | `mkupdate.py` + `aetherd` 轮询 + 更新源 | 改代码后**一条命令**推送，guest 自己更新 | 1 天 |
| **P2** | `--keep-var` + 整机更新 + 快照回滚 | 换内核/系统也能一条命令 | 1 天 |
| **P3** | 界面"检查更新"接到自动源（已有界面页 ✓） | 点一下就更新 | 半天 |

P0 完成后，"在 VMware 里装完整系统"这件事就闭环了；P1 完成后，"以后每次更新不麻烦"就成立了。

---

## 9. 需要你定的三件事

1. **更新源放哪**：① **VPS `aether.cbnac.com`**（已在跑、外网可达 → 推荐）② 局域网宿主 HTTP（最简单）③ GitHub Release（公开但依赖 guest 出网）。
2. **更新怎么应用**：① 检查自动、**应用需确认**（更安全，默认建议）② 全自动（无人值守场景更省事）——可做成配置项，两种都留。
3. **VMware 系统盘**：建议 **4 GB 稀疏盘 + 2 GB 内存 + 2 vCPU**（现在是 1 GB 内存、无磁盘）。要不要我沿用现有 `AetherOS-1011` 这台（加盘）还是新建一台干净的？

---

## 10. 我不想含糊的地方（诚实边界）

* 本方案里 **T1 的"非交互安装器"和"分区布局/`/var` 挂载"是已实测的**（2026-10-14 装机验证四步，见
  [`装机验证（INSTALL-VERIFY）-2026-10-14.md`](../证据/装机验证（INSTALL-VERIFY）-2026-10-14.md)）；
  但**"在 VMware 里无人安装"本身还没做过** —— 上面 P0 就是把它做实。
* **`aetherd` 轮询更新源是新功能**，现在只有"管理员把包放进 `incoming/`"这半套。
* VMware 侧的串口/快照能力是**本机实测过的工具能力**（`vmrun` 可用、`serial0` 已落盘），但**装机流程尚未在 VMware 上跑通**。
---

## 11. P1 实施细节（2026-10-16 补：更新源已定为 **VPS `aether.cbnac.com`**）

### 11.1 接口已经钉死（读自现有代码，不要另造）

`aetherd/src/update.rs` 认的包格式：

```jsonc
// <包目录>/update.json
{
  "version": "0.1.2",                 // 形如 1.2.3，只比较数字段；比当前旧 ⇒ 拒收
  "files": [
    { "path": "/var/apps/hello/hello", "sha256": "<64 位 hex 小写>", "bytes": 1234 }
    // path 是**绝对目标路径**；包内相对路径 = 去掉开头的 '/'（verify_bundle 的取法）
    // 三条必查：绝对、无 ..、无控制字符、命中白名单；且 sha256 必须是 64 位 hex、
    // 同一 path 不许出现两次。白名单**不含 /usr/bin/** ⇒ 系统组件走整机路径（--keep-var）
  ]
}
```

投递与生效链（**都已存在，别重写**）：

```
incoming/<包目录>/          管理员/程序把包放这里（界面只能选 incoming 的直接子目录）
   ↓ aetherd stage()        逐文件 sha256 校验（不符整包拒绝）→ 写 staged/<id>/ + ready.json
   ↓ 最后写 apply.list      每行 <sha256>\t<bytes>\t<target>（存在即表示暂存完整）
   ↓ 下次启动 aether-init    PID 1 按 apply.list 应用 → 写 applied 记录
```

### 11.2 P1 要新写的东西（两半）

| 文件 | 新/改 | 作用 | 验收判据 |
|---|---|---|---|
| `scripts/mkupdate.py` | 新 | **宿主侧**：从构建产物生成包（`update.json` + 文件 + 逐文件 sha256），并校验"目标都在白名单内" | `--selftest`（纯逻辑，本机可跑）；生成的包能被 `aetherd stage()` 接受 |
| `aetherd/src/updater.rs` | 新 | **guest 侧**：读 `/var/lib/aether/update.json` 的 `{"url": ..., "check_minutes": ..., "auto_apply": false}` → 定时 `GET <url>/update.json` → `version_is_newer` 才下载 → 逐文件校验 → 投进 `incoming/` → 调 `stage()` | 单元测试：版本比较、拒降级、哈希不符拒收、URL 拼接；不联网也能测（注入一个本地目录/桩） |
| `aetherd/src/main.rs` | 改 | 把 updater 起进服务循环 | `aetherd` 启动日志出现 updater 行 |
| `deploy-site.py` | 改 | 把更新包目录一并发布到 VPS（`/var/www/aether.cbnac.com/updates/`），并在站点上给 `update.json` | 部署后 `curl https://aether.cbnac.com/updates/update.json` 返回清单 |

### 11.3 端到端验收（P1 的"完成"定义）

1. 宿主：改一行代码 → `python scripts/mkupdate.py --version 0.1.2 --out updates/`（**不碰 guest**）
2. 发布：`deploy-site.py`（把 `updates/` 传上 VPS）
3. guest：`aetherd` 在一个 `check_minutes` 周期内自动拉到 → `/var/lib/aether/update/incoming/<包>` 出现
4. 重启 guest → 目标文件哈希 == 包内哈希 → 版本号/可观察行为确实变了
5. **负向对照**：把包内一个字节改坏 → 拒收（不写 `apply.list`）；把 `version` 改成更旧 → 拒降级

### 11.4 安全要点（沿用既有纪律）

* `UPDATE_ROOT` 只允许 root 写（既有前提）；updater 只写 `incoming/`，**不直接写系统路径**；
* 下载后**逐文件 sha256** 必须与清单一致，否则整包拒绝（不"应用能应用的那些"）；
* 更新源用 **HTTPS**（`https://aether.cbnac.com/updates/`），并考虑给清单加签名（下一步可选）；
* `auto_apply` 默认 **false**（检查自动、应用需确认）；无人值守场景再显式打开。
### 11.5 范围修正（2026-10-16 读码确认，很重要）

`aetherd/src/update.rs` 的白名单是**只允许**这三处：

```
pub const TARGET_PREFIXES = ["/var/apps/", "/etc/aether/", "/var/lib/aether/"]
// 注释原文：没有白名单的话它就成了「以 root 写任意文件」的工具。
```

**没有 `/usr/bin/`** —— 这是刻意设计：文件级更新只覆盖**应用 / AetherOS 配置 / AetherOS 状态**，
系统组件（内核、合成器、init、aetherd 自身）**不走这条路**。

于是"以后每次更新不麻烦"要分两类做：

| 类型 | 路径 | 现状 |
|---|---|---|
| 应用 / 配置 / 状态 | P1 文件级自动更新（`incoming` → `stage` → 重启应用） | 机制已有，缺"自动拉取"半 |
| **系统本身**（内核 / 组件） | **T3 整机更新**：新 ISO + 无人安装 + **保留 `/var`** | `--keep-var` 未做 ⇒ **优先做这个** |

**优先级的理由**：这个项目改得最多的恰恰是系统组件，而它现在必须整盘重装、丢 `/var`。
P0（无人安装）已通 ⇒ 只差"保留 `/var`"这一个开关，性价比远高于 P1 的拉取半。
按此调整：**P2（`--keep-var`）提前到 P1 之前**。
