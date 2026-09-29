# AetherOS 应用包

这份文档面向**要给 AetherOS 做应用的人**。装应用的人看「怎么装」那两节就够了。

## 先明确一件事：这里的"应用"是什么

AetherOS 没有包管理器，也没有软件仓库。`/` 是只读的（ISO9660 + initramfs 启动时整个读进内存），
只有持久化分区 `/var`（ext4）可写、跨重启保留。

所以"装应用"= **把一个目录复制到 `/var/apps/<id>/`，再给它一个能在终端里敲的名字**。

## 包长什么样

一个目录，必须有 `app.json`，`entry` 指向目录里的可执行文件：

```
hello/
  app.json          清单（必须）
  bin/hello         可执行文件（entry 指向它）
  lib/              可选：自带的共享库，启动时会进 LD_LIBRARY_PATH
```

`app.json`：

```json
{
  "id": "hello",
  "name": "Hello",
  "version": "1.0",
  "entry": "bin/hello",
  "args": [],
  "desc": "示例应用"
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | 目录名，也是终端里的命令名。只允许字母数字与 `-` `_` `.`，且以字母数字开头，≤64 字符 |
| `name` | 是 | 显示名，可以带中文 |
| `entry` | 是 | **包内相对路径**，不许绝对路径、不许含 `..`，且必须是普通文件 |
| `version` | 否 | 版本字符串，随便写 |
| `args` | 否 | 每次启动都追加的固定参数 |
| `desc` | 否 | 一句话说明 |

包里**不能有符号链接**。符号链接能指向系统任意位置，装完之后那个链接就躺在应用目录里 ——
等于把逃逸面装进系统。有符号链接的包会被整体拒绝，并且不会留下半个目录。

## 怎么写一个能跑的程序

这是最容易踩的地方，先看实测结论（2026-09-28 在镜像的根文件系统里跑过）：

| 情况 | 结果 |
|---|---|
| 静态链接（`gcc -static`、Rust `musl` 静态、Go 默认） | **能跑** |
| 动态链接，但依赖的库都在镜像里 | **能跑** |
| 动态链接，缺依赖库 | **跑不起来** —— 报 `error while loading shared libraries` |

镜像里的 C 库是 **glibc**（`/lib/libc.so.6`），最高版本 `GLIBC_2.38`，**向后兼容** ——
所以"在你的机器上编的、要求更高版本 glibc"这件事通常不是问题（实测 Ubuntu 24.04 编的
hello 直接跑通）。真正会挂的是**缺共享库**：镜像里只有 glibc、libgcc、libblkid 等少数几个，
没有 `libselinux`、`libpcre2`、`libssl` 之类。

**所以最省事的路子就是静态链接。** 清单里没写"依赖"，因为现在没有依赖解析 ——
要么静态，要么把 `.so` 放进包的 `lib/`。

### 装的时候会预检

`app_install`（以及 `aetherd app install`）在**复制之前**会读一遍可执行文件：

- ELF 静态 → 「静态链接的可执行文件，不依赖任何共享库」
- ELF 动态，`DT_NEEDED` 里的库都能找到（系统目录或包内 `lib/`）→ 「需要的共享库都在」
- ELF 动态，缺库 → **会列出缺哪几个**，并告诉你两个办法：让作者静态链接，或把 `.so` 放进 `lib/`
- `#!` 脚本 → 检查解释器在不在
- 既不是 ELF 也不是脚本 → **直接拒绝安装**，并说明"可能是别的平台的程序"

缺库的情况只警告、不拒绝（万一是误判），但会在结果里写清楚 ——
**装完跑不起来比装不上更糟**，人只会觉得系统坏了，不会想到是缺一个 `.so`。

## 怎么装

### 命令行（推荐先这样验证）

```sh
# 包目录通常在 U 盘、家目录或 /tmp
aetherd app install /tmp/hello

# 列出已装的
aetherd app list

# 终端里立刻可用（或新开一个 shell；开机时 aetherd 会自动做这件事）
aetherd app link

# 卸载（进回收站，不是直接删）
aetherd app remove hello
```

