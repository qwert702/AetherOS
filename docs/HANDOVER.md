# AetherOS 交接报告
**日期**：2026-09-11（v3 —— M3 完成，QEMU + VirtualBox 双环境验证通过）
**版本**：v0.1-preview
**项目根目录**：`D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\`
**分支**：`fix/m3-desktop-render`（含本次全部改动）
**关键提交**：`d7ba5d1` M3 打通 · `5c264af` VBox 上屏 + 打通 VBox 日志通路

---

## 零、快速接手（先看这里）

### 0.1 两台 VM

| VM | 用途 | 连接方式 |
|----|------|---------|
| `AetherOS-Build` | **构建机**（Ubuntu 24.04，含 Buildroot/QEMU/字体） | SSH `aether@127.0.0.1:2222`，密码 `aetheros` |
| `AetherOS-Demo` | **演示/验证机**（跑 ISO） | 无 SSH，用 `VBoxManage controlvm ... screenshotpng` 截图 |

> 构建机密码是本地一次性开发 VM 的，写在 `scripts/vm.py` 里。`scripts/vm.py` 是本项目
> 的 SSH/SFTP 助手（paramiko），用法：`python scripts/vm.py sh "<命令>"` / `put <本地相对路径> <VM绝对路径>`。

### 0.2 改一行代码 → 看到效果（最短路）

```bash
# 1) 改本地代码，同步到构建机（本地仓库是唯一真源）
python scripts/vm.py put aether-compositor/src/fbdev.rs /home/aether/aether-compositor/src/fbdev.rs

# 2) 构建机内：重编 compositor → cpio → 内核重嵌 initramfs → ISO（约 5 分钟）
python scripts/vm.py sh "cd /home/aether && setsid bash -c 'bash rebuild-comp.sh' </dev/null >rc.log 2>&1 &"

# 3) 从构建机取回 ISO（⚠️ 只能 get，不能 put，见 4.1）
python -c "import sys;sys.path.insert(0,'scripts');import vm;c=vm.client();s=c.open_sftp();s.get('/home/aether/aetheros-0.1-amd64.iso','aetheros-0.1-amd64.iso')"

