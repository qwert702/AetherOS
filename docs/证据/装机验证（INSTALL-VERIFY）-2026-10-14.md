# 装机验证（台账 #33 后半段）—— 2026-10-14

> **这笔账此前一直挂着**：ISO 重建好了、isohybrid 也验证了，但**没人真的装过一次机**。
> 本文是第一次完整的装机验证：起 ISO → 跑安装器 → 从盘引导 → 查持久化。
> **结论：四项判据全部通过。**

## 一、在哪做的（以及为什么不是构建机）

**平台：Windows 宿主上的 VirtualBox（headless）**，不是构建机内的 QEMU。

原因如实记录：构建机（`AetherOS-Build`）在跑 QEMU 时进入**三层嵌套虚拟化**
（Hyper-V/WHPX → VirtualBox guest → QEMU guest），guest 里的 sshd 会被饿到**无法完成认证**
（paramiko 与 OpenSSH 客户端都超时，`Connection timed out during banner exchange`）；
即使把构建机从 2 vCPU 加到 4 vCPU 也没救。于是改用宿主上的 VirtualBox：

* 有**可用的按键注入**（`keyboardputscancode`/`keyboardputstring`，本会话已多次验证）；
* 有**串口落盘**（`--uartmode1 file`）→ 全部观测都是**文本**，不需要看屏幕；
* 启动只要 **10 秒**（对比构建机内三层嵌套的分钟级）。

复现命令（宿主侧，全部可复制执行）：

```powershell
$vb = 'C:\Program Files\Oracle\VirtualBox\VBoxManage.exe'
& $vb createvm --name AetherOS-Install-Test --ostype Linux26_64 --register --basefolder D:\aether-vm\install-test
& $vb modifyvm AetherOS-Install-Test --memory 1024 --vram 32 --graphicscontroller vboxvga `
    --firmware bios --boot1 dvd --boot2 disk --nic1 nat
& $vb modifyvm AetherOS-Install-Test --uart1 0x3F8 4 --uartmode1 file D:\aether-vm\install-test\serial.txt
& $vb createmedium disk --filename D:\aether-vm\install-test\target.vdi --size 1024 --format VDI
& $vb storagectl AetherOS-Install-Test --name IDE --add ide --controller PIIX4
& $vb storageattach AetherOS-Install-Test --storagectl IDE --port 0 --device 0 --type dvddrive --medium <新 ISO>
& $vb storageattach AetherOS-Install-Test --storagectl IDE --port 1 --device 0 --type hdd --medium D:\aether-vm\install-test\target.vdi
& $vb startvm AetherOS-Install-Test --type headless
```

被测镜像：`aetheros-0.1-amd64.iso` · `42,991,616` 字节 · `sha256=162b19a7…`（2026-10-11 重建、isohybrid 已验证的那份）。

## 二、四项判据与实测证据

### ④-1 起 ISO → 系统起来

串口（`serial.txt`）：

```
aether-compositor: fbdev 1024x768 @32bpp stride=4096 字节序=Bgrx …
aether-ops: 巡检#1 up=2s mem=87/970MB 服务=aetherd=✓ compositor=✓ getty=✓ network=✓ ops=✓
aether-compositor: evdev 输入已接入（5 个设备）
aether-compositor: 已渲染 1 帧
```
✅ **通过**（五个服务全 ✓，合成器在 fbdev 上出帧）

### ④-2 走安装器

在 guest 控制台（tty1）以 root 登录（**空口令**，对应审计 M-4）后执行：

```
aether-install --disk /dev/sda --yes
```

串口里出现**内核侧的分区与格式化证据**：

```
[   30.926374]  sda: sda1 sda2
[   31.175618] EXT4-fs (sda2): mounted filesystem 2cf8c2c5-ff9d-4644-a537-c9304edc8eae r/w with ordered data mode
[   31.203280] EXT4-fs (sda2): unmounting filesystem 2cf8c2c5-ff9d-4644-a537-c9304edc8eae
```
✅ **通过**（目标盘出现 sda1/sda2；持久化分区被建好 ext4 并正确卸载）
（`--disk /dev/sda` 而不是 `/dev/vda`：VirtualBox 走 IDE，QEMU 那份脚本用 virtio ✓）

### ④-3 从**已安装的磁盘**引导（光驱已拔）

```powershell
& $vb modifyvm AetherOS-Install-Test --boot1 disk --boot2 none
& $vb storageattach AetherOS-Install-Test --storagectl IDE --port 0 --device 0 --type dvddrive --medium none
```

串口：

```
[aether-init] 持久化分区已挂载: /dev/sda2 -> /var（nodev,nosuid,errors=remount-ro）
[aether-init] 自启动序列: ["network", "aetherd", "compositor", "getty", "ops"]
aether-ops: 巡检#1 up=2s mem=91/970MB 服务=aetherd=✓ compositor=✓ getty=✓ network=✓ ops=✓
```
✅ **通过**（**没有光驱**也能独立引导，且挂上了磁盘上的持久化分区）

### ④-4 持久化：重启后 `/var` 的写入仍在

两步，都是**从磁盘镜像里读出来的硬证据**（不依赖看屏幕）：

1. 在磁盘引导的那次会话里写入探针：`echo probe-1014 > /var/tmp/probe`
2. 复位重启（`VBoxManage controlvm … reset`）后再看磁盘（`clonemedium … --format RAW` 导出后搜字节）：

```
probe-1014            出现 2 次  偏移 [181413520, 181424128]
启动记录（“启动（第”）出现 2 次
   1791986102 启动（第 1 次）
   1791986300 启动（第 2 次）
```
✅ **通过** ——
* 重启前写的 `probe-1014` **重启后仍在盘上**；
* `/var/boot.log` 里两条记录**序号递增**（第 1 次 → 第 2 次），正是 `aether-init/src/persist.rs:459`
  设计的"每次成功挂载追加一条启动记录"的持久化自证。

## 三、顺带确认的两件事（不是本次目标，但都是真实现象）

1. **mono 字体确实缺**（台账 #46 现场复现）：串口里有
   `aether-compositor: 无可用字体，尝试过 ["…/NotoSansMonoCJKsc-Regular.otf", "…/NotoSansMono-Regular.ttf", …]`
   —— 构建侧子集化那一步还没做。
2. **注入的按键会被两个消费者同时看到**：合成器（evdev 直读，5 个设备）与 tty1 的 getty
   共用同一个键盘。表现是：我敲的安装命令**既进了 AI 指令条**（串口里能看到
   `AI 查询「aether-install …」`），**也进了 tty1 的 root shell**（所以安装真的跑了）。
   → 以后写自动化时要知道：**AI 栏会"吃掉"一部分输入**，靠"看 AI 栏有没有收到"判断按键是否送达是不可靠的。

## 四、本次**没有**覆盖的（别把它当成"全绿"）

* **真机（物理机）**：仍是台账 #32 —— U 盘 dd 启动、装盘、断电、ALSA 出声、真实键鼠，都要真机。
* **发布**：新 ISO 尚未上传 Release、`site/release.json` 仍指旧资产（见手册 §5）。
* 安装器的**交互式**路径（不带 `--yes`）、擦盘开关 `--wipe` 的破坏性路径：本次都没走。