### 让 AI 装

对 AI 指令条说「把 /tmp/hello 装上」这类话，它会用 `app_install` 工具 ——
**L2 操作，会弹确认卡片**，卡片上写着这个包要被复制到哪、装进来的程序以你的权限运行。
装完会把预检结论一起告诉你。

AI 侧的源目录受限：只能从**用户数据区**（`/home`、`/tmp`、`/var/apps`）读，
所以先把包放到家目录或 `/tmp`。命令行没有这个限制（人在终端里不受策略约束）。

## 装完在哪

- **终端**：开机时 aetherd 会把每个应用生成一个包装脚本放进 `/usr/local/bin`。
  为什么要包装脚本而不是软链 —— 应用可能自带 `lib/`，需要设 `LD_LIBRARY_PATH`，软链带不了环境变量。
  `/usr` 在内存盘里，重启还原，所以这件事**每次开机重做**。
- **Dock**：合成器会扫 `/var/apps`，把已装应用**排在五个内建图标之后**（安装向导之前），
  底座用青绿色区分"这是你自己装的"。点一下会开一个终端窗口把它跑起来。
  Dock 每 ~3 秒重扫一次，所以让 AI 装完之后**不用重启合成器**，图标会自己冒出来。
  - 为什么启动是"开终端窗口"而不是独立窗口：这系统上的应用现在都是终端程序 ——
    没有 Wayland 客户端支持，别的程序也没法往这块 framebuffer 上画。所以"启动应用"
    ＝"开个终端把它跑起来"，这也复用了已经审过的 PTY 通路，而不是新开一条
    "让合成器 fork 任意程序"的路径（那会是个新的逃逸面）。
  - 名字撞上内建图标或"安装"的应用**不会显示在 Dock 里**（那几个名字在 Dock 有保留含义），
    但终端里照常能用。
- **卸载后**：包装脚本会在下次 `app link` 时被清掉。清的时候只删带本工具标记的脚本，
  不会动你自己往 `/usr/local/bin` 里放的东西。Dock 上的图标也会在下次重扫时消失。

## 明确的边界

- **目标端不做依赖解析**（刻意的）：镜像里没有仓库、也没有 `ld.so.cache`，`aetherd` 只回答
  "这个包在当前机器上跑不跑得起来"（递归查缺库 + 查 terminfo + 看包内 `lib/`）。
  依赖收集发生在**打包时**（宿主/构建机上，那边才有完整的系统库），见下节。
- **没有沙箱**：装进来的程序以你的权限运行，能读写你的一切。预检只回答"能不能跑"，
  不回答"是不是安全"
- **没有版本/升级机制**：更新就是卸载（进回收站）再装
- **`.aep` 是未压缩 tar**，在 guest 里用 BusyBox 的 `tar xf` 解开再 `app install`。
  刻意不让目标端实现 tar/gzip 解析器：少一个解析器就少一片攻击面。

---

## 能装市面上的 Linux 软件吗（2026-09-29 实测）

**结论：静态链接与常见动态依赖的程序能装能跑；GUI 程序与解释型还没戏。**

### 改造前的实测事实

| 项 | 原来 |
|---|---|
| 包管理器 | 全无（apt/dpkg/rpm/apk/opkg/pacman） |
| 解释器 / 编译器 | 全无（python/perl/node/gcc/make） |
| 共享库 | 只有 glibc 家族 + libgcc_s + e2fsprogs 的库 |
| terminfo | **没有** —— 而 PTY 设的 `TERM` 是 `xterm-256color` |

三类二进制 chroot 进镜像实跑的结果：

| 类型 | 结果 |
|---|---|
| `gcc -static`（静态） | ✅ 能跑 |
| 动态、只依赖 glibc | ✅ 能跑（镜像 glibc 2.38 向后兼容） |
| 动态、依赖别的东西（libstdc++ / libnl…） | ❌ `error while loading shared libraries` |

### 因此做了三件事

