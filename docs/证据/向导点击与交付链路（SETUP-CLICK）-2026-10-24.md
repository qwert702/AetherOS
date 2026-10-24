# 首启向导点击修复 · VMware 清单清理 · 交付链路复盘（SETUP-CLICK）· 2026-10-24

> **一句话**：本次做了两件事 —— ①**修好"首启向导里点中文/English 没反应"**（代码已提交 `ba3b36f` ✓）；
> ②**把 VMware 清单里的 5 个 Aether 条目清理成 1 个**（保留 `AetherOS` ✓）。
> 第三件事（把修好的系统装进 VMware）**没做完** ✗，失败点全部记录在第四节，供后续接手。
>
> 相关文档：[鼠标绝对坐标（EV_ABS）](鼠标绝对坐标（EV_ABS）-2026-10-17.md)（上一轮"鼠标不动"的根因与修复）、
> [T3 验证：keepvar 保住 var](T3验证-keepvar保住var-2026-10-17.md)、
> [装机与更新操作手册](../报告/装机与更新操作手册（OPS-MANUAL）-2026-10-17.md)。

---

## 一、问题一：向导里点「中文 / English」没有反应

### 现象
首启向导第一步是选界面语言，屏幕上有两个**看得见的按钮**（中文 / English），
但鼠标点上去**毫无反应**；与此同时**光标本身是能移动的**（上一轮修好后 ✓）。

### 根因（代码级，已核实）
1. 点击处理**本来就有**：`aether-compositor/src/main.rs` 的"**按下-抬起 = 一次点击**"路径里，
   会 `renderer.setup_hit(mouse.0, mouse.1)` → `setup::options(step)` → `apply_setup_action(...)`，
   并且**明确检查了 `renderer.setup.open`** —— 作者本意就是让它处理向导。
2. 但为了"键盘优先级"，后来加了 `KeyRoute::Setup` 守卫分支（在事件分发的最前面），
   该分支对**每个事件**都 `continue` —— 而它只翻译 `Char` / `Enter` / `Escape`，
   鼠标事件落进 `_ => setup::Action::None` 后被吞掉。
3. ⇒ **向导打开期间，那段点击代码永远执行不到** ✗。这就是"光标能动、按钮点不动"的全部原因。

> 这条也解释了**为什么键盘能绕过**：向导的键盘路径（`setup::handle_key`）一直是通的。

### 修复（提交 `ba3b36f`）
| 文件 | 改动 |
|---|---|
| `aether-compositor/src/setup.rs` | 新增纯函数 `action_at(step, idx) -> Action`：与 `options()` **同一张表**取动作，越界/未知步骤一律 `Action::None`（**不 panic**）—— 保证"点按钮"与"按数字键"同源、不会分叉 |
| `aether-compositor/src/main.rs` | `KeyRoute::Setup` 分支新增 **`MouseUp`** 分支：`renderer.setup_hit(mouse.0, mouse.1).map_or(Action::None, |idx| setup::action_at(step, idx))` |

**为什么用 `MouseUp` 而不是 `MouseDown`**：与下文"按下-抬起 = 一次点击（拖出去不算点击）"的既有约定一致，
不引入第二套点击语义。

**测试**（`setup::tests::action_at_maps_language_buttons_and_never_panics`）：
- 正向：第 1 步两个语言按钮 → `SetLang(0)` / `SetLang(1)`；第 4 步"开始使用" → `Next`；
- 负向：越界下标 `action_at(0, 2)`、未知步骤 `action_at(99, 0)` → `None`（且**不得 panic**）。

**验证**：`cargo build --workspace --all-targets --locked` **0 错误 0 告警**；
`cargo test -p aether-compositor` **全过**（含新增 1 条）；
`repo-stats --check` / `doc-numbers-check` **0 FAIL**；`gates.py --fast` **27/27 全过**。

### 临时绕过（无需部署即可用）
在向导里按 **`1`**（中文）/ **`2`**（English）/ **`Esc`**（跳过全部）—— 键盘路径一直有效；
走完后 `setup_done` 落盘，向导不再弹。

---

## 二、问题二：VMware 清单里的 5 个 Aether，只保留 1 个