# 4) 在构建机内用 QEMU 验证（约 2.5 分钟）
python scripts/vm.py sh "bash /home/aether/qemu-verify.sh std"
python scripts/vm.py sh "timeout 60 python3 /home/aether/qmp-shot.py /home/aether/x.png"
# 取回 x.png 查看
```

**务必确认这三条串口日志**：`自启动序列` 含 `compositor`、
`aether-compositor: fbdev WxH @bpp ... put=0`（modeset 成功）、`已渲染 N 帧`。

### 0.3 本次做成了什么

**M3 完成**：AetherOS 桌面在虚拟机中真实渲染上屏，中英文文字正常。

| 环境 | 显卡控制器 | DRM 驱动 | 分辨率 | 截图 |
|------|-----------|---------|--------|------|
| QEMU | `-vga std` / `-vga virtio` | `bochs-drm` / `virtio-gpu` | 1280x800 | `docs/screenshot-desktop.png` |
| VirtualBox | `--graphicscontroller vboxvga` | `vboxvideo` | 1024x768 | `docs/screenshot-vbox.png` |

> ⚠️ VBox **不要用默认的 `vmsvga`**（走 `vmwgfx`，该驱动在 VBox 上拒绝工作，见 5.1）。

---

## 一、原报告结论的修正

原报告把 M3 阻塞归结为"`build-iso.sh` 漏了 compositor"。**实际是三层根因叠加，只修一层都不出桌面**：

| # | 根因 | 现象 | 修复 |
|---|------|------|------|
| 1 | `compositor.json` 的 `autostart: false` | **compositor 从不启动** | 恢复默认 `true` |
| 2 | 引导菜单未把 `vga=` 传给内核 | 无 `/dev/fb0` | `BR2_TARGET_ROOTFS_ISO9660_BOOT_MENU` 指向自定义 `isolinux.cfg`（Buildroot 默认用内置 boot menu，只给 `root=/dev/sr0`） |
| 3 | **内核 DRM fbdev 只创建 fb0、从不点亮 CRTC** | 屏幕停在引导文本模式 | compositor 打开 fb0 后调 `FBIOPUT_VSCREENINFO` 触发真正 modeset（见 3.2） |
| 4 | VBox 默认控制器走 `vmwgfx` | VBox 下显示通路坏 | 改 `graphicscontroller=vboxvga`（见 5.1） |

根因 1 是历史脚本 `scripts/vm-m3-final.sh` 留下的（当时注释"compositor 等 smithay 后端就绪再自启"），
一直没改回来，**导致此前所有"compositor 黑屏"的排查都在追幻影**。

### 两个"静默失效"的坑（务必记住）

- **`BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILE` 是错的变量名**。Buildroot 实际用**复数**
  `BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILES`。写错时 fragment **静默不生效**，
  内核退回默认配置——此前所有 fragment 改动其实都没进内核。
- **构建用的是 Build VM 上 `/home/aether/platform/overlay/` 的副本**，不是本地仓库。
  两边一旦分叉，本地改得再对也不生效。本次已把本地仓库确立为唯一真源。

---

## 二、里程碑完成情况总览

| 里程碑 | 状态 | 主要内容 | 验证结论 |
|--------|------|---------|---------|
| M0 开发与架构设计 | ✅ 完成 | Buildroot 环境、仓库骨架、rust-toolchain、ARCHITECTURE.md | cargo build/test 通过 |
| M1 合成器 MVP | ✅ 完成 | minifb 预览（Windows）、拖拽/缩放窗口、4 种布局、AI 指令条 | preview 渲染正常 |
| M2 Shell 雏形 | ✅ 完成 | 顶栏、启动器、通知区、任务栏、毛玻璃面板 | Windows 预览完整 |
| M3 系统地基 | ✅ **完成** | ISO 开机 → compositor 渲染桌面并上屏 | QEMU(1280x800) + VBox(1024x768) 双环境截图 |
| M4 AI 中枢 aetherd | 🟡 部分完成 | aetherd IPC 已通、tool calling 框架就绪 | **系统路径尚未接通**，见 6.1 |
| M5 AI 运维 | ❌ 未开始 | ops 骨架已建，自修复未实现 | - |
| M6 安装器 0.1 发布 | ❌ 未开始 | - | - |

---

## 三、显示栈（M3 的核心，接手必读）

显示通路：**内核 DRM → fbdev 模拟 → `/dev/fb0` → compositor 软件光栅化写入**。

### 3.1 内核配置（`platform/br2-external/board/aether/linux.fragment`）

```
CONFIG_DRM=y
CONFIG_DRM_FBDEV_EMULATION=y      # 关键：由 DRM 提供 /dev/fb0
CONFIG_DRM_VBOXVIDEO=y            # VBox vboxvga（✅ VBox 用这个）
CONFIG_DRM_VMWGFX=y               # VBox vmsvga（❌ 在 VBox 上拒绝工作，见 5.1）
CONFIG_DRM_VIRTIO_GPU=y           # QEMU virtio-gpu
CONFIG_DRM_BOCHS=y                # QEMU -vga std
CONFIG_FB=y
# CONFIG_FB_VESA is not set       # 不用 VESA，避免与 DRM 争抢
# CONFIG_FRAMEBUFFER_CONSOLE is not set
```

三种虚拟显卡驱动同时编入，QEMU 与 VBox 各自匹配，**无需为不同环境切内核**。

### 3.2 必须由 compositor 主动 modeset（根因 3）

内核的 DRM fbdev 模拟**只创建 `fb0` 设备节点，不会点亮 CRTC**。实测 debugfs 状态：

```
crtc[35]: crtc-0     enable=0   active=0   mode: ""      ← 未启用
connector[31]: ...   crtc=(null)                         ← 未绑定
plane[33]: plane-0   crtc=(null)  fb=0                   ← 无 framebuffer
```

此时屏幕停在 ISOLINUX 文本模式，compositor 画得再对也不显示。
修复在 `aether-compositor/src/fbdev.rs` 的 `open()`：

```rust
var.activate = FB_ACTIVATE_NOW | FB_ACTIVATE_FORCE;
libc::ioctl(fd, FBIOPUT_VSCREENINFO, &mut var);   // → drm_fb_helper_set_par → 真正 modeset
```

调用后 `crtc.enable=1 / active=1 / plane.fb=37`，画面立刻上屏。

### 3.3 不要用 mmap，用 write()

`fbdev.rs` 的 blit 走 **`write()` 系统调用**（`fb_sys_write` 路径）。
实测 **mmap 写入会在首帧后把 guest 卡死**（CPU 100% 空转），`write()` 稳定。

### 3.4 诊断手段（环境相关的坑）

- **VBox**：串口是唯一日志通路，`--uartmode1 file` 的路径**必须用正斜杠**，
  否则 VM `Power up failed`（详见 5.1.1）。另外 VBox 下 VGA 文本控制台在 DRM 接管后
  不再刷新，**截图停在引导文本 ≠ guest 卡住**，务必以串口日志为准。
- **QEMU**：`scripts/qemu-verify.sh`（VNC + 串口落盘 + QMP）、`scripts/qemu-shot.py`
  （QMP screendump→PNG）、`scripts/vnc-shot.py`、`scripts/ppm2png.py`，全部纯标准库。
- 内核 cmdline 已含 `console=tty0`，**内核启动日志会出现在屏幕上**，便于定位早期问题。
- compositor 内置 `tty_log()`：把关键诊断写到 `/dev/tty0`，完全无串口时也能从截图看到。

---

## 四、构建与验证

### 4.1 构建（构建机内）

```bash
python scripts/vm.py put <本地相对路径> <VM绝对路径>     # 本地 → VM

