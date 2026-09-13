# AetherOS 交接报告
**日期**：2026-09-11（v4 —— M4 完成，M5 v0.1 落地；交互桌面 + AI 指令端到端 + 巡检自修复）
**版本**：v0.1-preview
**项目根目录**：`D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\`
**分支**：`fix/m3-desktop-render`
**关键提交**：`d7ba5d1` M3 打通 · `5c264af` VBox 上屏 · `962bdf3` M4 完成 + M5 v0.1

---

## 零、快速接手（先看这里）

### 0.1 两台 VM

| VM | 用途 | 连接方式 |
|----|------|---------|
| `AetherOS-Build` | **构建机**（Ubuntu 24.04，含 Buildroot/QEMU/字体） | SSH `aether@127.0.0.1:2222`，密码 `aetheros` |
| `AetherOS-Demo` | **演示/验证机**（跑 ISO） | 无 SSH，用 `VBoxManage controlvm ... screenshotpng` 截图 |

> `scripts/vm.py` 只做远程命令（`sh`）；**文件上传统一走 `scripts/transfer.py`**。
> `scripts/qmp-verify.py` 可向 QEMU guest 注入键盘/鼠标事件并截图（M4 端到端验证全靠它）。

### 0.2 改一行代码 → 看到效果（最短路）

```bash
# 0) ⚠️ Git Bash 下凡是命令行参数带 /home/... 必须加 MSYS2_ARG_CONV_EXCL="*"，
#    否则被 MSYS 改写成 C:\Program Files\Git\home\...，SFTP 报 ENOENT（本次最大坑，见 5.5）
export MSYS2_ARG_CONV_EXCL="*"

# 1) 改本地代码（本地仓库是唯一真源），同步到构建机
python scripts/transfer.py aether-compositor/src/main.rs /home/aether/aether-compositor/src/main.rs

# 2) 构建机内重编（默认 aetherd+compositor+ops → cpio → initramfs 重嵌 → ISO，约 6 分钟）
python scripts/vm.py sh "cd /home/aether && nohup bash rebuild-m4.sh >rebuild-m4.log 2>&1 < /dev/null & sleep 2"
#    轮询：grep REBUILD-DONE /home/aether/rebuild-m4.log

# 3) 从构建机取回 ISO（⚠️ 只能 get，不能 put 构建产物；transfer.py 仅做 put）
python -c "import sys;sys.path.insert(0,'scripts');import vm;c=vm.client();c.open_sftp().get('/home/aether/aetheros-0.1-amd64.iso','aetheros-0.1-amd64.iso');c.close()"

# 4) 构建机内 QEMU 验证（启动约 2 分钟到桌面）
python scripts/vm.py sh "pkill -f '[q]emu-system-x86_64'; cd /home/aether && nohup bash qemu-verify.sh std >qverify.out 2>&1 < /dev/null & sleep 3"
python scripts/vm.py sh "grep -a aether-compositor /home/aether/qemu-serial.log | tail -5"
python scripts/vm.py sh "python3 /home/aether/qmp-verify.py shot" > docs/x.png          # 截图走 stdout
#    键盘注入：python3 /home/aether/qmp-verify.py key t h r e e ret
#    鼠标注入：python3 /home/aether/qmp-verify.py mouse 30 0 click
```

**务必确认的串口日志**：`自启动序列` 含 `compositor`、
`fbdev WxH @bpp`（modeset 成功）、`evdev 输入已接入（N 个设备）`、`已渲染 1 帧`、
`aether-ops: 巡检#1 …`（M5 巡检心跳）。

### 0.3 本次做成了什么

**M4 完成**：系统内桌面从"静态演示"变为**可交互**——evdev 键盘/鼠标接入、
AI 指令条真实连通 aetherd（离线意图）、Action 端到端落地（AI 说"排成三列"→ 窗口动画成三列）。