### 清单结构（`%APPDATA%\VMware\inventory.vmls`）
每个虚拟机由**两块**配置描述：`vmlistN.*`（条目本体）与 `indexM.*`（guest OS 信息缓存）；
只删其中一块会留下"半条记录"，因此两块都要删。

### 处理结果
| 条目 | 配置路径 | 处置 |
|---|---|---|
| `vmlist2` "Aether OS" | `D:\CBN-HT\Desktop\Aether-VM\aether.vmx` | 删除 ✓（文件早就不存在，`State=broken`）|
| `vmlist7` "AetherOS-VMware" | `D:\aether-vm\AetherOS-VMware\AetherOS.vmx` | 删除 ✓（目录此前已删）|
| `vmlist8` "AetherOS-1011" | `D:\aether-vm\AetherOS-1011\AetherOS-1011.vmx` | 删除 ✓（目录不存在）|
| `vmlist10` "AetherOS2" | `E:\aether-vm\AetherOS2\AetherOS2.vmx` | 删除 ✓（并删掉磁盘目录，0.7 MB）|
| **`vmlist9` "AetherOS"** | `E:\aether-vm\AetherOS\AetherOS.vmx` | **保留** ✓ |

同时删掉对应的 `index0` / `index5` / `index6` / `index8` 缓存块，并把 `index.count` 从 `10` 改成 `6`。
**校验**：vmlist 条目 **10 → 6**；含 `Aether`/`aether` 的行只剩 **3 行**（全部指向保留的那台）；
`index.count = "6"`；文件 **179 → 111 行**。

### ⚠️ 两个必须记住的坑
1. **这个文件是 GBK 编码**（首行 `.encoding = "GBK"`，路径里有中文）。用 UTF-8 读写会把**其它虚拟机的中文路径写坏** ✗。
   正确做法：`[System.Text.Encoding]::GetEncoding(936)` 读**与**写，或不碰非目标行。
2. **必须在 VMware 完全关闭时改**：VMware 退出时会**重写**该文件，运行中改会被覆盖 ✗
   （`vmware-authd` 是服务，不写清单，不需要停）。

