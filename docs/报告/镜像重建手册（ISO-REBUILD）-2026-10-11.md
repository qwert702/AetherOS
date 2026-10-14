# 镜像重建手册（ISO-REBUILD） · 2026-10-11

> **这份手册回答一个问题**：`platform/build-iso.sh` 里 `isohybrid` 与 MBR 自检**代码早就补好了**
> （台账 #33），为什么装机还是不可用？—— 因为**没有重新构建过**。
> 本手册就是"从零重建一版可装机 ISO、并把它发出去"的完整流程。
>
> **口径**：代码补了 **≠** 修好了。只有重建出产物、装机验过、且发布事实同步之后，
> 台账 #33 才能销账。本文每一步都写了"怎么算过"。

---

## 0. 前置条件（缺一不可）

| 需要 | 说明 | 谁来提供 |
|---|---|---|
| Linux 构建机 | Ubuntu 24.04（`scripts/setup-vm.ps1` 可一键建 VirtualBox 无人值守 VM：4 GB / 2 CPU / 40 GB） | 本机可建 |
| 构建机凭据 | 无人值守安装与后续 SSH 用**同一个**密码，走 `$env:AETHER_VM_PASSWORD`，**不硬编码** | 你设置环境变量 |
| 联网 | 下载 Buildroot 2024.02.1 与工具链（首次 30–60 分钟） | 构建机 |
| 磁盘空间 | VM 内 ≥ 20 GB 可用（Buildroot 输出很大） | — |
| 发布凭据 | 上传 GitHub Release 的权限（tag `v0.1.0` 或其后续版本） | 你 |

> **一处需要你澄清的事实**：台账 #33 写着"阻塞于构建机凭据（`秘钥.md` 为 0 字节）"。
> 2026-10-11 复核：**仓库里没有任何脚本引用 `秘钥.md`**，在 `…\系统\` 全树也**没找到该文件**。
> 所以这本手册只能把它写成"来自环境变量或你指定的路径" —— 请确认凭据现在的实际存放位置，
> 否则这一步无法自动化（也不该硬编码进脚本）。

---

## 1. 建构建机（Windows 宿主）

```powershell
# 管理员 PowerShell；密码只进环境变量，不进脚本、不进历史
$env:AETHER_VM_PASSWORD = "<构建机登录密码>"
powershell -ExecutionPolicy Bypass -File scripts/setup-vm.ps1
```

**怎么算过**：`VBoxManage list vms` 里出现 `AetherOS-Build`；VM 能 SSH 上去
（`scripts/vm.py sh "uname -a"` 有输出）。

---

## 2. 构建 ISO（在构建机内）

```bash
# 宿主机 → 构建机：跑构建，日志落 ~/iso-build.log（增量约 4 分钟，首次 30–60 分钟）
python scripts/vm.py sh "bash scripts/vm-build-iso.sh"

# 观察进度（不要挂在 SSH 通道上等）
python scripts/vm.py sh "tail -30 /home/aether/iso-build.log"
```

**为什么要有 `vm-build-iso.sh` 这一层**：`platform/build-iso.sh` 里用的是裸 `cargo`/`rustup`，
而 SSH **非交互式会话的 PATH 不含 `~/.cargo/bin`** —— 直接调会得到
`cargo: command not found`，看起来像"构建脚本坏了"。这一层显式补 PATH。

**构建脚本内部的三步**（`platform/build-iso.sh` 头注释为准）：

1. musl 静态交叉编译自研组件（7 个 crate 里的 6 个可编，`aetherd` 需 musl-gcc）；
2. 连同中文屏显字体（Noto Sans CJK SC 子集）拷进 rootfs overlay；
3. 下载 Buildroot → 应用 `aetheros_defconfig` → 出 ISO → **isohybrid + MBR 自检**。

---

## 3. 构建后自检（**这一步才是 #33 的关键**）

```bash
# ③-1 isohybrid 是否真的生效：首 512 字节末尾必须是 0x55 0xAA
python scripts/vm.py sh "xxd -l 512 /home/aether/Aether/aetheros-0.1-amd64.iso | tail -2"

# ③-2 体积（release.json 里声明的 iso_size 必须与**上传的资产**一致，不是与本地文件一致）
python scripts/vm.py sh "ls -l /home/aether/Aether/aetheros-0.1-amd64.iso"

# ③-3 自研组件是否都在镜像里（本机也能跑，它查的是仓库与构建脚本的一致性）
python scripts/iso-components-check.py