**M5 v0.1**：aether-ops 从占位变成**真实巡检自修复 agent**：15s 一轮，
/proc 真实指标 + aether-init:7312 服务状态，异常服务经 ServiceControl 自愈重启（20 轮冷却防风暴）。

| 环境 | 显卡控制器 | DRM 驱动 | 分辨率 | 截图 |
|------|-----------|---------|--------|------|
| QEMU | `-vga std` / `-vga virtio` | `bochs-drm` / `virtio-gpu` | 1280x800 | `docs/screenshot-desktop.png`、`docs/screenshot-m4-interactive.png` |
| VirtualBox | `--graphicscontroller vboxvga` | `vboxvideo` | 1024x768 | `docs/screenshot-vbox.png` |
| **VMware Workstation** | 默认 SVGA | `vmwgfx`（原生主场） | 1280x800 | `docs/screenshot-vmware.png` |

**VMware 已实测可用**（Workstation 17，绿色版在 `D:\CBN-HT\Desktop\asdf\`，
VM 在 `D:\aether-vm\AetherOS-VMware\`）：vmwgfx modeset 成功、桌面完整渲染、
evdev 收到 5 个输入设备（含 vmmouse）、宿主鼠标点击能操作 guest UI、
**网络已通**（内核 `CONFIG_PCNET32=y` → eth0 → NAT DHCP 拿到 IP，五服务全 ✓）。
VMX 要点：`bios.bootOrder = "cdrom"` + **SATA 光驱**（IDE 光驱引导不起来）+
`serial0` 落盘到 serial.txt（唯一的日志通路；重启时 VMware 会弹"替换/附加"对话框）。
启动：`vmrun start AetherOS.vmx`。

> ⚠️ VBox **不要用默认的 `vmsvga`**（走 `vmwgfx`，该驱动在 VBox 上拒绝工作）。
> ⚠️ QEMU 默认 `-m 512` 对桌面偏紧（initramfs 全在内存，巡检会报"内存紧张"是真实压力），演示建议 `-m 1024`。

---

## 一、本次两个根因级 bug（都已被端到端测试暴露，务必记住）

| # | 根因 | 现象 | 修复 |
|---|------|------|------|
| A | **`/init` 从不拉起 `lo` 回环**（内核默认 lo 是 down 的） | aetherd bind 127.0.0.1:7311 "成功"，但 compositor 的 connect 无限期阻塞——AI 查询永远停在"…思考中" | `/init` 加 `ip link set lo up`；compositor connect 加 3s 超时兜底 |
| B | **aetherd 把 `Action` 放在 `ChatChunk{done:true}` 之后发送** | 客户端收到 done 即停止读取，`layout_set` 等桌面行为**全部被静默丢弃**（回复正常显示但布局不变） | aetherd 语义改为 **Action 必须在 done 之前**（`server.rs handle_chat`） |

> 根因 A 的教训：bind 成功 ≠ 可达。内核启动后 lo 是 down 的，udhcpc 只管 eth0。
> 根因 B 的教训：M2 预览路径只测过"回复"，Action 顺序从未被端到端验证过。

---

## 二、里程碑完成情况总览

| 里程碑 | 状态 | 主要内容 | 验证结论 |
|--------|------|---------|---------|
| M0 开发与架构设计 | ✅ 完成 | Buildroot 环境、仓库骨架、rust-toolchain、ARCHITECTURE.md | cargo build/test 通过 |
| M1 合成器 MVP | ✅ 完成 | minifb 预览（Windows）、拖拽/缩放窗口、4 种布局、AI 指令条 | preview 渲染正常 |
| M2 Shell 雏形 | ✅ 完成 | 顶栏、启动器、通知区、任务栏、毛玻璃面板 | Windows 预览完整 |
| M3 系统地基 | ✅ 完成 | ISO 开机 → compositor 渲染桌面并上屏 | QEMU(1280x800) + VBox(1024x768) 双环境截图 |
| M4 AI 中枢 aetherd | ✅ **完成** | fbdev 路径：evdev 输入 + AI 指令条 + Action 全链路 | **QEMU 实测**：注入 "three"+Enter → 离线意图 → `layout_set three_col` → 三列动画落地；Dock 点击开窗；软件光标随鼠标移动 |
| M5 AI 运维 | ✅ **v1 完成** | 巡检自修复 agent（15s/轮、真实指标、ServiceControl 自愈、冷却防风暴）+ aetherd 状态接真实数据 + **日志监听预警**（aether-init logtee 落盘 /tmp/log/*.log + ops 增量扫描字面量模式告警） | **全部实测**：①巡检心跳真实内存/服务；②AI "status" 回复「内存 451/467MB，已运行 2 分钟；5/5 服务运行中」；③**自愈实测**：root 登录 tty1 `killall aether-compositor` → ops 一轮内发现 → `自修复 → 重启 compositor` → 新实例恢复渲染；④**告警实测**：AI 条输入乱串 → aetherd 记 `ERROR LLM 请求失败` → ops `📢 aetherd 日志异常` |
| M6 安装器 0.1 发布 | ✅ **v0.1 完成** | `aether-install` 组件（isohybrid dd 方案）+ aetherd `install_disk` 工具 + 重建流程 isohybrid 化 | **全链路实测**：ToolCall（hostfwd→7311）→ 安装器写盘（`55 aa` 签名）→ **无光驱纯磁盘引导** → 桌面/五服务/巡检全部正常 |

---

## 三、显示栈与输入栈（接手必读）

显示通路：**内核 DRM → fbdev 模拟 → `/dev/fb0` → compositor 软件光栅化写入**（M3 不变，详见 v3 报告存档/ git 历史）。

输入通路（M4 新增）：**QEMU PS/2(i8042) / VBox → 内核 evdev（`CONFIG_INPUT_EVDEV=y`，内核已内置无需重编）→ `/dev/input/event*` → `input.rs` 后台线程 → mpsc 通道 → 渲染主循环 drain**。

- `aether-compositor/src/input.rs`：evdev 解析（EV_KEY/EV_REL；EV_ABS 不支持）。
  键盘：字母/符号进 AI 指令条，Enter 发送，退格删除，**数字 1–4 保留为布局快捷键**；
  鼠标：REL 位移累积（2x 手感倍率，不受渲染帧率影响）、左键按下/抬起。
- `run_fbdev()`：预览路径的完整交互逻辑（菜单/下拉/Dock 命中、拖拽+吸附、缓动动画、toast/回复气泡）已整体移植，另加 `draw::draw_cursor` 软件光标（fbdev 无硬件指针）。
- AI 通路：Enter → `query_aether`（**connect 3s / read 15s 超时**，每步打串口日志）→ aetherd:7311 → 离线意图（"three"/"两列"/"tidy"/"status"等，无需云端）→ Action（done 之前）→ `apply_action`。
- 诊断：QMP 注入键鼠（`scripts/qmp-verify.py`）可以全自动端到端测试，不依赖人手。

---

## 四、构建与验证

### 4.1 构建（构建机内）

```bash
export MSYS2_ARG_CONV_EXCL="*"    # 仅 Git Bash 需要；见 5.5
python scripts/transfer.py <本地相对路径> <VM绝对路径>      # 上传（可多对）
python scripts/vm.py sh "<命令>" [超时秒]                   # 远程命令