### 回滚
备份：`C:\Users\cbn\AppData\Roaming\VMware\inventory.vmls.bak-20261022-194856`（7,585 字节）；
另有一份在 `E:\aether-vm\_config-backup-20261018\`（该目录随外置盘走，见第五节）。
被删条目指向的文件本来就不存在，**恢复备份只会让列表里多两个坏条目**，不会丢任何虚拟机。

---

## 三、环境事件：E 盘（Netac 外置盘）掉线又恢复

- 清理清单时发现 **E: 整个盘符消失**，系统里只剩 C:/D: 两块物理盘 ✗（不是"盘符丢了"，是**盘没接上**）。
- 事后确认：E: 是 **Netac DISK 3.2（477 GB 外置盘）**，重新接好后恢复（可用 271.8 GB）✓。
- 影响：`E:\aether-vm\AetherOS`（**保留的那台，也是鼠标已验证可用的那台**）一度不可达；
  外置盘上的备份目录 `E:\aether-vm\_config-backup-20261018\` 同样不可达。
  **清单备份放在 C: 上是对的**（不受外置盘影响）✓。
- 教训：涉及外置盘的批量操作，**先确认盘符存在再动手**；删除类操作要判空，避免"路径不存在时把相对路径删错"。

---

## 四、问题三（**未完成** ✗）：把修好的系统装进 VMware

目标：把带"鼠标修复 + 向导点击修复"的 ISO 装进 `E:\aether-vm\AetherOS`，`/var` 保留。
**过程中连踩三个坑，每一个都有串口证据**，全部记录如下（避免后人重走）：

### 坑 1：光驱根本没挂上 ⇒ 安装从未执行
- 现象：`vmware-keepvar.py --apply` 打印了"①改 vmx ②启动 ③改回 ④再启动"，
  但盘上系统**没变**；串口里**没有** `自动安装开始`。
- 原因：调用 `VBoxManage storageattach`（VBox 侧）与 `vmware` 侧挂载时，
  **把命令的错误输出喂给了 `Out-Null`**，挂载失败被吞掉；虚拟机 10 秒内"无引导介质"直接退出，
  被误当成"装完了"。
- **纪律**：验证脚本**绝不隐藏错误输出**；"装完了"的判据只能是**串口里出现 `自动安装开始`**。

### 坑 2：autoinstall 被自己的安全判据正确拦下
- 现象：用 **autoinstall** 版 ISO 对一块**已装过系统**的盘启动，串口输出：
  `[aether-init] 首次装机跳过：本机已有 AetherOS 安装（安装标记在）—— 绝不重装`
- 这是**设计如此**（防止误重装 ✓），不是 bug。要做"全新安装"必须**从空盘开始**
  （新建一块 `vmware-vdiskmanager` 空白盘 + 独立 vmx）。

### 坑 3：装完后引导配置仍带 `aether.keepvar=1` ⇒ 每次从盘启动都想重装
- 安装本身**成功**（串口原文见附录 A）；
- 但**从磁盘启动**时，串口出现：
  ```
  [aether-init] 自动安装开始（keep_var=true）：… --keep-var
  Error: 打不开安装源 /dev/sr0      ← 光驱已断开、无介质
  [aether-init] 自动安装失败 …
  AETHER-AUTOINSTALL: FAIL → Power down
  ```
- 原因链：
  1. 装好的系统，其**引导配置**来自 ISO 的**只读 iso9660 分区**，里面写着 `aether.keepvar=1`，**改不掉**；
  2. 于是每次从盘启动都会请求一次 keep-var 安装；
  3. 本应拦住它的判据是 `live_medium()` —— 读 `/sys/block/sr0/size`，为 0 或读不到就"不是 live"；
  4. 但在 VMware 下，**光驱设备仍在、只是没放盘**（`sata0:1.startConnected="FALSE"`）时，
     该 `size` 仍报告上次介质的容量 ⇒ 被判成"live" ⇒ 安装被触发 ⇒ 因无介质而失败 ⇒ **关机**。
- 已做的处置：把**整个光驱设备**从 vmx 摘掉（`sata0:1.present = "FALSE"`，已备份 `AetherOS.vmx.bak-nocd-*`）。
  结果：VM 能启动 ✓，但**串口无输出、`disk.vmdk` 不再写入** ✗ —— 客机**没有真正引导**，原因待查 ✗。

### 下一步（二选一，都写清楚了）
- **方案 A（改代码，推荐）**：把 `live_medium()` 从"读 size"改成**真正探测介质**
  （例如打开设备读 ISO9660 卷描述符 / 读 `EVIOCGMEDIA`-类信息），然后重建 ISO 重装。
  这样"从盘启动"与"live 介质"的判据才是可靠的，不依赖设备是否被摘掉。
- **方案 B（改交付流程）**：给"装好的系统"在**安装时**就把引导配置改写成 `DEFAULT aether`
  （不带 `keepvar`/`autoinstall`）—— 但系统分区是只读 iso9660，需要在**分区表层面**另建一个可写的
  引导分区，改动面更大，需单独立项。

> **当前状态**：`E:\aether-vm\AetherOS` 的盘上系统**已经是带修复的新版** ✓（安装串口证明 ✓），
> 卡在"引导配置残留导致每次启动想重装"这一环 ✗。

---

## 五、教训清单（这次踩过的，按可复用性排序）

1. **验证脚本绝不隐藏错误**：`Out-Null` / `-ErrorAction SilentlyContinue` 会把"根本没执行"伪装成"执行成功"。
   本次因它连续误判两轮（坑 1、坑 3）。
2. **"装完了"的判据只能是串口里的那行**：`自动安装开始（keep_var=true）` → `自动安装完成`；
   没有这两行，一律视为**没装**。
3. **截图只有"变化"才有意义**：两张截图逐字节相同 ⇒ 客机卡死；而"差异只在同一小块区域" ⇒ 那是固定闪烁元素，
   **不是光标移动**（上一轮我曾据此误判一次 ✗）。
4. **设备存在 ≠ 有介质**：`/sys/block/sr0/size` 在有设备无介质时可能仍报旧容量。
5. **改 VMware 清单要关 VMware、要按 GBK 读写**；改 vmx 要在**关机**状态（运行中改会被覆盖）。
6. **外置盘先说在不在**：E: 一度整盘消失（Netac 外置盘），涉及 E: 的删除操作全部判空后跳过 ✓，
   所以没造成误删 ✓。
7. **PowerShell 5.1 没有 `??`、没有 `Join-String`**；`$f:` 会被当成驱动器引用；
   双引号 here-string 会吃掉反引号 —— 写脚本时用单引号 here-string 或 `chr(36)` 之类绕开。
8. **向导/锁屏这类"模态分支"必须同时处理鼠标**：`continue` 掉的守卫会连带吞掉指针与按键，
   只有键盘的用户永远出不去（这是上一轮鼠标 bug 的同源问题）。

---

## 六、当前状态（截至 2026-10-24 20:50）

| 项 | 状态 |
|---|---|
| 仓库 | `HEAD = ba3b36f` ✓、工作区干净 ✓、**已全部推送** ✓（远端确认过）|
| 代码质量 | `--all-targets` **0 错误 0 告警** ✓；合成器测试全过 ✓；快速门禁 **27/27** ✓；`repo-stats`/`doc-numbers` **0 FAIL** ✓ |
| VMware 清单 | 仅剩 **1 个** Aether 条目（`AetherOS` ✓），备份在 C: ✓ |
| `E:\aether-vm\AetherOS` | 盘上是**带修复的新版** ✓（安装串口为证），但**引导配置残留 `keepvar`** ✗ → 从盘启动会尝试重装并失败 ✗ |
| ISO（带鼠标 + 点击修复） | 构建 `RESULT=PASS` ✓，sha256 前 16 位 `91fa7060a30c76f4` ✓（构建机与本地一致 ✓）|
| VirtualBox `AetherOS-T3` | **鼠标可用的系统** ✓（用户实测 ✓），配置在 C:、磁盘 `D:\aether-vm\T3-disk.vdi` ✓；当前 `poweroff` ✓ |
| VirtualBox `AetherOS-Build` | 构建机，当前 `aborted` ✗（需 `controlvm poweroff` 再启动；若卡 GRUB 发一次回车 ✓）|
| 磁盘空间 | **C: 仅 0.2 GB** ✗⚠️（反复出现的风险：会引发各种"诡异失败"）；D: 187 GB ✓；E: 272 GB ✓ |

---

## 附录 A：安装成功（keepvar 保留 /var）的串口原文

```
[    0.000000] Command line: … aether.keepvar=1
[    2.289033] sr 1:0:0:0: [sr0] scsi3-mmc drive: 1x/1x writer dvd-ram cd/rw xa/form2 cdda tray
[aether-init] /dev/sda2 身份绑定通过（磁盘签名 3D8DD522）
[aether-init] 自动安装开始（keep_var=true）：/usr/bin/aether-install --disk /dev/sda --yes --keep-var
aether-install: 目标 /dev/sda（4096MB），源 /dev/sr0
aether-install: --keep-var：沿用原分区项（不重新规划），/var 原地保留
aether-install: 持久化分区 2 起始扇区 83968 大小 4055MB
aether-install: --keep-var：跳过 wipefs/mkfs —— 持久分区原样保留
aether-install: 持久化分区 /dev/sda2
[aether-init] 自动安装完成（/dev/sda，keep_var=true）
AETHER-KEEPVAR: PASS
```

## 附录 B：从盘启动失败（坑 3）的串口原文

```
[aether-init] /dev/sda2 身份绑定通过（磁盘签名 3D8DD522）
[aether-init] 自动安装开始（keep_var=true）：/usr/bin/aether-install --disk /dev/sda --yes --keep-var
aether-install: 目标 /dev/sda（4096MB），源 /dev/sr0
Error: 打不开安装源 /dev/sr0

Caused by:
    No medium found (os error 123)
[aether-init] 自动安装失败：Ok(ExitStatus(unix_wait_status(256)))
AETHER-AUTOINSTALL: FAIL
[    2.951603] sd 0:0:0:0: [sda] Stopping disk
[    3.809270] reboot: Power down
```
