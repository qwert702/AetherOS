# 装机与更新操作手册（2026-10-17）

> **给谁看**：要把 AetherOS 装进 VMware、或要把系统/应用更新到新版本的人。
> **原则**：能用一条命令就不用两条；每个破坏性动作都写明**怎么回滚**。
>
> 这台便携版 VMware 的 `vmrun` **不可用**（拉不起、也看不到 GUI 启动的 VM），
> 所以下面所有启动都用 `vmware.exe -x <vmx>`。

## 一、准备：四张 ISO 各管什么

| ISO（在 `E:\aether-vm\AetherOS\`）| 用途 |
|---|---|
| `aetheros-0.1.1.iso` | 正式发布版（当前 Release）|
| `aetheros-autoinstall.iso` | **首次装机**：挂上就自己装、装完自己关机 |
| `aetheros-keepvar.iso` | **换系统、保留 `/var`**（应用与数据都留着）|
| `aetheros-updater.iso` | 首次装机 + **含自动更新能力**（新部署建议用这张）|

## 二、首次装一台新系统

1. 新建 VM（2 vCPU / 2 GB / ≥4 GB 盘），光驱指向 `aetheros-updater.iso`；
2. 在 `.vmx` 里设 `bios.bootOrder = "cdrom"`、`sata0:1.startConnected = "TRUE"`；
3. `vmware.exe -x "E:\aether-vm\AetherOS\AetherOS.vmx"` 启动；
4. 它会**自动装盘**（约 1 分钟）→ 串口/屏幕出现 `AETHER-AUTOINSTALL: PASS` → **自己关机**；
5. 把 `.vmx` 改回：`bios.bootOrder = "disk"`、`sata0:1.startConnected = "FALSE"`（等于拔掉光驱）；
6. 再启动 → 进桌面，`/var` 已挂载（`持久化分区已挂载: /dev/sda2 -> /var`）。

> **硬判据**：从盘引导时**光驱必须是断开的**（第 5 步）。这样才是"独立系统"，不是 live。

## 三、换系统（保留 `/var`）—— **一条命令**

```powershell
# ① 先在 VMware 窗口里关机（虚拟机 → 电源 → 关闭客户机）
# ② 取基线（换之前跑一次，记下 UUID 与记录条数）
python scripts/keepvar-verify.py --report
# ③ 一条命令换系统（会：备份 vmx → 挂 keepvar ISO → 装 → 自己关机 → 改回磁盘引导 → 再启动 → 出证据）
python scripts/vmware-keepvar.py --apply --expect-uuid <①的UUID> --min-records <①的条数>
```

**成功判据**（工具会打印）：
- `/var` 的 **ext4 UUID 不变** ✓
- `boot.log` 里的**启动记录不减** ✓

**回滚**：工具开始前会备份 `AetherOS.vmx.bak-<时间戳>` ✓；装失败时 `finally` 会**保证 vmx 回到磁盘引导** ✓
（不会留下"从光驱引导"的死状态）。旧系统盘仍在（`disk.vmdk` 只有系统分区被重写，`/var` 未被碰 ✓）。

## 四、更新应用 / 配置（文件级自动更新）

### 宿主侧：打一个包

```powershell
# 首次：生成 Ed25519 签名密钥（私钥必须在仓库外，0600；公钥写入 aetherd/keys/）
python scripts/update-keygen.py --private-key "$HOME/.aether/update-signing-key.pem"

# 暂存树按"目标路径"摆：stage/var/apps/hello/hello ⇒ 目标 /var/apps/hello/hello
python scripts/mkupdate.py --version 0.1.2 --from stage --out updates `
  --signing-key "$HOME/.aether/update-signing-key.pem"
```

> 白名单**只有** `/var/apps/`、`/etc/aether/`、`/var/lib/aether/`；
> 系统组件（`/usr/bin/*`、内核、合成器）**不能**走这条路 —— 那用第三节的整机更新。
> 2026-10-24 起，生产包必须带 `update.json.sig`；`--allow-unsigned-dev` 只用于本地开发，
> guest 默认会拒绝无签名清单。

### 发布