# VM 内重建：bash /home/aether/rebuild-m4.sh（aetherd+compositor+ops+ISO，约 6 分钟）
# 只改 compositor：bash /home/aether/rebuild-comp.sh（旧脚本仍可用）
# 改 /init 或 *.json：rebuild-m4.sh 已覆盖（cpio → initramfs 重嵌 → ISO）
# 改内核配置：需强制重配+重编内核（15-25 分钟，见 git 历史里的 v3 报告 4.1）
```

**⚠️ ISO 只能 pull，不能 push**：把本地旧 ISO `put` 上去会覆盖新镜像。

### 4.2 验证

**QEMU（构建机内，推荐）**

```bash
python scripts/vm.py sh "cd /home/aether && nohup bash qemu-verify.sh std >qverify.out 2>&1 < /dev/null & sleep 3"
python scripts/vm.py sh "grep -a aether-ops /home/aether/qemu-serial.log | tail -5"
python scripts/vm.py sh "python3 /home/aether/qmp-verify.py shot" > docs/x.png
python scripts/vm.py sh "python3 /home/aether/qmp-verify.py key t h r e e; sleep 1; python3 /home/aether/qmp-verify.py key ret"
python scripts/vm.py sh "python3 /home/aether/qmp-verify.py mouse 264 364 click"
```

> `ppm2png.py` / `qemu-shot.py` / `vnc-shot.py` / `qmp-verify.py shot` 现在都把 PNG 写到 **stdout**，
> 用 `> out.png` 落盘，诊断信息走 stderr。

**VirtualBox（`AetherOS-Demo`）**：同 v3 报告（`--graphicscontroller vboxvga --vram 32`，
串口 `--uartmode1 file` 路径必须正斜杠）。

---

## 五、坑（本次新增 + 历史保留）

### 5.5 ⭐ 传输 ENOENT 的真凶：路径双拼 + SFTP 偶发抽风
1. **vm_path_of 双前缀（已修复的教训）**：防穿越重定根函数曾对已带 `/home/aether`
   前缀的绝对路径**再拼一层**，得到 `/home/aether/home/aether/...` —— SFTP 报
   `SSHFX_NO_SUCH_FILE`，100% 复现且极像玄学。该函数现在对已带根前缀的路径原样放行。
   **诊断这类问题的最快手段：用 stderr/报错把"服务器实际收到的路径"打出来**（本次是
   base64 兜底通道的 bash 报错暴露的），不要对着 paramiko 的 `FileNotFoundError` 猜。
2. **构建机 sftp-server 偶发 NO_SUCH_FILE**：真实存在的间歇抽风（同一代码时好时坏）。
   `transfer.py` 已带 base64-over-ssh 兜底通道（stdin 管道，不经命令行参数），主通道失败自动降级。
3. Git Bash 的 MSYS 参数改写是**理论风险**（实测本机未触发），`MSYS2_ARG_CONV_EXCL="*"` 作无害保险。
4. **重建脚本必须 `set -euo pipefail`**：`cargo build | tail -1` 的管道退出码是 tail 的，
   编译失败会被静默吞掉、然后 cp 拷到**旧二进制**混出新 ISO（本次真实踩坑：
   ops 少个常量编译失败，ISO 照样出炉、行为悄悄回退）。rebuild-m4.sh 已修。

### 5.6 ⭐ Mimosa 安全门禁与提交
本仓库 git commit 被 Mimosa hook 门禁拦截：**argv 派生路径 → 文件写原语（open 'wb'/putfo/rename）**
一律判 high（数据流规则，函数内校验/重定根不豁免；连服务端 rename 到变量路径也不行）。
已按其规则重构：截图工具全部 stdout 化、上传统一 `transfer.py`（putfo + `vm_path_of` 重定根）、
`vm.py` 只留 `sh`。**新增带文件写入的脚本时，优先 stdout / 固定字面量路径，否则 commit 会被拦。**
门禁提示过"library_source 不可用，覆盖不完整"——建议用户择机跑一次 `/mimosa-scan` 完整审计。

### 5.1 ✅ VBox：vmwgfx 不支持（历史保留）
默认控制器 `vmsvga` 走 VMware 驱动，在 VBox 上拒绝工作。改 `vboxvga`。
串口 `--uartmode1 file` 路径必须正斜杠，否则 `Power up failed`。

### 5.2 ✅ 中文字体（历史保留）
wqy-microhei 打进 rootfs；粗体必须同字体（DejaVu 无 CJK）。

### 5.3 性能参考
- VBox（硬件虚拟化）：约 0.25 s/帧（4 fps）
- QEMU TCG（嵌套，无 KVM）：约 4 s/帧——**截图/气泡类验证要留足时间窗**，或直接看串口日志（M4 起查询全程有日志）

### 5.4 CRLF 风险
Windows 写文件带 `\r\n`；`transfer.py`/`vm.py` 上传时自动归一为 `\n`。`/init` shebang 必须第一行。

---

## 六、未完成清单

### 6.1 M5 剩余（P2）
1. **故障诊断报告**：采集→根因→方案→一键执行的结构化输出（接 aetherd LLM 通道）
2. 内存紧张（512MB guest）只告警不行动——可加"回收字体缓存/降帧"等罐头动作
3. 告警模式表目前 5 个字面量（monitor.rs ALERT_PATTERNS），可按需扩充/加每模式冷却

> **如何在 QEMU 里做 guest 内实验**（自愈实测的方法，已验证）：QMP `sendkey` 的键会同时到达
> evdev（compositor AI 条）和 tty 层（tty1 的 getty）。节奏：`ret` → 等 2s → `r o o t` → `ret`
> → 等 3s → 命令（**空格的 qcode 是 `spc` 不是 `space`**，`-` 是 `minus`）→ `ret`。
> 登录成功的标志是串口出现 `login[121]: root login on 'tty1'`。compositor 已设
> `restart:false`，其崩溃由 ops 自愈（监督器不接管）——这是刻意设计，让 M5 自愈有真实职责。
> 触发告警的最快方法：AI 条输入乱串（如 `asdf`+回车）→ aetherd 记 `ERROR LLM 请求失败` → ops 📢。

> **如何在 QEMU 里做 guest 内实验**（自愈实测的方法，已验证）：QMP `sendkey` 的键会同时到达
> evdev（compositor AI 条）和 tty 层（tty1 的 getty）。节奏：`ret` → 等 2s → `r o o t` → `ret`
> → 等 3s → 命令（**空格的 qcode 是 `spc` 不是 `space`**，`-` 是 `minus`）→ `ret`。
> 登录成功的标志是串口出现 `login[121]: root login on 'tty1'`。compositor 已设
> `restart:false`，其崩溃由 ops 自愈（监督器不接管）——这是刻意设计，让 M5 自愈有真实职责。

### 6.2 M6 剩余（v0.2+）
1. **持久化 rootfs**：安装后系统仍是内存驻留（initramfs），需把磁盘扩为真实 root 分区 + overlay 写入，重启才有状态
2. **安装向导 UI**：目前是 CLI/ToolCall；做成桌面安装器应用（选盘、进度、确认卡片走权限 L2）
3. 安装器细节：进度百分比、安装后自动扩容
4. 品牌设计：Logo、开机动画、默认壁纸
5. smithay 真 Wayland 合成器（长期方向，当前 fbdev 软渲染是刻意选择）

### 6.3 安装器工作原理（M6 v0.1，接手必读）
- ISO 在重建流程末尾经 host `isohybrid` 处理（`55 aa` MBR 签名 + 隐藏 ISO 分区表），
  **dd 到块设备即可 BIOS 引导**——零额外引导器依赖（extlinux host 二进制是 glibc 动态链接，musl guest 跑不了）
- `aether-install --disk <块设备> --yes`：防呆（块设备/sysfs 容量≥64MB/显式 --yes）
  → `dd if=/dev/sr0 of=<盘> bs=4M` → sync。目标路径白名单校验在 aetherd 工具层再做一道
- **触发方式（实测推荐）**：QEMU `-netdev user,hostfwd=tcp:127.0.0.1:17311-:7311` →
  宿主直调 `ToolCall{tool:"install_disk", arguments:{disk:"/dev/vda"}}` → 同步等 ToolResult
  （构建机上的驱动：/home/aether/m6-toolcall.py）。guest 的 aetherd 需 `AETHER_BIND=0.0.0.0`
  （/init 已设，仅 NAT VM 调试用；真实部署删掉）
- 验证看两处：ToolResult 的 output + `od -j510 -N2 dist.raw` 应为 `55 aa`
- 块设备 `metadata().len()` 恒为 0，容量必须读 `/sys/block/<盘>/size`（扇区×512）
- 测试盘：qemu-verify.sh 自动建 /home/aether/dist.raw（virtio → guest /dev/vda）
- tty1 盲打的坑：手动 shell 程序的输出**不会**进串口（tty1 ≠ console）；
  观测要么重定向 `/dev/ttyS0`（qmp-verify `combo shift+dot` 打 '>'，`combo shift+s` 打 'S'），
  要么走上面的 ToolCall 路线（推荐，全程可观测）

---

## 七、项目结构（仅列本次变动 + 关键文件）

```
Aether/
├── aether-compositor/
│   ├── src/input.rs            # ★ 新增：evdev 键鼠后端（含单测）
│   ├── src/main.rs             # ★ run_fbdev 全交互化；query_aether 带日志与超时
│   ├── src/fbdev.rs            # ioctl 请求参数 musl/glibc 自适应（as _）
│   └── src/draw.rs             # + draw_cursor 软件光标
├── aetherd/src/server.rs       # ★ Action 必须在 done 之前（协议序修复）
├── aether-ops/
│   ├── src/monitor.rs          # ★ 新增：巡检决策纯函数 + /proc 解析（6 单测）
│   └── src/main.rs             # ★ 15s 巡检自修复循环
├── platform/overlay/init       # ★ ip link set lo up（根因 A 修复）
├── platform/overlay/etc/aether/services/ops.json   # restart:true
└── scripts/
    ├── qmp-verify.py           # ★ 新增：QMP 注入键鼠 + 截图（stdout）
    ├── rebuild-m4.sh           # ★ 新增：三组件 musl 重建 → ISO
    ├── transfer.py             # ★ 新增：SFTP 上传（putfo+重定根+重试）
    ├── vm.py                   # 只留 sh；put 指引到 transfer.py
    ├── qemu-verify.sh / qemu-shot.py / vnc-shot.py / ppm2png.py   # 截图 stdout 化
    └── docs/HANDOVER.md        # 本文件
