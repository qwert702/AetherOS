# T3 验证：`--keep-var` 换系统 —— **结论更正：FAIL** · 2026-10-17

> ## ⚠️ 更正（2026-10-17 晚，第三轮实验后）
>
> **本文先前的结论「PASS」是错的，现已推翻。** 错的不是数据，是"通过"的标准：
>
> * `--keep-var` **确实保住了数据**：换系统后 `/var` 的 ext4 UUID 逐字节不变（见第二节）✅；
> * **但装完之后系统认不出那块分区** ❌ —— 从磁盘独立引导时串口是
>   `[aether-init] 未发现持久化分区（内存态运行，重启后状态不保留）`，
>   系统跑在内存里。对用户来说，这跟丢数据**没有区别**。
>
> **根因（已定位，证据在第四节）**：`--keep-var` 把持久分区项写进了 MBR 的 **slot 0**，
> **顶掉了系统分区那一项** ⇒ 内核只看到 `/dev/sda1`（持久分区），
> 而 `aether-init` 的候选设备写死是 `/dev/sda2` ⇒ 永远认不出 ⇒ 只能内存态运行。
> 方向：`capture_persist()` 返回的分区索引与 `set_partition_entry()` 的索引约定不一致（差一），
> 需要写一个断言它俩一致的单元测试。
>
> **修完必须重跑本实验**（第二、三、四节全部重做）；在重跑通过之前，T3 **不算完成**。

> **结论：PASS ✓** —— 换系统后 `/var` 的 ext4 UUID **逐字节不变**，且 mkfs/wipefs **确实被跳过**。
>
> **在哪验的**：VirtualBox 里新建的 `AetherOS-T3`（4 GB VDI + 自动安装版 ISO 装第一遍，
> 再用 keepvar 版 ISO 装第二遍）。**为什么不在 VMware 里验**：那台 VM 需要人关机才能换盘，
> 而 `--keep-var` 的判据（分区项复用、磁盘签名还原、跳过 mkfs）**与虚拟化平台无关** ——
> 在 VBox 里成立即在 VMware 里成立；VMware 那侧只剩"换盘"这一步，由
> `scripts/vmware-keepvar.py` 覆盖（干跑已验证它只改那 3 行）。

## 一、实验步骤

```
① 建机：VBoxManage createvm AetherOS-T3（4 GB VDI，串口 → t3-serial.txt）
② 第一遍：挂 aetheros-autoinstall.iso → 开机 → 自动装盘 → 自己关机（30 秒）
   串口 → AETHER-AUTOINSTALL: PASS
③ 存基线：把串口存档为 t3-base-serial.txt（记下 /var 的 ext4 UUID）
④ 第二遍：换挂 aetheros-keepvar.iso → 开机 → 自动"换系统、保留 /var" → 自己关机（30 秒）
⑤ 对比两次串口里的 ext4 UUID
```

## 二、证据（串口原文，两次对比）

**① 第一次装机（基线）**

```
Command line: BOOT_IMAGE=/boot/bzImage console=tty0 console=ttyS0,115200n8 aether.autoinstall=1
 sda: sda1 sda2
mkfs.ext4 (159) used greatest stack depth: 13400 bytes left
EXT4-fs (sda2): mounted filesystem 392177a2-9c0e-452a-b629-24b3ad91eec8 r/w with ordered data mode
EXT4-fs (sda2): unmounting filesystem 392177a2-9c0e-452a-b629-24b3ad91eec8
[aether-init] 自动安装完成（/dev/sda）
AETHER-AUTOINSTALL: PASS
```

**② 第二次：换系统（`--keep-var`）**

```
Command line: BOOT_IMAGE=/boot/bzImage console=tty0 console=ttyS0,115200n8 aether.keepvar=1
EXT4-fs (sda2): orphan cleanup on readonly fs
EXT4-fs (sda2): mounted filesystem 392177a2-9c0e-452a-b629-24b3ad91eec8 ro with ordered data mode
EXT4-fs (sda2): unmounting filesystem 392177a2-9c0e-452a-b629-24b3ad91eec8
aether-install: --keep-var：沿用原分区项（不重新规划），/var 原地保留
aether-install: --keep-var：跳过 wipefs/mkfs —— 持久分区原样保留
[aether-init] 自动安装完成（/dev/sda，keep_var=true）
```

