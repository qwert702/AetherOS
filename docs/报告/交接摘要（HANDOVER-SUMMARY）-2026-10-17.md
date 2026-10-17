# 交接摘要（2026-10-17）

> **给后来的人**：5 分钟看懂 AetherOS 现在到哪了、哪些是真做完的、哪些是**卡在人手上**的。
> 更细的证据在两份文档里：[VMware 无人装机](../证据/VMware无人装机（AUTOINSTALL）-2026-10-16.md)、
> [自动更新链路（P1）](../证据/自动更新链路（P1）-2026-10-17.md)；操作步骤见
> [装机与更新操作手册](装机与更新操作手册（OPS-MANUAL）-2026-10-17.md)。

## 一、现状一句话

**代码/工具/文档侧已做到判据全绿**（41 项完整门禁 · 779 测试 · 0 告警 · 文档与站点事实一致）；
**端到端与生产发布一步未动**，各自卡在一个无法由 agent 代替的动作上（见第四节）。

## 二、2026-10-15 ～ 10-17 这段交付了什么

| 主题 | 交付 | 验证 |
|---|---|---|
| **P0 无人装机** | `aether-init/src/autoinstall.rs` + 引导菜单项 + `AETHER_AUTOINSTALL` 构建开关 | VMware 里真装成：`AETHER-AUTOINSTALL: PASS` → 自动关机 → **拔光驱独立引导进桌面**（人眼确认）|
| **P2 换系统保留 /var** | `aether-install --keep-var`（dd 前记住分区项+磁盘签名，dd 后还原）+ `aether.keepvar=1` 无人路径 + keepvar 版 ISO | 单测 + 负向对照；**端到端待验**（需 VM 关机）|
| **P1 自动更新** | `mkupdate.py`（打包）· `publish-updates.py`（发布）· `aetherd/src/updater.rs`（guest 拉取+校验+投递）· `update-contract-check.py`（**契约真 HTTP 实测**）| 各段自检/单测全过；**端到端待验**（需 VM 关机）|
| **运维工具** | `vmware-keepvar.py`（**一条命令换系统**）· `keepvar-verify.py`（读磁盘出 T3 证据）· `follow-gate-numbers.py`（照判据回填数字）| 全部接入 `gates.py`，自检含负向对照 |
| **文档** | 2 份证据文档 + 1 份操作手册 + 方案文档状态如实更新 | 链接判据 93 条 0 断 |

## 三、质量证据（可复跑）

```powershell
python scripts/gates.py            # 41 项完整门禁 → 当前全过
python scripts/gates.py --fast     # 27 项，日常改动用这个
```

当前：**41/41 门禁** · **779 测试** · host(`--all-targets`) 与 musl 六个 crate **0 告警** ·
文档数字/站点事实/链接一致 · `site/release.json` v0.1.1（ISO 41.0 MB · sha256 `eea68da9…`）。

## 四、未做 / 待验（**读这一节最省时间**）

| 事项 | 卡在哪 | 怎么解（都已就绪）|
|---|---|---|
| **T3 端到端**：换系统后 `/var` 的 ext4 UUID 不变、启动记录不减 | **需要把那台 VMware VM 关机**（agent 无法代替）| 关机后一条命令：`python scripts/vmware-keepvar.py --apply --expect-uuid <基线> --min-records <条数>` |
| 更新链路真机端到端（guest 拉到 → 重启生效）| 同上 | 假源可在宿主起，不依赖生产 VPS；注：`aetherd` 的 updater **强制 https**，本地测试要按 `update-contract-check.py` 的方式验布局 |
| 更新源上线生产 | **需要人确认**（只新增 `/var/www/aether.cbnac.com/updates/`，`rm -rf` 即回滚）| `python scripts/publish-updates.py updates/<ver> --apply` |
| 真机验收 #32（45 项）| **需要物理机** | 手册见 `docs/报告/实机验收手册（RUNBOOK）-2026-10-11.md` |
| M-13 GUI 粘贴端到端 | 需要 VM 关机 + GUI 操作 | — |

## 五、环境事实（这次踩出来的，别再踩）

完整 8 条见 [自动更新链路（P1）](../证据/自动更新链路（P1）-2026-10-17.md) 第三节，最要紧的三条：

1. **这台便携版 VMware 的 `vmrun` 用不了** ⇒ 启动一律 `vmware.exe -x <vmx>`；guest 内一切必须引导期自动；
2. **那台 VMware VM 的串口不可靠**（`serial.txt` 常 0 字节）⇒ 判据用 `keepvar-verify.py` 直接读磁盘；
3. **构建机（VirtualBox）硬断电后会停在 GRUB 菜单** ⇒ `keyboardputscancode 1c 9c`（一个回车）即可救回；
   它串口恒 0 字节，别指望。

## 六、给后来人的三条提醒

1. **"判据全绿" ≠ "做完了"** —— 这次 41 门禁全绿，但 T3 与更新链路端到端**一次都没在真机上跑过**。
   本摘要第四节就是为此存在的，别把它当已完成跳过。
2. **改文档数字不要手抄、不要宽正则** —— 本次一天之内因为宽正则改坏文档三次（合计行带逗号导致判据
   "整行匹配不到"、把测试表列改错、把历史段落改坏）。用 `python scripts/follow-gate-numbers.py`：
   它照判据打印的"声明 X / 实测 Y"精确回填。**并且：检查工具退出非零 ⇒ 一律不提交。**
3. **"默认安全动作"也要接受显式开关名** —— 干跑默认是对的，但 `--dry-run` 必须能被显式写出来，
   否则文档里的示例会 `unrecognized arguments`（这个错这次犯了两次）。