1. **镜像补库**：`ncurses`（含 **terminfo** —— 全屏程序的硬需求）、`zlib`、`openssl`、
   `libffi`、`expat`。ISO 30.6 MB → **33.4 MB**。顺带按项目安全基线关掉了 OpenSSL 3.x 的
   legacy 算法与调试机制（逐项说明见 `platform/br2-external/configs/aetheros_defconfig`）。
2. **打包器 [`scripts/mkapp.py`](../scripts/mkapp.py)**：把宿主上的一个程序打成
   "自带缺的库 + terminfo" 的包。
3. **分发 [`scripts/serve-apps.py`](../scripts/serve-apps.py)**：宿主起只读 HTTP，guest 用
   BusyBox `wget` 拉包（QEMU 用户态网络里宿主就是 `10.0.2.2`）。

### 打包器的三条规则（每条都有实测教训）

- **glibc 家族永不打包**。实测：把宿主 `libc.so.6` 打进包、再让 `LD_LIBRARY_PATH` 指过去，
  加载器会用**外来 libc** → `undefined symbol: __tunable_is_initialized, version GLIBC_PRIVATE`。
- **只收镜像里没有的库，而且要递归**。`--image-lib-dir` 指向镜像 rootfs 逐个比对；
  libA 依赖 libB、libB 又依赖 libC —— 少收一层，装上去照样挂。
- **镜像里"有"不等于"能用"**：版本符号（ELF 的 `.gnu.version_r` / `.gnu.version_d`）要对得上。
  实测 htop 需要 `libncursesw.so.6` 的 `NCURSESW6_*`，而 Buildroot 编的 ncurses **没有版本符号表**
  → 每次运行都打印 `no version information available (required by htop)`。
  打包器会识别这种情况，改带宿主那份。

### 用法

```bash
# 宿主/构建机：打包（--image-lib-dir 是镜像的 rootfs 目录）
python3 scripts/mkapp.py /usr/bin/htop --id htop --name htop \
    --image-lib-dir /home/aether/platform/build/output/target \
    --out dist/apps --tar

# 只想问"这个程序在镜像上能跑吗"（不产出任何文件）
python3 scripts/mkapp.py /usr/bin/htop --check --image-lib-dir <镜像 rootfs>

# 宿主：把包服务出去
python3 scripts/serve-apps.py --dir dist/apps

# 解析器自测（不需要 Linux、不联网）
python3 scripts/mkapp.py --selftest
```

guest 里：

```sh
wget http://10.0.2.2:8765/htop.aep -O /var/tmp/htop.aep
mkdir -p /var/tmp/pkg && tar xf /var/tmp/htop.aep -C /var/tmp/pkg
aetherd app install /var/tmp/pkg/htop
htop
```

### 实测战果

**htop 3.3.0（Ubuntu 24.04 官方包）在 AetherOS 里跑起来了。** 打包器的判断：

```
跳过（glibc 家族，镜像自带，绝不打包）：libc.so.6, libm.so.6
镜像里的 libncursesw.so.6 版本符号对不上（缺 NCURSESW6_5.7.20081102, …）→ 改带宿主那份
需要随包携带（4 个）：libncursesw.so.6、libnl-3.so.200、libnl-genl-3.so.200、libtinfo.so.6
要求 glibc ≥ 2.38；镜像提供 2.38          → 结论：可以跑 ✅
```

包约 1 MB；装完终端里直接敲 `htop` 就能用（包装脚本会设好 `LD_LIBRARY_PATH` 与
`TERMINFO_DIRS`）。

### 还不能跑的

| 类型 | 为什么 |
|---|---|
| GUI 程序（GTK / Qt / X11） | 桌面走 `DRM → fbdev`，没有 X11/Wayland 客户端库（Phase 3 的事） |
| 解释型（`.py` / `.pl` / npm 包） | 镜像里没有解释器，也**不打算**塞（会让 ISO 涨到 100 MB+） |
| 发行版包（`.deb` / `.rpm`） | 没有包管理器；就算解开，依赖也不会自己出现 —— 用打包器代替 |
| 要求 glibc 比 2.38 更新的程序 | 打包器会直接拒（这条现在有自动化检查，不再靠人肉判断） |
