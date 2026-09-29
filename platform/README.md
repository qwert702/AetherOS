# platform/ — AetherOS 系统构建

本目录负责把内核、底层积木与自研用户态组装成可启动的 ISO。

## 一键构建

```bash
./platform/build-iso.sh          # 在 Linux/Ubuntu 构建机内运行（WSL2 或独立 VM 均可）
```

脚本三步（头注释为准）：musl 静态交叉编译自研组件 → 连同中文屏显字体拷进 rootfs overlay
→ 下载 Buildroot 2024.02.1、应用 `aetheros_defconfig` 并出 ISO。
产物落在仓库根目录 `aetheros-0.1-amd64.iso`（约 30 MB），中间产物在 `platform/build/`（不入版本控制）。

构建机准备见 `scripts/setup-vm.ps1`；在 VM 内也可以用 `scripts/vm-build-iso.sh` 包一层
（它负责把 `~/.cargo/bin` 补进 PATH，并把日志落到 `iso-build.log`）。

## 目录

| 路径 | 内容 |
|---|---|
| `build-iso.sh` | ISO 一键构建入口 |
| `br2-external/configs/aetheros_defconfig` | Buildroot 配置：x86_64、initramfs、ISO9660、引导、busybox、e2fsprogs |
| `br2-external/board/aether/linux.fragment` | 内核片断（PCNET32、fbdev/evdev、virtio 等） |
| `br2-external/Config.in` / `external.mk` / `external.desc` | Buildroot 外部树声明（AETHER） |
| `br2-external/grub-embedded.cfg` / `isolinux.cfg` | 两种引导方式的嵌入配置 |
| `overlay/init` | 内核首用户态：挂载伪文件系统 + 拉起 lo → `exec aether-init --pid1` |
| `overlay/etc/aether/services/*.json` | 服务定义 5 个：network / aetherd / compositor / ops / getty |
| `build/` | 构建中间产物（不入版本控制） |

> `overlay/usr/bin/` 与 `overlay/usr/share/fonts/` 是 `build-iso.sh` **构建期生成**的
> （自研二进制 + 文泉驿微米黑），已在 `.gitignore` 中排除，不要手工提交。
