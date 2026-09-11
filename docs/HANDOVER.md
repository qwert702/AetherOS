# AetherOS 交接报告
**日期**：2026-09-11（v2，含 M3 桌面联调结论）
**版本**：v0.1-preview
**项目根目录**：`D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\`

---

## 零、本次更新摘要（2026-09-11）

**里程碑**：M3 打通 —— **AetherOS 桌面已在虚拟机中真实渲染上屏**（首个可用桌面）。

| 环境 | 显卡控制器 | DRM 驱动 | 分辨率 | 状态 |
|------|-----------|---------|--------|------|
| QEMU | `-vga std` / `-vga virtio` | `bochs-drm` / `virtio-gpu` | 1280x800 | ✅ 截图 `docs/screenshot-desktop.png` |
| VirtualBox | `--graphicscontroller vboxvga` | `vboxvideo` | 1024x768 | ✅ 截图 `docs/screenshot-vbox.png` |

> ⚠️ VBox **不要用默认的 `vmsvga`**：它走 `vmwgfx`，而该驱动在 VBox 上明确拒绝工作
> （见 5.1）。

**原报告把 M3 阻塞归结为"build-iso.sh 漏了 compositor"，实际是三层叠加问题**，只修一层都不出桌面：

| # | 根因 | 现象 | 修复 |
|---|------|------|------|
| 1 | `compositor.json` 的 `autostart: false` | compositor 从不启动 | 改为默认 `true`（`platform/overlay/etc/aether/services/compositor.json`） |
| 2 | 引导菜单未把 `vga=` 传给内核 | 无 `/dev/fb0` | `BR2_TARGET_ROOTFS_ISO9660_BOOT_MENU` 指向自定义 `isolinux.cfg`（Buildroot 默认用内置 boot menu，只给 `root=/dev/sr0`） |
| 3 | **内核 DRM fbdev 只创建 fb0、从不点亮 CRTC** | 屏幕停留在引导文本模式 | compositor 打开 fb0 后调用 `FBIOPUT_VSCREENINFO`，触发 `drm_fb_helper_set_par` 执行真正的 modeset |

根因 1 是历史脚本 `scripts/vm-m3-final.sh` 留下的（当时注释为"compositor 等 smithay 后端就绪再自启"），
一直没改回来，**导致此前所有"compositor 黑屏"的排查都在追幻影**。

**核心教训（务必记住）**：
- **构建用的是 Build VM 上 `/home/aether/platform/overlay/` 的副本**，不是本地仓库。
  两边一旦分叉，本地改得再对也不生效。本次已把本地仓库确立为唯一真源并同步。
- `BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILE` 是**错的变量名**，Buildroot 实际用**复数**
  `BR2_LINUX_KERNEL_CONFIG_FRAGMENT_FILES`。写错时 fragment **静默不生效**，
  内核会退回默认配置——此前所有 fragment 改动其实都没进内核。

---

## 一、里程碑完成情况总览

| 里程碑 | 状态 | 主要内容 | 验证结论 |
|--------|------|---------|---------|
| M0 开发与架构设计 | ✅ 完成 | WSL/Buildroot 环境、仓库骨架、rust-toolchain、ARCHITECTURE.md | cargo test/build 通过 |
| M1 合成器 MVP | ✅ 完成 | minifb 预览（Windows）、拖拽/缩放窗口、4 种布局、AI 指令条 | preview.bmp 正常 |
| M2 Shell 雏形 | ✅ 完成 | 顶栏、启动器、通知区、任务栏、毛玻璃面板 | Windows 预览完整 |
| M3 系统地基 | ✅ **完成** | ISO 开机 → **compositor 渲染桌面并上屏** | QEMU(1280x800) + VBox(1024x768) 双环境截图，见 `docs/screenshot-*.png` |
| M4 AI 中枢 aetherd | 🟡 部分完成 | IPC 已通、tool calling 框架就绪 | compositor 的 Action 执行层已接入代码，未联调 |
| M5 AI 运维 | ❌ 未开始 | ops 骨架已建，自修复未实现 | - |
| M6 安装器 0.1 发布 | ❌ 未开始 | - | - |

---

## 二、显示栈（M3 的关键，接手必读）

AetherOS 的显示通路是：**内核 DRM → fbdev 模拟 → /dev/fb0 → compositor 软件光栅化写入**。

### 2.1 内核配置（`platform/br2-external/board/aether/linux.fragment`）

```
CONFIG_DRM=y
CONFIG_DRM_FBDEV_EMULATION=y      # 关键：由 DRM 提供 /dev/fb0
CONFIG_DRM_VBOXVIDEO=y            # VBox vboxvga（✅ VBox 用这个）
CONFIG_DRM_VMWGFX=y               # VBox vmsvga（❌ 该驱动在 VBox 上拒绝工作，见 5.1）
CONFIG_DRM_VIRTIO_GPU=y           # QEMU virtio-gpu
CONFIG_DRM_BOCHS=y                # QEMU -vga std
CONFIG_FB=y
# CONFIG_FB_VESA is not set       # 不用 VESA，避免与 DRM 争抢
# CONFIG_FRAMEBUFFER_CONSOLE is not set
```

### 2.2 必须由 compositor 主动 modeset（根因 3）

内核的 DRM fbdev 模拟**只创建 `fb0` 设备节点，不会点亮 CRTC**。实测 debugfs 状态：

```
crtc[35]: crtc-0     enable=0   active=0   mode: ""      ← 未启用
connector[31]: ...   crtc=(null)                         ← 未绑定
plane[33]: plane-0   crtc=(null)  fb=0                   ← 无 framebuffer
```

此时屏幕停留在 ISOLINUX 的文本模式，compositor 画得再对也不显示。
修复：`aether-compositor/src/fbdev.rs` 的 `open()` 里调用一次

```rust
var.activate = FB_ACTIVATE_NOW | FB_ACTIVATE_FORCE;
libc::ioctl(fd, FBIOPUT_VSCREENINFO, &mut var);   // → drm_fb_helper_set_par → 真正 modeset
```

调用后 `crtc.enable=1 / active=1 / plane.fb=37`，画面立刻上屏。

### 2.3 不要用 mmap，用 write()

`fbdev.rs` 的 blit 走 **`write()` 系统调用**（`fb_sys_write` 路径）。
实测 **mmap 写入会在首帧后把 guest 卡死**（QEMU 下 CPU 100% 空转），
而 `write()` 稳定。参考实现见 `aether-compositor/src/fbdev.rs`。

### 2.4 诊断手段（环境相关的坑）

- **VBox**：串口是唯一日志通路，但 `--uartmode1 file` 的路径**必须用正斜杠**，
  否则 VM `Power up failed`（详见 5.1.1）。另外 VBox 下 VGA 文本控制台在 DRM 接管后
  就不再刷新，**截图会停在引导文本，这不代表 guest 卡住**——务必以串口日志为准。
- **QEMU**：工具见 `scripts/qemu-verify.sh`、`scripts/qemu-shot.py`、
  `scripts/vnc-shot.py`、`scripts/ppm2png.py`（串口落盘 + QMP screendump + VNC 抓屏，全部纯标准库）。
- compositor 内置 `tty_log()`：把关键诊断写到 `/dev/tty0`（VGA 文本控制台），
  在完全没有串口的场合可从截图看到。
- 内核 cmdline 已含 `console=tty0`，因此**内核启动日志也会出现在屏幕上**，便于定位早期问题。

---

## 三、当前项目结构

```
Aether/
├── Cargo.toml                   # workspace
├── aether-init/                 # PID 1，服务编排（拓扑排序 + 监督重启）
├── aether-compositor/           # 合成器
│   ├── src/main.rs              # Windows 预览路径 + Linux fbdev 路径（cfg 门控）
│   ├── src/fbdev.rs             # /dev/fb0 后端：ioctl 读参数 + FBIOPUT 触发 modeset + write() 写帧
│   └── src/draw.rs              # 软件光栅化渲染器
├── aetherd/                     # AI 中枢守护（IPC 127.0.0.1:7311 + tool 调用）
├── aether-ops/                  # 运维骨架（占位，restart:false）
├── aether-ipc/                  # IPC 协议
├── aether-shell/                # UI 组件库
├── platform/
│   ├── br2-external/
│   │   ├── configs/aetheros_defconfig
│   │   ├── board/aether/linux.fragment
│   │   └── isolinux.cfg         # APPEND console=tty0 console=ttyS0,115200n8（无 vga=，DRM 自己 modeset）
│   ├── overlay/
│   │   ├── init                 # exec aether-init --pid1
│   │   ├── etc/aether/services/*.json
│   │   └── usr/bin/             # 四个 musl 静态二进制（构建时拷入）
│   └── build-iso.sh             # 一键构建（COMPONENTS 含 aether-compositor + 拷入 CJK 字体）
├── scripts/                     # VM 辅助 + QEMU 验证工具
├── docs/
│   ├── screenshot-desktop.png   # ✅ QEMU 1280x800 验证截图
│   ├── screenshot-vbox.png      # ✅ VBox 1024x768 验证截图
│   └── HANDOVER.md              # 本文件
└── aetheros-0.1-amd64.iso       # 当前产物（29.6MB，含 CJK 字体）
```

---

## 四、构建与验证流程

### 4.1 构建（在 Build VM 内，`AetherOS-Build`，aether@127.0.0.1:2222）

```bash
# 本地 → VM 同步（重要：本地是唯一真源）
python scripts/vm.py put <本地相对路径> <VM绝对路径>

# VM 内：musl 编译四个组件 → cpio → 内核重嵌入 initramfs → iso9660
bash /home/aether/rebuild-comp.sh      # 增量（只改了 compositor）
# 或改内核配置后全量：
#   make ... aetheros_defconfig && rm -f .stamp_configured/.stamp_built/.config && make ...
```

**注意**：`/init` 与 `*.json` 在 initramfs 里，**改了必须重链内核**（`linux-rebuild-with-initramfs`）；
只改 `isolinux.cfg`（引导层）则只需重打 `rootfs-iso9660`。

**⚠️ ISO 只能 pull，不能 push**：`/home/aether/aetheros-0.1-amd64.iso` 是构建产物，
若把本地旧 ISO `put` 到 VM，会覆盖新构建的镜像，导致"改了却看不到效果"的假象
（本次调试就踩过：本地旧的 26.8MB ISO 覆盖了 VM 上新出的 29.6MB）。
本地要更新 ISO，用 `get` 从 VM 取回。

### 4.2 验证

**QEMU（构建 VM 内已装 qemu-system-x86，推荐日常用）**

```bash
bash scripts/qemu-verify.sh std                 # 启动（VNC + 串口落盘 + QMP）
python3 scripts/qemu-shot.py /home/aether/x.png # QMP screendump → PNG
grep aether-compositor /home/aether/qemu-serial.log   # 看 compositor 日志
```

**VirtualBox（桌面 VM `AetherOS-Demo`）**

```bash
VBoxManage modifyvm "AetherOS-Demo" --graphicscontroller vboxvga --vram 32   # 不要用 vmsvga
VBoxManage modifyvm "AetherOS-Demo" --uart1 0x3f8 4
VBoxManage modifyvm "AetherOS-Demo" --uartmode1 file "D:/aether-vm/serial.txt"  # 正斜杠！
VBoxManage startvm "AetherOS-Demo" --type headless
VBoxManage controlvm "AetherOS-Demo" screenshotpng out.png
```

**务必确认这三条日志**（QEMU 看串口文件，VBox 看 `D:/aether-vm/serial.txt`）：
`自启动序列` 含 `compositor`、`aether-compositor: fbdev WxH @bpp ... put=0`（modeset 成功）、`已渲染 N 帧`。

---

## 五、已知问题与待办

### 5.1 ✅ 已解决：VBox 下桌面未上屏
**根因**：VBox 默认控制器 `vmsvga` 由 `vmwgfx` 驱动，而 **vmwgfx 是 VMware 的驱动，
在 VirtualBox 上明确拒绝工作**：

```
vmwgfx 0000:00:02.0: [drm] *ERROR* vmwgfx seems to be running on an unsupported hypervisor.
vmwgfx 0000:00:02.0: [drm] *ERROR* This configuration is likely broken.
```

驱动仍会创建 fb0，但显示通路是坏的，屏幕停在引导文本。

**修复**：VBox 侧改用 `vboxvga` 控制器（走 `vboxvideo` 驱动）：

```bash
VBoxManage modifyvm <vm> --graphicscontroller vboxvga --vram 32
```

`vboxvideo` 干净初始化，compositor 正常 modeset（1024x768，`put=0`）并出桌面。

**注意**：两种环境用不同驱动，内核 fragment 里**两者都已启用**，无需切换内核：
QEMU 用 `bochs` / `virtio-gpu`，VBox 用 `vboxvideo`。

### 5.1.1 ⚠️ VBox 串口的正确用法（否则拿不到任何日志）
VBox 7.2 下 `--uartmode1 file` 的**路径必须用正斜杠**，反斜杠会让 VM `Power up failed`
且不报明显错误：

```bash
# 正确
VBoxManage modifyvm <vm> --uart1 0x3f8 4
VBoxManage modifyvm <vm> --uartmode1 file "D:/path/to/serial.txt"
# 错误（反斜杠）→ Failed to open host device ... Power up failed
#   --uartmode1 file "D:\path\to\serial.txt"
```

这是本次调试最大的坑：因为拿不到 VBox 日志，一度误判为"guest 卡死"，
实际 guest 一切正常（compositor 稳定渲染 100+ 帧），只是 VGA 文本控制台不再刷新。
**排查 VBox 问题前，先把串口配通。**

### 5.2 ✅ 已解决：桌面文字（中文字体）
`aether-compositor/src/text.rs` 的字体路径原本是 **Windows 专有**（`C:/Windows/Fonts/msyh.ttf`），
Linux 上 `TextRenderer::load()` 返回 `None` → 桌面结构正常但没有文字。

**已修复**：打包文泉驿微米黑（`fonts-wqy-microhei`，5.1MB）进 rootfs，
`platform/build-iso.sh` 会自动从宿主 `/usr/share/fonts/truetype/wqy/` 拷入 overlay。
`REGULAR_FONTS` / `BOLD_FONTS` 已把 Linux 路径放在最前（Windows 路径保留作预览回退）。

**注意**：粗体**必须用同一 CJK 字体**（wqy-microhei 无独立粗体）。
若把粗体回退到 `DejaVuSans-Bold`，中文标题会整体缺字——DejaVu 没有 CJK 字形。

**依赖**：构建机需 `sudo apt-get install fonts-wqy-microhei`，否则 build-iso.sh 会告警且桌面无字。

### 5.3 🟢 每帧 ~4 秒（仅 QEMU TCG 下）
QEMU 无 KVM（嵌套虚拟化）时软件光栅化 1280x800 很慢。真实硬件/VBox 下不适用。

### 5.4 🟢 CRLF 风险
Windows 写入的脚本/配置会带 `\r\n`，Linux 工具链敏感。
同步后统一 `sed -i 's/\r$//'`（`scripts/vm.py put` 已做 `\r\n`→`\n` 归一）。

---

## 六、未完成清单

### M4 收尾（P1）
1. **Action 执行层联调**：compositor 收到 aetherd 的 Action 后执行桌面行为（代码已就绪，未端到端验证）

### M5 / M6（排期后续）
3. AI 运维：日志监控 agent、故障诊断、自修复执行器
4. 简易安装器
5. 品牌设计：Logo、开机动画、默认壁纸

---

## 七、关键文件速查

| 内容 | 路径 |
|------|------|
| 一键构建 | `platform/build-iso.sh` |
| Buildroot 配置 | `platform/br2-external/configs/aetheros_defconfig` |
| 内核配置片段 | `platform/br2-external/board/aether/linux.fragment` |
| isolinux 菜单（含内核 cmdline） | `platform/br2-external/isolinux.cfg` |
| 系统初始脚本 | `platform/overlay/init` |
| 服务定义 | `platform/overlay/etc/aether/services/*.json` |
| **Framebuffer 后端（modeset 关键）** | `aether-compositor/src/fbdev.rs` |
| 合成器主入口 | `aether-compositor/src/main.rs` |
| PID 1 + 服务编排 | `aether-init/src/unit.rs` |
| AI 中枢 IPC | `aetherd/src/lib.rs` |
| VM 同步助手 | `scripts/vm.py` |
| QEMU 验证工具 | `scripts/qemu-verify.sh` / `qemu-shot.py` / `vnc-shot.py` |

---

*报告更新于：2026-09-11 UTC+8*
