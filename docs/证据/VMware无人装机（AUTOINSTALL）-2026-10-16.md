# VMware 无人装机（P0）—— 2026-10-16

> **目标**：在 VMware 里把 AetherOS 装成**真正的系统**（装到虚拟磁盘、从盘独立引导、`/var` 持久化），
> 并且**全程无人值守** —— 因为 VMware 的 CLI **没有按键注入**（`vmrun runProgramInGuest` 需要 VMware Tools，
> AetherOS 没有），任何"进 guest 敲命令"的方案都不成立。
>
> **结论：P0 已完成** —— 挂上"自动安装版 ISO"开机即自动装盘、自动关机，再从磁盘独立引导进桌面。

## 一、产物与现场

| 项 | 值 |
|---|---|
| 虚拟机 | `E:\aether-vm\AetherOS\`（VMware Workstation 17，`vmware.exe -x` 启动） |
| 配置 | 2 vCPU · 2 GB 内存 · 4 GB 稀疏盘（`disk.vmdk`）· SATA 光驱 · NAT · **串口落盘** |
| 自动安装版 ISO | `aetheros-autoinstall.iso`（42,991,616 字节 · `sha256=8fd7860484cf…`）<br>构建方式：`AETHER_AUTOINSTALL=1 bash scripts/build-iso.sh`（构建机内） |
| 装机判据 | 串口出现 `AETHER-AUTOINSTALL: PASS` ✓ · `disk.vmdk` **0.6 MB → 44.6 MB** ✓ · VM **自动关机** ✓ |
| 独立引导判据 | 把 `bios.bootOrder` 改成 `disk`、光驱 `startConnected=FALSE`（**拔掉光驱**）后启动 → **进了 AetherOS 桌面** ✓ |

## 二、串口证据（原文摘录）

```
Command line: BOOT_IMAGE=/boot/bzImage console=tty0 console=ttyS0,115200n8 aether.autoinstall=1
...
[    2.394259] sd 0:0:0:0: [sda] 8388608 512-byte logical blocks: (4.29 GB/4.00 GiB)
[    2.411799] sd 0:0:0:0: [sda] Attached SCSI disk
[aether-init] 自动安装开始：/usr/bin/aether-install --disk /dev/sda --yes
aether-install: 目标 /dev/sda（4096MB），源 /dev/sr0
[    4.051080]  sda: sda1 sda2
aether-install: 提示 —— 镜像内没有 wipefs，跳过旧签名清理
[    4.300778] EXT4-fs (sda2): mounted filesystem df33d668-070b-491d-8a70-fd16724296c0 r/w with ordered data mode
[    4.315014] EXT4-fs (sda2): unmounting filesystem df33d668-070b-491d-8a70-fd16724296c0
aether-install: 安装完成。请重启并移除引导介质，系统将从 /dev/sda 引导。
[aether-init] 自动安装完成（/dev/sda）
AETHER-AUTOINSTALL: PASS
[aether-init] 关机 —— 等宿主把引导顺序改成磁盘后再开机
```

## 三、判据的设计与一次真失败（值得记下来）

触发条件**必须同时**满足两条：

1. 内核命令行有 `aether.autoinstall=1`（只有引导菜单的"自动安装"项带）；
2. **本机还没有 AetherOS 的安装**（`persist::any_existing_install()`：遍历持久分区候选，走既有的
   安装标记探测 —— 只读挂载 + 卷标核对 + **磁盘 MBR 签名绑定**）。

只看第 1 条会出**毁盘级**事故：系统分区是 ISO 的拷贝、引导配置一起进了磁盘 ⇒ 从盘引导时命令行
**照样带**这个参数 ⇒ 把刚装好的系统**再装一遍**。

**第一版判据写错了**（"根是只读 + iso9660/squashfs"）：AetherOS 是 initramfs 引导，根报 `rootfs`（rw），
判据**永远不成立**且**静默**（串口里连一行输出都没有）。幸好降级路径是"什么都不做、按普通 live 启动"，
所以没有乱写磁盘 —— 换成本项目自己的**安装标记**之后一次通过。

> 教训一句话：**判据要挑"这件事本身的证据"，不要挑它的间接迹象。**

## 四、环境坑（这台机器上实测）

* **`vmrun start` 拉不起 VM**（日志 `Vix: Postpone the command. VM not running`）→ 用
  **`vmware.exe -x <vmx>`** 启动 ✓（窗口开在桌面，配置照读）。
* **串口落盘不稳定**：ISO 引导那次 35 分钟后才出现文件；从盘引导这次**始终没有** ——
  所以判据不能只靠串口，必须叠加"磁盘文件长大""结果标记"以及**人眼确认**（本次即如此）。
* 改引导顺序/换光驱前**必须先关机**（VMware 关机时会改写 `.vmx`）；换完再启。

## 五、还没做

* **P1 自动更新**：`aetherd` 定时拉更新源 → 校验 → 重启生效（全程不碰 guest）—— 方案见
  [`VMware装机与自动更新（VMWARE-OTA）-2026-10-15.md`](../报告/VMware装机与自动更新（VMWARE-OTA）-2026-10-15.md)。
* **`--keep-var` 整机更新**（P2）：换内核/系统级改动时保留 `/var` 的路径。
* 镜像里**没有 `wipefs`**（安装器已显式提示并跳过旧签名清理）—— 装机路径下不影响，但值得补进镜像。