```powershell
python scripts/publish-updates.py updates/0.1.2 --dry-run   # 先看要传什么
python scripts/publish-updates.py updates/0.1.2 --apply     # 真上传（要 AETHER_VPS_* 凭据）
```

### guest 侧：它自己会拉

`aetherd` 每 `check_minutes`（配置在 `/etc/aether/updater.json`，**只接受 https**）检查一次：

```
拉清单 + update.json.sig → **先验签（Ed25519）** → 比版本（拒降级）
→ 逐文件下载 → 校验哈希（不符整包拒绝）
→ 投 incoming/<版本>/ → stage() 暂存 → **下次启动生效**
```

### 回滚（应用级）

更新包落地后覆盖的是目标文件；回滚 = **再发一个把内容改回去的包**（版本号必须更大，因为拒降级）。
所以：**发版前先在宿主留一份上一版的包**，回滚时直接重新发布它（并把版本号 +1）。

## 五、自检与验收

```powershell
python scripts/gates.py            # 完整门禁（当前 41 项；含像素/i18n/性能/文档/契约…）
python scripts/gates.py --fast     # 秒级子集（27 项，日常改动用这个）
python scripts/update-contract-check.py   # 更新链路契约（发布布局 ↔ guest URL，真起 HTTP）
python scripts/keepvar-verify.py --selftest
```

## 六、故障排查（都是实测踩过的）

| 现象 | 原因 | 处理 |
|---|---|---|
| 串口 `serial.txt` 是 0 字节 | 这台 VMware 的串口落盘**不稳定** | 别靠串口；用 `keepvar-verify.py` 读磁盘出证据 |
| `vmrun start` 报 `Postpone the command. VM not running` | `vmrun` 与 GUI 启动的 VM 不互通 | 一律用 `vmware.exe -x <vmx>` |
| 改了 vmx 不生效 | **VM 运行时 VMware 会改写 vmx** | 先关机，再改 |
| 构建机（VirtualBox）停在 GRUB 菜单 | 硬断电后的典型表现 | `VBoxManage controlvm <vm> keyboardputscancode 1c 9c`（一个回车）|
| 构建机 SSH 不通但 VM 在跑 | 同上：客机根本没进 Linux | 先看 GRUB；`acpipowerbutton` 温柔关机后重启 |
| 在 ISO 里搜不到某个字符串 | 根文件系统是**压缩**的 | "搜不到" ≠ "不存在"：在打包前的二进制/overlay 上验 |
| 首启向导里点「中文/English」没反应 | 向导的鼠标点击路径在 `KeyRoute::Setup` 守卫之后，被 `continue` 吞掉（2026-10-24 已修，提交 `ba3b36f`）| 旧镜像上按 `1`（中文）/`2`（English）/`Esc` 绕过；装了新镜像后可直接点 |
| 换了 ISO 但系统没变（安装像没执行）| 光驱没真正挂上，VM 10 秒"无引导介质"退出；脚本又把 `storageattach` 的错误吞了 | 挂载命令**不要** `Out-Null`；判据只看串口是否出现 `自动安装开始（keep_var=true）` |
| 从盘启动后自动安装失败并关机（`打不开安装源 /dev/sr0` / `No medium found`）| 装好的系统引导配置仍带 `aether.keepvar=1`，且"空光驱仍报旧容量"骗过了 `live_medium()` | 临时：`sata0:1.present = "FALSE"` 摘掉光驱设备；根治：改用真正探测介质（见证据文档 SETUP-CLICK 第四节）|

## 七、当前状态（2026-10-17）

- 代码侧：**41 项完整门禁全过**（含本轮新增 5 项与更新链路相关的自检）· 779 测试 · 0 告警；
- **T3 已完成**（2026-10-17）：`--keep-var` 三轮实验全绿 —— 装机 → 换系统（/var 的 ext4 UUID 不变）→ 拔光驱从盘引导（`持久化分区已挂载: /dev/sda2 -> /var`，且不重复安装）。\n- **未完成**：更新链路的**真机端到端**（需生产 https 源）、
  更新源上线生产（需人确认）、真机验收 #32（需物理机）、M-13 GUI 粘贴（需 VM + GUI）。