# VM 内（增量：只改了 compositor）
bash /home/aether/rebuild-comp.sh
# VM 内（改了内核配置：需强制重配 + 重编内核，约 15-25 分钟）
#   make ... aetheros_defconfig
#   rm -f output/build/linux-*/.stamp_configured .stamp_built .config output/images/bzImage
#   make ... -j2
```

- `/init` 与 `*.json` 在 **initramfs** 里 → 改了**必须重链内核**（`linux-rebuild-with-initramfs`）
- 只改 `isolinux.cfg`（引导层）→ 只需重打 `rootfs-iso9660`
- **一劳永逸**：`platform/build-iso.sh` 是完整构建（含 musl 编译四组件 + 字体拷入）

**⚠️ ISO 只能 pull，不能 push**：`/home/aether/aetheros-0.1-amd64.iso` 是构建产物，
把本地旧 ISO `put` 上去会覆盖新镜像，造成"改了却看不到效果"的假象
（本次调试踩过：本地旧的 26.8MB ISO 覆盖了 VM 上新出的 29.6MB）。

### 4.2 验证

**QEMU（构建机内，推荐日常用）**

```bash
bash scripts/qemu-verify.sh std                      # 启动
python3 scripts/qemu-shot.py /home/aether/x.png      # 抓屏
grep aether-compositor /home/aether/qemu-serial.log  # 日志
```

**VirtualBox（`AetherOS-Demo`）**

```bash
VBoxManage modifyvm "AetherOS-Demo" --graphicscontroller vboxvga --vram 32   # 不要用 vmsvga
VBoxManage modifyvm "AetherOS-Demo" --uart1 0x3f8 4
VBoxManage modifyvm "AetherOS-Demo" --uartmode1 file "D:/aether-vm/serial.txt"   # 正斜杠！
VBoxManage startvm "AetherOS-Demo" --type headless
VBoxManage controlvm "AetherOS-Demo" screenshotpng out.png
```

---

## 五、已解决的坑（复现/排查参考）

### 5.1 ✅ VBox 下桌面未上屏 —— vmwgfx 不支持 VBox
VBox 默认控制器 `vmsvga` 由 `vmwgfx` 驱动，而它是 **VMware 的驱动，在 VirtualBox 上明确拒绝工作**：

```
vmwgfx 0000:00:02.0: [drm] *ERROR* vmwgfx seems to be running on an unsupported hypervisor.
vmwgfx 0000:00:02.0: [drm] *ERROR* This configuration is likely broken.
```

驱动仍会创建 fb0，但显示通路是坏的。**改用 `vboxvga`（`vboxvideo`）即通过**：

```bash
VBoxManage modifyvm <vm> --graphicscontroller vboxvga --vram 32
```

### 5.1.1 ⚠️ VBox 串口的正确用法（否则拿不到任何日志）
VBox 7.2 下 `--uartmode1 file` 的**路径必须用正斜杠**，反斜杠会让 VM `Power up failed`
且不报明显错误：

```bash
# 正确
VBoxManage modifyvm <vm> --uart1 0x3f8 4
VBoxManage modifyvm <vm> --uartmode1 file "D:/path/to/serial.txt"
# 错误（反斜杠）→ Failed to open host device ... Power up failed
```

**这是本次调试最大的坑**：因为拿不到 VBox 日志，一度误判为"guest 卡死"，
实际 guest 一切正常（compositor 稳定渲染 100+ 帧），只是 VGA 文本控制台不再刷新。
**排查 VBox 问题前，先把串口配通。**

### 5.2 ✅ 桌面文字（中文字体）
`aether-compositor/src/text.rs` 的字体路径原本是 **Windows 专有**（`C:/Windows/Fonts/msyh.ttf`），
Linux 上 `TextRenderer::load()` 返回 `None` → 桌面结构正常但没有文字。

**已修复**：打包文泉驿微米黑（`fonts-wqy-microhei`，5.1MB）进 rootfs；
`build-iso.sh` 自动从宿主 `/usr/share/fonts/truetype/wqy/` 拷入 overlay；
`REGULAR_FONTS` / `BOLD_FONTS` 已把 Linux 路径放在最前（Windows 路径保留作预览回退）。

**粗体必须用同一 CJK 字体**（wqy-microhei 无独立粗体）。若回退到 `DejaVuSans-Bold`，
中文标题会整体缺字——DejaVu 没有 CJK 字形。

**依赖**：构建机需 `sudo apt-get install fonts-wqy-microhei`，否则 `build-iso.sh` 告警且桌面无字。

### 5.3 性能参考
- **VBox（有硬件虚拟化）**：约 0.25 s/帧（4 fps）
- **QEMU TCG（构建机内嵌套虚拟化，无 KVM）**：约 4 s/帧 —— 只是验证环境慢，非程序问题

### 5.4 CRLF 风险
Windows 写入的脚本/配置会带 `\r\n`，Linux 工具链敏感。
`scripts/vm.py put` 已做 `\r\n`→`\n` 归一；手工上传后仍建议 `sed -i 's/\r$//'`。

---

## 六、未完成清单

### 6.1 M4 收尾（P1）—— 当前最大的缺口

**系统（fbdev）路径目前是一个「静态演示桌面」**：`run_fbdev()` 用 `ui_stub()` 渲染，
**没有任何输入**（无键盘/鼠标 evdev），也**不连接 aetherd**——
`query_aether()` / `apply_action()` 目前**只在 Windows minifb 预览路径里被调用**。

要做的事：
1. **输入接入**：`/dev/input/event*`（evdev）读取键盘/鼠标，替代 minifb 的事件源
2. **AI 指令条接通**：把预览路径的 `query_aether` + `AiEvent` 循环移植到 `run_fbdev`
3. **Action 执行联调**：aetherd 返回 `layout_set` / `open_app` / `close_active` →
   `apply_action` 落地（代码已有，需端到端验证）

> 参考：`aether-compositor/src/main.rs` 里 `preview_main()`（预览路径）与 `run_fbdev()`（系统路径）
> 共用 `draw` / `layout` / `text`，把前者的输入与 AI 部分搬过去即可。

### 6.2 M5 / M6（排期后续）
1. AI 运维：日志监控 agent、故障诊断、自修复执行器
2. 简易安装器
3. 品牌设计：Logo、开机动画、默认壁纸

---

## 七、项目结构

```
Aether/
├── Cargo.toml                   # workspace
├── aether-init/                 # PID 1，服务编排（拓扑排序 + 监督重启）
├── aether-compositor/           # 合成器
│   ├── src/main.rs              # preview_main()（Windows 预览）/ run_fbdev()（系统路径）
│   ├── src/fbdev.rs             # ★ /dev/fb0：ioctl 读参 + FBIOPUT 触发 modeset + write() 写帧
│   ├── src/text.rs              # ★ 字体加载（Linux CJK 优先）
│   └── src/draw.rs              # 软件光栅化渲染器
├── aetherd/                     # AI 中枢守护（IPC 127.0.0.1:7311 + tool 调用）
├── aether-ops/                  # 运维骨架（占位，restart:false）
├── aether-ipc/                  # IPC 协议
├── aether-shell/                # UI 组件库
├── platform/
│   ├── br2-external/
│   │   ├── configs/aetheros_defconfig
│   │   ├── board/aether/linux.fragment
│   │   └── isolinux.cfg         # APPEND console=tty0 console=ttyS0,115200n8
│   ├── overlay/
│   │   ├── init                 # exec aether-init --pid1
│   │   ├── etc/aether/services/*.json
│   │   └── usr/bin/             # 四个 musl 静态二进制（构建时拷入）
│   └── build-iso.sh             # 一键构建
├── scripts/                     # VM 辅助 + QEMU 验证工具
├── docs/
│   ├── screenshot-desktop.png   # ✅ QEMU 1280x800
│   ├── screenshot-vbox.png      # ✅ VBox 1024x768
│   └── HANDOVER.md              # 本文件
└── aetheros-0.1-amd64.iso       # 当前产物（29.6MB，含 CJK 字体）
```

---

## 八、关键文件速查

| 内容 | 路径 |
|------|------|
| 一键构建 | `platform/build-iso.sh` |
| Buildroot 配置 | `platform/br2-external/configs/aetheros_defconfig` |
| 内核配置片段 | `platform/br2-external/board/aether/linux.fragment` |
| 内核 cmdline | `platform/br2-external/isolinux.cfg` |
| 系统初始脚本 | `platform/overlay/init` |
| 服务定义 | `platform/overlay/etc/aether/services/*.json` |
| **Framebuffer 后端（modeset 关键）** | `aether-compositor/src/fbdev.rs` |
| **字体加载（CJK）** | `aether-compositor/src/text.rs` |
| 合成器主入口 | `aether-compositor/src/main.rs` |
| PID 1 + 服务编排 | `aether-init/src/unit.rs` / `aether-init/src/manager.rs` |
| AI 中枢 IPC | `aetherd/src/lib.rs` / `aetherd/src/server.rs` |
| VM SSH/SFTP 助手 | `scripts/vm.py` |
| QEMU 验证工具 | `scripts/qemu-verify.sh` / `qemu-shot.py` / `vnc-shot.py` |

---

*报告更新于：2026-09-11 UTC+8*