```

---

## 八、关键文件速查

| 内容 | 路径 |
|------|------|
| 一键构建（全量） | `platform/build-iso.sh` |
| 增量重建（常用） | 构建机 `/home/aether/rebuild-m4.sh`（源：`scripts/rebuild-m4.sh`） |
| Buildroot 配置 | `platform/br2-external/configs/aetheros_defconfig` |
| 内核配置片段 | `platform/br2-external/board/aether/linux.fragment`（输入驱动已由 x86_64 默认配置覆盖，无需加） |
| 内核 cmdline | `platform/br2-external/isolinux.cfg` |
| 系统初始脚本 | `platform/overlay/init`（含 lo up） |
| 服务定义 | `platform/overlay/etc/aether/services/*.json` |
| Framebuffer 后端 | `aether-compositor/src/fbdev.rs` |
| **evdev 输入** | `aether-compositor/src/input.rs` |
| 合成器主入口 | `aether-compositor/src/main.rs`（run_fbdev = 系统路径） |
| PID 1 + 服务编排 | `aether-init/src/manager.rs`（监督退避）/ `ipc.rs`（7312 控制通道） |
| AI 中枢 | `aetherd/src/server.rs`（7311）/ `intent.rs`（离线意图表） |
| **巡检自修复** | `aether-ops/src/monitor.rs` |
| VM 命令/传输助手 | `scripts/vm.py` / `scripts/transfer.py` |
| 端到端验证 | `scripts/qmp-verify.py` + `scripts/qemu-verify.sh` |

---

*报告更新于：2026-09-11 UTC+8*