## 三、三条判据逐条对账

| 判据 | 证据 | 结论 |
|---|---|---|
| **`/var` 的 ext4 UUID 不变** | 两次都是 `392177a2-9c0e-452a-b629-24b3ad91eec8` | ✅ **通过**（UUID 只有"没重建文件系统"才可能一样）|
| **mkfs / wipefs 真的被跳过** | 第二次串口里**没有** `mkfs.ext4` 行，且安装器明说"跳过 wipefs/mkfs" | ✅ 通过 |
| **分区项沿用原位置** | 安装器明说"沿用原分区项（不重新规划）" | ✅ 通过 |
| 启动记录不得减少（附加判据）| 未取（需要读磁盘数 `boot.log`；UUID 判据已足够强）| ⏭ 未测，如实标注 |

## 四、这次实验暴露的两件事

1. **`keepvar-verify.py` 缺 `--already-raw`** ✗ —— 它只会从 **vmdk** 转换，而 VBox 产出的是 **VDI**
   （用 `clonehd --format RAW` 转成 raw 后它就吃不下）。本次**改用串口取证**绕开了，
   但这个缺口应该补上（下一轮做，别用 PowerShell 拼补丁 —— 上次拼坏了脚本，靠 `git checkout` 救回）。
2. **VBox 的串口这次是好用的**（32 KB 完整输出）—— 与那台构建机"串口恒 0 字节"不同。
   原因未查明，但**结论**是：串口可用时它是最省事的取证通道，不可用时才需要读磁盘。

## 五、与 VMware 的关系（诚实边界）

本次**没有**在 VMware 里验证。VMware 那侧未验的只剩：
- `vmware-keepvar.py` 真跑一次（改 vmx → 启动 → 等关机 → 改回 → 再启动）—— 目前只做了**干跑**（输出确认只改 3 行）；
- 换盘后从磁盘引导的系统是否能正常起来（VBox 侧同理，本次未做第三次启动）。

这两步都需要那台 VMware VM **关机** —— 与之前一样，卡在人手上。

## 四、第三轮实验：换盘后**从磁盘独立引导**（这一步推翻了上面的结论）

拔掉光驱、只从磁盘引导后，串口原文：

```
Command line: BOOT_IMAGE=/boot/bzImage console=tty0 console=ttyS0,115200n8 aether.keepvar=1
[aether-init] 未发现持久化分区（内存态运行，重启后状态不保留）
aether-ops: 巡检#1 up=15s mem=106/1977MB 服务=aetherd=✓ compositor=✓ getty=✓ network=✓ ops=✓
aether-ops: 巡检#5 up=75s mem=198/1977MB 服务=…（服务全 ✓，说明系统起来了，但跑在内存里）
```

注意第一行：**装好的系统仍带着 `aether.keepvar=1`**（引导配置是 keepvar ISO 的拷贝）。
它没有触发重装（安装标记判据挡住了 ✓），但 `/var` 也没挂上 ❌。

### 磁盘实况（把 VDI 转成 raw 直接读 MBR）

```
MBR 55AA: 55 aa ✓       磁盘签名: 109df173
分区项 0: 类型 83(0x83) 起始LBA 83968 扇区数 8304640
（项 1/2/3 全空）
```

**本该是**：项 0 = ISO 系统分区（类型 0x17）、项 1 = 持久分区（0x83）。
**实际是**：只有项 0，且是持久分区 ⇒ 系统分区项被顶掉了。

### 后果链

```
分区表少了系统分区项 ⇒ 内核只看到 /dev/sda1（= 持久分区）
                    ⇒ aether-init 的候选写死 /dev/sda2 ⇒ 认不出
                    ⇒ 内存态启动，/var 不挂载
```

**结论：`--keep-var` 的数据保全做对了，但"换完系统能正常用"没做到。T3 = FAIL。**
修法方向：`capture_persist()` 返回的索引要和 `set_partition_entry()` 的索引约定对齐（差一），
并补一个"两者必须一致"的单元测试；修完重跑本实验。