# ③-4 取回产物
python scripts/vm-pull.py aetheros-0.1-amd64.iso
```

**怎么算过**：① 末两字节是 `55 aa`；② 体积与 `site/release.json` 的 `iso_size` 对得上
（**改版时两者都要更新**：release.json 记录的是"用户实际下载到的那个文件"）；
③ 组件门禁 [OK]。

> `build-iso.sh` 自身的判据：`isohybrid` 之后仍无 MBR 签名 → **直接 `exit 1`**，
> 且打印实得签名。所以构建脚本返回 0 就说明签名自检过了（[build-iso.sh:121-142](../../platform/build-iso.sh)）。

---

## 4. 装机验证（QEMU 先过，再上真机）

```bash
# ④-1 起 ISO（后台；内存建议 1024 —— 512 MB 下 aether-ops 会每轮报"内存紧张"淹没日志）
MEM=1024 bash scripts/qemu-verify.sh

# ④-2 可选：QMP 键鼠注入 + 截图（也是 e2e 的真实路径）
python scripts/qemu-shot.py screen.ppm > docs/证据/装机-桌面.png

# ④-3 走安装器（登录 guest → 跑安装器 → 查串口）
bash scripts/m6-run-install.sh

# ④-4 从**已安装的磁盘**引导（无光驱），验持久化
bash scripts/qemu-disk-boot.sh
#     持久化分区内容（先把镜像 loop 挂到 /mnt/pp）
bash scripts/inspect-persist.sh
```

**怎么算过**：④-3 串口里出现安装完成、目标盘出现分区；
④-4 能从磁盘独立引导进桌面，且 `/var` 的写入在重启后仍在（`echo x > /var/tmp/probe` → 重启 → 还在）。

**真机（物理机）**：按 `实机验收手册（RUNBOOK）-2026-10-11.md` §1.1 的 `dd` 流程写 U 盘，
走 RUNBOOK 的 #1 / #23 / #25 三项（启动、装盘后重启、断电）。

---

## 5. 发布（**顺序不能反**）

`site/release.json` 的 `_how_to_update` 写明了顺序，照抄如下：

1. 重建 ISO（第 2 节）；
2. **上传 Release 资产**（tag 沿用 `v0.1.0` 或递增）；
3. 用 `sha256sum` / `Get-FileHash` 算出**该资产**的大小与哈希；
4. 更新 `site/release.json`（`iso_bytes` / `iso_size` / `iso_sha256` / `iso_date`）；
5. `python scripts/gen-site.py --refresh`（站点是生成物，**不要手改 HTML**）；
6. `python scripts/gen-site.py --check` → 站点门禁必须 [OK]。

> **一个踩过的坑**：release 上的资产（40,327,168 字节）与本地产物（40,359,936 字节）
> 曾经不是同一个文件，于是下载页公布的校验和根本对不上 —— 所以第 3 步算的必须是
> **上传后的那个资产**，不是本地文件。

**预发布标记**：当前 Release 是 prerelease。GitHub 的 `/releases/latest` **只返回非预发布**，
所以站内与 README 的链接必须是 `/releases/tag/<tag>`（这条由站点门禁与文档数字判据一起守着）。

---

## 6. 失败模式与回滚

> **2026-10-11 实测补的一节：这次"照着手册做"第一次就卡住了三处**，都写在这里，
> 免得下一个人再花一小时重新发现。

### 6.1 环境层（先做这三步，否则根本连不上构建机）

| 现象 | 根因（实测确认） | 处置 |
|---|---|---|
| `VBoxManage startvm` 报 `uApicMode: saved=3 config=2` | 保存状态是旧版 VirtualBox 写的，**无法恢复** | `VBoxManage discardstate <vm>` 后重新开机（磁盘上的 Buildroot 树不受影响） |
| SSH 永远连不上 2222 | **Windows 保留了 TCP 2180–2279**（`netsh int ipv4 show excludedportrange protocol=tcp`），2222 落在里面 → VirtualBox 报 `VERR_NAT_REDIR_SETUP` | 加一条转发 `ssh22022,tcp,,22022,,22`，用 `AETHER_VM_PORT=22022`；**不要**去动系统保留段 |
| `[vm.py] … not found in known_hosts` | 严格主机密钥校验按 `host:port` 记账，换端口就是新条目 | 把原 2222 那条按端口复制一份（**不要**直接删记录走 TOFU：那是"静默信任新密钥"） |
| 密码丢了（`秘钥.md` 是 0 字节，VM 里只有口令哈希） | 无人值守安装只留下 SHA-512 哈希，不可逆 | **本地控制台恢复**：GRUB 菜单按 `e` → `linux` 行尾加 `rw init=/bin/bash` → `Ctrl+X` → root shell → 注入自己的公钥（`authorized_keys`）。headless VM 也能做：`VBoxManage controlvm <vm> screenshotpng` 看画面、`keyboardputscancode` 注入按键 |

> **为什么选"注入公钥"而不是"改口令"**：不动原有凭据、可直接撤销
> （删掉 `/home/aether/.ssh/authorized_keys` 里那一行即可），而且
> `AETHER_VM_KEY` 本来就是 `scripts/vm.py` **文档化的另一种认证方式**。

### 6.2 代码层（这次真的拦住了构建，两个缺陷同一根因）

| 现象 | 根因 | 处置 |
|---|---|---|
| `[1/3]` 里 `aetherd` 报 `cannot find module or crate libc` | `timesync.rs` 在 `#[cfg(unix)]` 里调 `libc::…`，但 `aetherd/Cargo.toml` **没声明 libc**。Windows 上这段不参与编译，`musl-warn-check.py` 又特意跳过 `aetherd`（ring 需 musl-gcc）→ 两边同时漏掉 | 补 `[target.'cfg(unix)'.dependencies] libc = "0.2"`；并新增**纯静态**判据 `scripts/crate-dep-check.py`（进 `gates.py`） |
| 修完上一条后报 `E0004: non-exhaustive patterns: Adjustment::Refused not covered` | 同一个 `#[cfg(unix)]` 平台函数里的 `match how` 少了分支 —— 同样因为"这段代码从没在 Linux 上编过" | 补齐 `Refused` 分支（语义 = **不动时钟**，不是伪造成成功/失败） |
| musl 下 `libc::time_t` **deprecated** 警告（项目要求 0 告警） | libc 该别名在 musl 1.2 起改 64 位，上游要求别直接用 | 写成 `as _`，让字段自身类型说话（写死 `i64` 会把 32 位平台写死） |

