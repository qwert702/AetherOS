# platform/ — AetherOS 系统构建

本目录负责把内核、底层积木与自研用户态组装成可启动的 ISO。

- `rootfs/` — root 文件系统组装配方（M3 起基于 Buildroot：内核 + glibc + Mesa + 固件 + 我们的组件）
- `iso/` — GRUB 引导配置、ISO 打包脚本
- `kernel/` — 内核配置（先沿用 Buildroot 默认，后期自定义裁剪）

一键构建入口：仓库根目录 `scripts/build-iso.sh`（M3 实现，需在 WSL2/Linux 中运行）。