> **这一节的教训（比三条修复本身重要）**：`#[cfg(unix)]` / `#[cfg(target_os = "…")]`
> 这类平台分支，**必须在那个平台上真正编译一次**才算存在。本次事故里，
> 从 2026-10-03（加 SNTP 对时）到 10-11，**ISO 一直是构建不出来的**，
> 而所有本地判据都是绿的 —— 台账 #33「ISO 未重构建」的真正代价就在这里。
> 所以：**每次发版前必须真的跑一次 `[1/3]`**，它是 `aetherd` 唯一的 Linux 编译验证。

### 6.3 其它失败模式

| 现象 | 原因 | 处置 |
|---|---|---|
| `cargo: command not found` | SSH 非交互 PATH 缺 `~/.cargo/bin` | 走 `scripts/vm-build-iso.sh`（已补 PATH） |
| 构建到一半断 | 首次构建 30–60 分钟，SSH 通道易断 | 用 `vm.py sh` 起后台 + 轮询日志；`vm-resume-build.sh` 续 |
| `vm.py sh` 卡住不返回 | 后台进程继承了 SSH 通道（stdout/stdin 没断开） | 命令写成 `nohup … >/dev/null 2>&1 &`，并**单独**用另一次 `vm.py sh "tail …"` 看进度 |
| 下载很慢（实测 ~84 KB/s） | **构建机访问不了 GitHub**（见 `vm-sync.py` 头注释），Buildroot 包只能走公共镜像 | 耐心等；`platform/build/buildroot-2024.02.1/dl` 缓存下来的包不会重复下 |
| `isohybrid` 报找不到 | 缺 syslinux 主机工具 | `vm-setup-deps.sh`（构建机内）补依赖 |
| 自检发现无 MBR 签名 | isohybrid 未生效 | **不要发布**：按 `build-iso.sh` 输出定位（它会 exit 1） |
| 发布后下载页校验和对不上 | 第 3 步算错文件 | 重算资产哈希 → 更新 release.json → `gen-site.py --refresh` |
| 装完起不来 | 分区/引导问题 | 保留串口日志，按 RUNBOOK #23/#24 记录后回滚到上一版 ISO |

**回滚**：ISO 是独立产物，回滚 = 把 `site/release.json` 指回上一版资产并 `--refresh`；
**不需要动代码**。代码侧的改动按批次 `git revert`。

---

## 7. 与其它文档的关系

| 文档 | 管什么 |
|---|---|
| 本手册 | 重建 ISO + 装机 + 发布 |
| `实机验收手册（RUNBOOK）-2026-10-11.md` | 真机逐项验收（含存疑项的记录格式） |
| `platform/README.md` | 构建目录结构与 defconfig 说明 |
| `应用包格式（APP-PACKAGES）-2026-09-28.md` | 应用包（`.aep`）怎么打进系统 |
| `功能缺口清单（FEATURE-GAPS）-2026-10-02.md` 台账 #33 | 本手册要销的那笔账 |
